use async_trait::async_trait;
use bytes::Bytes;
use forger_core::{
    FinishReason, Message, Plugin, PluginId, Provider, ProviderError, Role, StreamEvent, ToolSpec,
};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ToolChoice {
    #[default]
    Auto,
    Required,
    None,
}

#[derive(Clone, Debug)]
pub struct OpenAiCompatConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub tool_choice: ToolChoice,
}

impl OpenAiCompatConfig {
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("FORGER_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .ok()?;
        let base_url = std::env::var("FORGER_BASE_URL")
            .unwrap_or_else(|_| "https://api.deepseek.com/v1".into());
        let model = std::env::var("FORGER_MODEL").unwrap_or_else(|_| "deepseek-chat".into());
        Some(Self {
            base_url,
            api_key,
            model,
            tool_choice: ToolChoice::Auto,
        })
    }

    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.deepseek.com/v1".into(),
            api_key: api_key.into(),
            model: "deepseek-chat".into(),
            tool_choice: ToolChoice::Auto,
        }
    }

    pub fn with_tool_choice(mut self, tool_choice: ToolChoice) -> Self {
        self.tool_choice = tool_choice;
        self
    }
}

pub struct OpenAiCompatProvider {
    config: OpenAiCompatConfig,
    client: reqwest::Client,
}

impl OpenAiCompatProvider {
    pub fn new(config: OpenAiCompatConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
        }
    }
}

impl Plugin for OpenAiCompatProvider {
    fn id(&self) -> PluginId {
        PluginId("openai-compat")
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn model(&self) -> &str {
        &self.config.model
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
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let mut body = json!({
            "model": self.config.model,
            "messages": messages_to_openai(messages),
            "stream": true,
        });
        if !tools.is_empty() {
            body["tools"] = tools_to_openai(tools);
            body["tool_choice"] = match self.config.tool_choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Required => json!("required"),
                ToolChoice::None => json!("none"),
            };
        }

        let request = self
            .client
            .post(&url)
            .bearer_auth(&self.config.api_key)
            .header("content-type", "application/json")
            .json(&body);

        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
            resp = request.send() => {
                resp.map_err(|e| ProviderError::Transport(e.to_string()))?
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Transport(format!("HTTP {status}: {text}")));
        }

        let byte_stream = response
            .bytes_stream()
            .map(|r| r.map_err(|e| ProviderError::Transport(e.to_string())));
        Ok(sse_to_events(byte_stream, cancel))
    }
}

fn messages_to_openai(messages: &[Message]) -> Value {
    json!(messages
        .iter()
        .map(|m| {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            let mut obj = json!({
                "role": role,
                "content": m.content,
            });
            if let Some(ref calls) = m.tool_calls {
                obj["tool_calls"] = json!(calls
                    .iter()
                    .map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": c.arguments,
                        }
                    }))
                    .collect::<Vec<_>>());
            }
            if let Some(ref id) = m.tool_call_id {
                obj["tool_call_id"] = json!(id);
            }
            if let Some(ref name) = m.name {
                obj["name"] = json!(name);
            }
            obj
        })
        .collect::<Vec<_>>())
}

fn tools_to_openai(tools: &[ToolSpec]) -> Value {
    json!(tools
        .iter()
        .map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters,
            }
        }))
        .collect::<Vec<_>>())
}

fn sse_to_events<S>(
    byte_stream: S,
    cancel: CancellationToken,
) -> BoxStream<'static, Result<StreamEvent, ProviderError>>
where
    S: futures::Stream<Item = Result<Bytes, ProviderError>> + Send + 'static,
{
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        futures::pin_mut!(byte_stream);
        let mut buf = String::new();
        let mut saw_finish = false;
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = tx.send(Err(ProviderError::Cancelled));
                    break;
                }
                item = byte_stream.next() => item,
            };
            match next {
                None => {
                    let rest = buf.trim();
                    if !rest.is_empty() {
                        match parse_sse_frame(rest) {
                            ParsedFrame::Done => {
                                if !saw_finish
                                    && tx
                                        .send(Ok(StreamEvent::Finished {
                                            reason: FinishReason::Stop,
                                        }))
                                        .is_err()
                                {
                                    return;
                                }
                            }
                            ParsedFrame::Events(events) => {
                                for ev in events {
                                    if tx.send(ev).is_err() {
                                        return;
                                    }
                                }
                            }
                        }
                    }
                    break;
                }
                Some(Err(e)) => {
                    let _ = tx.send(Err(e));
                    break;
                }
                Some(Ok(bytes)) => {
                    buf.push_str(&String::from_utf8_lossy(&bytes));
                    let (events, done) = take_frames(&mut buf);
                    for ev in events {
                        if matches!(ev, Ok(StreamEvent::Finished { .. })) {
                            saw_finish = true;
                        }
                        if tx.send(ev).is_err() {
                            return;
                        }
                    }
                    if done {
                        if !saw_finish {
                            let _ = tx.send(Ok(StreamEvent::Finished {
                                reason: FinishReason::Stop,
                            }));
                        }
                        break;
                    }
                }
            }
        }
    });

    Box::pin(futures::stream::unfold(rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    }))
}

