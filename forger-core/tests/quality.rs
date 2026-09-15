use async_trait::async_trait;
use forger_core::approval::AutoApprover;
use forger_core::{
    AgentLoop, AgentLoopConfig, CancellationToken, FinishReason, Message, Plugin, PluginId,
    Provider, ProviderError, QualityConfig, QualityRunner, StreamEvent, ToolRegistry, ToolSpec,
};
use futures::stream::{self, BoxStream};
use std::sync::{Arc, Mutex};

struct ScriptedProvider {
    scripts: Mutex<Vec<Vec<Result<StreamEvent, ProviderError>>>>,
}

impl ScriptedProvider {
    fn new(scripts: Vec<Vec<Result<StreamEvent, ProviderError>>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts),
        })
    }
}

impl Plugin for ScriptedProvider {
    fn id(&self) -> PluginId {
        PluginId("scripted")
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn model(&self) -> &str {
        "scripted"
    }
    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        let next = {
            let mut g = self.scripts.lock().unwrap();
            if g.is_empty() {
                vec![
                    Ok(StreamEvent::TextDelta {
                        text: "{\"score\":1,\"rationale\":\"default\"}".into(),
                    }),
                    Ok(StreamEvent::Finished {
                        reason: FinishReason::Stop,
                    }),
                ]
            } else {
                g.remove(0)
            }
        };
        Ok(Box::pin(stream::iter(next)))
    }
}

fn text(s: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    vec![
        Ok(StreamEvent::TextDelta { text: s.into() }),
        Ok(StreamEvent::Finished {
            reason: FinishReason::Stop,
        }),
    ]
}

#[tokio::test]
async fn quality_picks_higher_score() {
    // Each candidate consumes one scripted assistant reply.
    // The reviewer then consumes two score scripts: 4 then 9 → winner index 1.
    let weak = ScriptedProvider::new(vec![text("weak answer")]);
    let strong = ScriptedProvider::new(vec![text("strong answer")]);
    let reviewer = ScriptedProvider::new(vec![
        text("{\"score\": 4, \"rationale\": \"weak\"}"),
        text("{\"score\": 9, \"rationale\": \"strong\"}"),
    ]);

    let make = {
        let providers = [weak, strong];
        let n = std::sync::atomic::AtomicUsize::new(0);
        move || {
            let i = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AgentLoop::new(
                providers[i].clone(),
                ToolRegistry::new(),
                Arc::new(AutoApprover {
                    allow_sensitive: true,
                    allow_denylist: false,
                }),
                AgentLoopConfig::default(),
            )
        }
    };

    let runner = QualityRunner::new(reviewer, QualityConfig { candidates: 2 });
    let (session, report) = runner
        .run(make, "do the thing", CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(report.winner_index, 1);
    assert_eq!(report.candidates[0].score, 4);
    assert_eq!(report.candidates[1].score, 9);
    assert_eq!(session.last_assistant_text(), Some("strong answer"));
}
