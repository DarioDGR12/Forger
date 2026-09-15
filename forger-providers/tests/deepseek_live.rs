//! Live integration test against DeepSeek's OpenAI-compatible endpoint.
//!
//! Skips (does not fail) when `DEEPSEEK_API_KEY` is unset or empty:
//!
//! ```text
//! DEEPSEEK_API_KEY=sk-... cargo test -p forger-providers --test deepseek_live -- --nocapture
//! ```

use forger_core::{
    CancellationToken, FinishReason, Message, Provider, StreamEvent, ToolCallAccumulator, ToolSpec,
};
use forger_providers::{OpenAiCompatConfig, OpenAiCompatProvider, ToolChoice};
use futures::StreamExt;
use serde_json::json;

fn deepseek_api_key() -> Option<String> {
    match std::env::var("DEEPSEEK_API_KEY") {
        Ok(key) if !key.trim().is_empty() => Some(key),
        _ => None,
    }
}

fn weather_tool() -> ToolSpec {
    ToolSpec {
        name: "get_weather".into(),
        description: "Get the current weather for a city. The user must supply a location.".into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "location": {
                    "type": "string",
                    "description": "City and country, e.g. Tokyo, Japan"
                }
            },
            "required": ["location"]
        }),
    }
}

#[tokio::test]
async fn deepseek_streaming_assembles_tool_calls_by_index() {
    let Some(api_key) = deepseek_api_key() else {
        eprintln!("skipping live DeepSeek test: DEEPSEEK_API_KEY is not set");
        return;
    };

    let provider = OpenAiCompatProvider::new(
        OpenAiCompatConfig::deepseek(api_key).with_tool_choice(ToolChoice::Required),
    );
    let messages = vec![Message::user(
        "What is the weather in Tokyo, Japan right now? Use the get_weather tool.",
    )];
    let tools = vec![weather_tool()];

    let mut stream = provider
        .stream(&messages, &tools, CancellationToken::new())
        .await
        .expect("DeepSeek stream should start");

    let mut saw_index = false;
    let mut accum = ToolCallAccumulator::new();
    let mut finish = None;

    while let Some(item) = stream.next().await {
        let event = item.expect("stream event from DeepSeek");
        if let StreamEvent::ToolCallDelta { index, .. } = &event {
            if *index == 0 {
                saw_index = true;
            }
        }
        if let StreamEvent::Finished { reason } = &event {
            finish = Some(*reason);
        }
        accum.apply_event(&event);
    }

    let calls = accum.finish();
    assert!(
        !calls.is_empty(),
        "expected assembled tool_calls, finish={finish:?}"
    );
    let call = &calls[0];
    assert_eq!(call.name, "get_weather");
    assert!(
        !call.id.is_empty(),
        "tool call id should arrive on the first fragment"
    );
    let args: serde_json::Value = serde_json::from_str(&call.arguments).unwrap_or_else(|err| {
        panic!(
            "arguments must be valid JSON after concat: {err}; raw={}",
            call.arguments
        )
    });
    let location = args
        .get("location")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(
        location.contains("tokyo"),
        "expected Tokyo in get_weather arguments, got {args}"
    );
    assert!(
        saw_index,
        "provider must surface ToolCallDelta.index from the SSE chunks"
    );
    assert!(
        finish == Some(FinishReason::ToolCalls) || !calls.is_empty(),
        "finish_reason={finish:?}"
    );
}
