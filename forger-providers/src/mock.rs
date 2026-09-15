use async_trait::async_trait;
use forger_core::{
    FinishReason, Message, Plugin, PluginId, Provider, ProviderError, StreamEvent, ToolSpec,
};
use futures::stream::{self, BoxStream};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// One scripted model response, used by tests and by `--provider mock`.
#[derive(Clone, Debug)]
pub enum MockScript {
    Text(String),
    Events(Vec<Result<StreamEvent, ProviderError>>),
}

impl MockScript {
    fn into_events(self) -> Vec<Result<StreamEvent, ProviderError>> {
        match self {
            MockScript::Text(text) => vec![
                Ok(StreamEvent::TextDelta { text }),
                Ok(StreamEvent::Finished {
                    reason: FinishReason::Stop,
                }),
            ],
            MockScript::Events(ev) => ev,
        }
    }
}

pub struct MockProvider {
    scripts: Mutex<Vec<MockScript>>,
    default_text: String,
}

impl MockProvider {
    pub fn new(scripts: Vec<MockScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts),
            default_text: "mock: no scripted turn left; hello from MockProvider".into(),
        }
    }

    pub fn single_text(text: impl Into<String>) -> Self {
        Self::new(vec![MockScript::Text(text.into())])
    }

    /// Stream that ends without a finish event — used to test truncation.
    pub fn truncated(text: impl Into<String>) -> Self {
        Self::new(vec![MockScript::Events(vec![Ok(StreamEvent::TextDelta {
            text: text.into(),
        })])])
    }

    /// One tool call, then a final text reply. Used by evals.
    pub fn tool_then_text(
        call_id: impl Into<String>,
        tool: impl Into<String>,
        arguments: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(vec![
            MockScript::Events(vec![
                Ok(StreamEvent::ToolCallDelta {
                    index: 0,
                    id: Some(call_id.into()),
                    name: Some(tool.into()),
                    arguments: Some(arguments.into()),
                }),
                Ok(StreamEvent::Finished {
                    reason: FinishReason::ToolCalls,
                }),
            ]),
            MockScript::Text(text.into()),
        ])
    }
}

impl Plugin for MockProvider {
    fn id(&self) -> PluginId {
        PluginId("mock")
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn model(&self) -> &str {
        "mock"
    }

    async fn stream(
        &self,
        _messages: &[Message],
        _tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let events = {
            let mut g = self.scripts.lock().unwrap();
            if g.is_empty() {
                MockScript::Text(self.default_text.clone()).into_events()
            } else {
                g.remove(0).into_events()
            }
        };
        Ok(Box::pin(stream::iter(events)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    #[tokio::test]
    async fn mock_emits_text_and_finish() {
        let p = MockProvider::single_text("hi");
        let mut s = p.stream(&[], &[], CancellationToken::new()).await.unwrap();
        let mut got = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev.unwrap());
        }
        assert_eq!(got.len(), 2);
        assert!(matches!(got[0], StreamEvent::TextDelta { .. }));
        assert!(matches!(
            got[1],
            StreamEvent::Finished {
                reason: FinishReason::Stop
            }
        ));
    }
}
