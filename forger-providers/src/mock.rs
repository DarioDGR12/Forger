use async_trait::async_trait;
use forger_core::{
    FinishReason, Message, Plugin, PluginId, Provider, ProviderError, Role, StreamEvent, ToolSpec,
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
    /// When set, ignore scripts and drive a tiny explore loop (list_dir then summarize).
    explorer: bool,
}

impl MockProvider {
    pub fn new(scripts: Vec<MockScript>) -> Self {
        Self {
            scripts: Mutex::new(scripts),
            default_text: "mock: no scripted turn left; hello from MockProvider".into(),
            explorer: false,
        }
    }

    pub fn single_text(text: impl Into<String>) -> Self {
        Self::new(vec![MockScript::Text(text.into())])
    }

    /// Local demo backend: first step calls `list_dir`, then replies with the listing.
    pub fn explorer() -> Self {
        Self {
            scripts: Mutex::new(Vec::new()),
            default_text: String::new(),
            explorer: true,
        }
    }

    /// Stream that ends without a finish event — used to test truncation.
    pub fn truncated(text: impl Into<String>) -> Self {
        Self::new(vec![MockScript::Events(vec![Ok(StreamEvent::TextDelta {
            text: text.into(),
        })])])
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
        messages: &[Message],
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let events = if self.explorer {
            explorer_events(messages, tools)
        } else {
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

fn explorer_events(
    messages: &[Message],
    tools: &[ToolSpec],
) -> Vec<Result<StreamEvent, ProviderError>> {
    let can_list = tools.iter().any(|t| t.name == "list_dir");
    let last_tool = messages
        .iter()
        .rev()
        .find(|m| m.role == Role::Tool)
        .map(|m| m.content.as_str());
    if can_list && last_tool.is_none() {
        return vec![
            Ok(StreamEvent::ToolCallDelta {
                index: 0,
                id: Some("mock-list".into()),
                name: Some("list_dir".into()),
                arguments: Some(r#"{"path":"."}"#.into()),
            }),
            Ok(StreamEvent::Finished {
                reason: FinishReason::ToolCalls,
            }),
        ];
    }
    let body = match last_tool {
        Some(listing) => format!(
            "Mock explorer (set FORGER_API_KEY for a real model).\nWorkspace listing:\n{listing}"
        ),
        None => "MockProvider: set FORGER_API_KEY to use an OpenAI-compatible backend.".into(),
    };
    MockScript::Text(body).into_events()
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

    #[tokio::test]
    async fn explorer_requests_list_dir_then_summarizes() {
        let p = MockProvider::explorer();
        let tools = [ToolSpec {
            name: "list_dir".into(),
            description: "list".into(),
            parameters: serde_json::json!({}),
        }];
        let mut s = p
            .stream(&[], &tools, CancellationToken::new())
            .await
            .unwrap();
        let mut got = Vec::new();
        while let Some(ev) = s.next().await {
            got.push(ev.unwrap());
        }
        assert!(matches!(
            got[0],
            StreamEvent::ToolCallDelta {
                name: Some(ref n),
                ..
            } if n == "list_dir"
        ));

        let msgs = vec![Message::tool_result(
            "mock-list",
            "list_dir",
            "file src/main.rs",
        )];
        let mut s = p
            .stream(&msgs, &tools, CancellationToken::new())
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(ev) = s.next().await {
            if let StreamEvent::TextDelta { text: t } = ev.unwrap() {
                text.push_str(&t);
            }
        }
        assert!(text.contains("src/main.rs"));
    }
}