fn take_frames(buf: &mut String) -> (Vec<Result<StreamEvent, ProviderError>>, bool) {
    let mut events = Vec::new();
    let mut done = false;
    loop {
        let idx_lf = buf.find("\n\n");
        let idx_crlf = buf.find("\r\n\r\n");
        let (idx, sep) = match (idx_lf, idx_crlf) {
            (None, None) => break,
            (Some(a), None) => (a, 2usize),
            (None, Some(b)) => (b, 4usize),
            (Some(a), Some(b)) => {
                if a <= b {
                    (a, 2)
                } else {
                    (b, 4)
                }
            }
        };
        let frame = buf[..idx].to_string();
        buf.drain(..idx + sep);
        match parse_sse_frame(&frame) {
            ParsedFrame::Done => done = true,
            ParsedFrame::Events(ev) => events.extend(ev),
        }
        if done {
            break;
        }
    }
    (events, done)
}

enum ParsedFrame {
    Events(Vec<Result<StreamEvent, ProviderError>>),
    Done,
}

fn parse_sse_frame(frame: &str) -> ParsedFrame {
    let mut data_lines = Vec::new();
    for line in frame.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.trim_start());
        }
    }
    if data_lines.is_empty() {
        return ParsedFrame::Events(Vec::new());
    }
    let data = data_lines.join("\n");
    if data == "[DONE]" {
        return ParsedFrame::Done;
    }
    match serde_json::from_str::<ChatChunk>(&data) {
        Ok(chunk) => ParsedFrame::Events(chunk_to_events(chunk)),
        Err(e) => ParsedFrame::Events(vec![Err(ProviderError::InvalidResponse(format!(
            "{e}: {data}"
        )))]),
    }
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    #[serde(default)]
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct Delta {
    content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    #[serde(default)]
    index: usize,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    #[serde(default, deserialize_with = "deserialize_opt_stringish")]
    arguments: Option<String>,
}

fn deserialize_opt_stringish<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(match value {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

fn chunk_to_events(chunk: ChatChunk) -> Vec<Result<StreamEvent, ProviderError>> {
    let mut out = Vec::new();
    for choice in chunk.choices {
        if let Some(text) = choice.delta.content {
            if !text.is_empty() {
                out.push(Ok(StreamEvent::TextDelta { text }));
            }
        }
        if let Some(tcs) = choice.delta.tool_calls {
            for tc in tcs {
                out.push(Ok(StreamEvent::ToolCallDelta {
                    index: tc.index,
                    id: tc.id,
                    name: tc.function.as_ref().and_then(|f| f.name.clone()),
                    arguments: tc.function.as_ref().and_then(|f| f.arguments.clone()),
                }));
            }
        }
        if let Some(reason) = choice.finish_reason {
            let reason = match reason.as_str() {
                "stop" => FinishReason::Stop,
                "tool_calls" => FinishReason::ToolCalls,
                "length" => FinishReason::Length,
                other if other.contains("cancel") => FinishReason::Cancelled,
                _ => FinishReason::Stop,
            };
            out.push(Ok(StreamEvent::Finished { reason }));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(frame: &str) -> Vec<Result<StreamEvent, ProviderError>> {
        match parse_sse_frame(frame) {
            ParsedFrame::Events(ev) => ev,
            ParsedFrame::Done => panic!("expected data frame, got [DONE]"),
        }
    }

    #[test]
    fn parses_text_delta_and_finish() {
        let ev = events(r#"data: {"choices":[{"delta":{"content":"hi"},"finish_reason":null}]}"#);
        assert!(matches!(ev[0], Ok(StreamEvent::TextDelta { .. })));
        let ev = events(r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#);
        assert!(matches!(
            ev[0],
            Ok(StreamEvent::Finished {
                reason: FinishReason::Stop
            })
        ));
    }

    #[test]
    fn parses_tool_call_delta() {
        let ev = events(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":"{}"}}]},"finish_reason":null}]}"#,
        );
        match &ev[0] {
            Ok(StreamEvent::ToolCallDelta {
                name, arguments, ..
            }) => {
                assert_eq!(name.as_deref(), Some("read_file"));
                assert_eq!(arguments.as_deref(), Some("{}"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn done_frame_is_empty() {
        assert!(matches!(parse_sse_frame("data: [DONE]"), ParsedFrame::Done));
    }

    #[test]
    fn missing_tool_call_index_defaults_to_zero() {
        let ev = events(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"id":"c1","function":{"name":"n","arguments":"{}"}}]}}]}"#,
        );
        match &ev[0] {
            Ok(StreamEvent::ToolCallDelta { index, name, .. }) => {
                assert_eq!(*index, 0);
                assert_eq!(name.as_deref(), Some("n"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn take_frames_keeps_index_across_fragmented_tool_call_chunks() {
        let mut buf = String::from(
            r#"data: {"choices":[{"delta":{"tool_calls":[{"index":1,"id":"c1","function":{"name":"get_time","arguments":""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c0","function":{"name":"get_weather","arguments":""}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"tz\":"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Tokyo\"}"}}]}}]}

data: {"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"UTC\"}"}}]}}]}

data: {"choices":[{"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]

"#,
        );
        let (events, done) = take_frames(&mut buf);
        assert!(done);
        let events: Vec<StreamEvent> = events.into_iter().map(|r| r.expect("frame")).collect();
        let mut acc = forger_core::ToolCallAccumulator::new();
        for ev in &events {
            acc.apply_event(ev);
        }
        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments, r#"{"city":"Tokyo"}"#);
        assert_eq!(calls[1].name, "get_time");
        assert_eq!(calls[1].arguments, r#"{"tz":"UTC"}"#);
        assert!(matches!(
            events.last(),
            Some(StreamEvent::Finished {
                reason: FinishReason::ToolCalls
            })
        ));
    }
}
