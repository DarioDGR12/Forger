//! HTTP-level stream through `OpenAiCompatProvider`, including tool_calls
//! fragments split across TCP writes the way DeepSeek/OpenAI actually send them.

use forger_core::{
    CancellationToken, FinishReason, Message, Provider, StreamEvent, ToolCallAccumulator, ToolSpec,
};
use forger_providers::{OpenAiCompatConfig, OpenAiCompatProvider, ToolChoice};
use futures::StreamExt;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn sse_body(payloads: &[&str]) -> String {
    let mut body = String::new();
    for payload in payloads {
        body.push_str("data: ");
        body.push_str(payload);
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

async fn spawn_http(status: u16, content_type: &str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let content_type = content_type.to_string();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 64 * 1024];
        let mut filled = 0usize;
        loop {
            if filled >= buf.len() {
                break;
            }
            let n = socket.read(&mut buf[filled..]).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            filled += n;
            if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let reason = if status == 200 { "OK" } else { "Error" };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = socket.write_all(header.as_bytes()).await;
        // Split the SSE body the way a real proxy/TCP stack would.
        for chunk in body.chunks(17) {
            if socket.write_all(chunk).await.is_err() {
                break;
            }
        }
        let _ = socket.shutdown().await;
    });
    format!("http://{addr}")
}

async fn provider_against(base_url: &str) -> OpenAiCompatProvider {
    OpenAiCompatProvider::new(OpenAiCompatConfig {
        base_url: base_url.into(),
        api_key: "test-key".into(),
        model: "deepseek-chat".into(),
        tool_choice: ToolChoice::Required,
    })
}

#[tokio::test]
async fn http_stream_assembles_fragmented_tool_calls_across_tcp_chunks() {
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"role":"assistant","content":null},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_time","type":"function","function":{"name":"get_time","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_weather","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"location\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"tz\":"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Tokyo\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"UTC\"}"}}]},"finish_reason":null}]}"#,
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#,
    ]);
    let url = spawn_http(200, "text/event-stream", body.into_bytes()).await;
    let provider = provider_against(&url).await;
    let mut stream = provider
        .stream(
            &[Message::user("weather and time")],
            &[ToolSpec {
                name: "get_weather".into(),
                description: "weather".into(),
                parameters: json!({"type":"object"}),
            }],
            CancellationToken::new(),
        )
        .await
        .expect("stream starts");

    let mut accum = ToolCallAccumulator::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        let event = item.expect("event");
        if let StreamEvent::Finished { reason } = &event {
            finish = Some(*reason);
        }
        accum.apply_event(&event);
    }

    let calls = accum.finish();
    assert_eq!(finish, Some(FinishReason::ToolCalls));
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].id, "call_weather");
    assert_eq!(calls[0].name, "get_weather");
    assert_eq!(calls[0].arguments, r#"{"location":"Tokyo"}"#);
    assert_eq!(calls[1].id, "call_time");
    assert_eq!(calls[1].name, "get_time");
    assert_eq!(calls[1].arguments, r#"{"tz":"UTC"}"#);
}

#[tokio::test]
async fn http_done_without_finish_reason_still_completes() {
    let body = sse_body(&[
        r#"{"choices":[{"delta":{"content":"Hello"}}]}"#,
        r#"{"choices":[{"delta":{"content":"!"}}]}"#,
    ]);
    let url = spawn_http(200, "text/event-stream", body.into_bytes()).await;
    let provider = provider_against(&url).await;
    let mut stream = provider
        .stream(&[Message::user("hi")], &[], CancellationToken::new())
        .await
        .unwrap();
    let mut text = String::new();
    let mut finish = None;
    while let Some(item) = stream.next().await {
        match item.unwrap() {
            StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
            StreamEvent::Finished { reason } => finish = Some(reason),
            _ => {}
        }
    }
    assert_eq!(text, "Hello!");
    assert_eq!(finish, Some(FinishReason::Stop));
}

#[tokio::test]
async fn http_error_is_not_a_truncated_stream() {
    let url = spawn_http(
        401,
        "application/json",
        br#"{"error":{"message":"bad key"}}"#.to_vec(),
    )
    .await;
    let err = match provider_against(&url)
        .await
        .stream(&[Message::user("hi")], &[], CancellationToken::new())
        .await
    {
        Err(e) => e,
        Ok(_) => panic!("401 should fail before the SSE stream"),
    };
    let msg = err.to_string();
    assert!(msg.contains("401"), "{msg}");
    assert!(msg.contains("bad key"), "{msg}");
}
