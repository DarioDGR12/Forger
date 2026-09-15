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

/// One `Arc<dyn Provider + Send + Sync>` shared by N spawned loops. Sequential
/// `run_turn().await` would keep max_inflight at 1 and stretch wall time to
/// ~2× delay; real `tokio::spawn` overlaps the sleeps.
struct SlowShared {
    delay: std::time::Duration,
    inflight: std::sync::atomic::AtomicUsize,
    max_inflight: std::sync::atomic::AtomicUsize,
}

impl SlowShared {
    fn new(delay: std::time::Duration) -> Arc<Self> {
        Arc::new(Self {
            delay,
            inflight: std::sync::atomic::AtomicUsize::new(0),
            max_inflight: std::sync::atomic::AtomicUsize::new(0),
        })
    }
}

impl Plugin for SlowShared {
    fn id(&self) -> PluginId {
        PluginId("slow-shared")
    }
}

#[async_trait]
impl Provider for SlowShared {
    fn model(&self) -> &str {
        "slow-shared"
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
        _cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        use std::sync::atomic::Ordering;
        let now = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_inflight.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        self.inflight.fetch_sub(1, Ordering::SeqCst);
        Ok(Box::pin(stream::iter(text("from shared provider"))))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quality_candidates_overlap_on_shared_provider() {
    use std::time::{Duration, Instant};

    let delay = Duration::from_millis(150);
    let shared = SlowShared::new(delay);
    let reviewer = ScriptedProvider::new(vec![
        text("{\"score\": 5, \"rationale\": \"ok\"}"),
        text("{\"score\": 6, \"rationale\": \"ok\"}"),
    ]);
    let agent = AgentLoop::new(
        Arc::clone(&shared) as forger_core::SharedProvider,
        ToolRegistry::new(),
        Arc::new(AutoApprover {
            allow_sensitive: true,
            allow_denylist: false,
        }),
        AgentLoopConfig::default(),
    );

    let started = Instant::now();
    let (_session, report) = forger_core::run_quality_turn(
        agent,
        reviewer,
        "do the thing",
        2,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let elapsed = started.elapsed();

    let max = shared
        .max_inflight
        .load(std::sync::atomic::Ordering::SeqCst);
    assert!(
        max >= 2,
        "expected overlapping candidate provider calls, max_inflight={max}"
    );
    assert!(
        elapsed < delay + Duration::from_millis(120),
        "quality turn took {elapsed:?}; sequential candidates would be ~{:?}",
        delay * 2
    );
    assert_eq!(report.candidates.len(), 2);
    assert_eq!(report.winner_index, 1);
}
