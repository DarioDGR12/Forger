//! End-to-end AgentLoop tests against an in-crate scripted provider.
//!
//! Covers: reusable session, streaming, malformed tool calls, truncated
//! streams, sensitive vs denylist confirmation, and cancel-without-corruption.

use async_trait::async_trait;
use forger_core::approval::AutoApprover;
use forger_core::{
    Agent, AgentEvent, AgentLoop, AgentLoopConfig, CancellationToken, FinishReason, Message,
    Plugin, PluginId, Provider, ProviderError, Role, Session, StreamEvent, Tool, ToolContext,
    ToolError, ToolRegistry, ToolSpec, TurnOutcome, UserTurn,
};
use futures::stream::{self, BoxStream};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::timeout;

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
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        if cancel.is_cancelled() {
            return Err(ProviderError::Cancelled);
        }
        let next = {
            let mut g = self.scripts.lock().unwrap();
            if g.is_empty() {
                vec![Ok(StreamEvent::TextDelta {
                    text: "default".into(),
                }), Ok(StreamEvent::Finished {
                    reason: FinishReason::Stop,
                })]
            } else {
                g.remove(0)
            }
        };
        Ok(Box::pin(stream::iter(next)))
    }
}

struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "echo".into(),
            description: "echo args".into(),
            parameters: json!({"type":"object","properties":{"text":{"type":"string"}}}),
        }
    }

    async fn execute(&self, args: Value, _ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        Ok(args
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string())
    }
}

struct SensitiveWrite;

#[async_trait]
impl Tool for SensitiveWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "write".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"},"contents":{"type":"string"}}}),
        }
    }

    fn is_sensitive(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or("");
        if path.contains(".env") && !ctx.denylist_override {
            return Err(ToolError::Denied {
                name: "write_file".into(),
                reason: "denylist (sandbox layer) blocked .env".into(),
            });
        }
        Ok(format!("wrote {path}"))
    }
}

fn loop_with(
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    approver: AutoApprover,
) -> AgentLoop {
    AgentLoop::new(
        provider,
        tools,
        Arc::new(approver),
        AgentLoopConfig::default(),
    )
}

fn text_then(text: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    vec![
        Ok(StreamEvent::TextDelta {
            text: text.into(),
        }),
        Ok(StreamEvent::Finished {
            reason: FinishReason::Stop,
        }),
    ]
}

fn tool_call(id: &str, name: &str, args: &str) -> Vec<Result<StreamEvent, ProviderError>> {
    vec![
        Ok(StreamEvent::ToolCallDelta {
            index: 0,
            id: Some(id.into()),
            name: Some(name.into()),
            arguments: Some(args.into()),
        }),
        Ok(StreamEvent::Finished {
            reason: FinishReason::ToolCalls,
        }),
    ]
}

async fn run(
    agent: &AgentLoop,
    session: &mut Session,
    text: &str,
) -> Result<TurnOutcome, forger_core::AgentError> {
    let mut sink = |_e: AgentEvent| {};
    agent
        .run_turn(
            session,
            UserTurn {
                text: text.into(),
            },
            CancellationToken::new(),
            &mut sink,
        )
        .await
}

#[tokio::test]
async fn end_to_end_turn_against_scripted_provider() {
    let provider = ScriptedProvider::new(vec![text_then("hello from mock")]);
    let agent = loop_with(provider, ToolRegistry::new(), AutoApprover {
        allow_sensitive: true,
        allow_denylist: false,
    });
    let mut session = Session::new();
    let outcome = run(&agent, &mut session, "hi").await.unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(session.last_assistant_text(), Some("hello from mock"));
}

#[tokio::test]
async fn reusable_session_appends_across_turns() {
    let provider = ScriptedProvider::new(vec![text_then("one"), text_then("two")]);
    let agent = loop_with(provider, ToolRegistry::new(), AutoApprover::default());
    let mut session = Session::new();
    run(&agent, &mut session, "first").await.unwrap();
    run(&agent, &mut session, "second").await.unwrap();
    let roles: Vec<_> = session.messages().iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::User, Role::Assistant]
    );
    assert_eq!(session.last_assistant_text(), Some("two"));
}

#[tokio::test]
async fn streaming_emits_text_deltas() {
    let provider = ScriptedProvider::new(vec![vec![
        Ok(StreamEvent::TextDelta { text: "hel".into() }),
        Ok(StreamEvent::TextDelta { text: "lo".into() }),
        Ok(StreamEvent::Finished {
            reason: FinishReason::Stop,
        }),
    ]]);
    let agent = loop_with(provider, ToolRegistry::new(), AutoApprover::default());
    let mut session = Session::new();
    let mut deltas = Vec::new();
    {
        let mut sink = |e: AgentEvent| {
            if let AgentEvent::TextDelta { text } = e {
                deltas.push(text);
            }
        };
        agent
            .run_turn(
                &mut session,
                UserTurn {
                    text: "go".into(),
                },
                CancellationToken::new(),
                &mut sink,
            )
            .await
            .unwrap();
    }
    assert_eq!(deltas, vec!["hel", "lo"]);
    assert_eq!(session.last_assistant_text(), Some("hello"));
}

#[tokio::test]
async fn malformed_tool_call_does_not_panic_and_continues() {
    let provider = ScriptedProvider::new(vec![
        tool_call("c1", "echo", "this is not json{{{"),
        text_then("recovered"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = loop_with(provider, tools, AutoApprover {
        allow_sensitive: true,
        allow_denylist: false,
    });
    let mut session = Session::new();
    run(&agent, &mut session, "call it").await.unwrap();
    let tool_msg = session
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .expect("tool result");
    assert!(tool_msg.content.contains("malformed"));
    assert_eq!(session.last_assistant_text(), Some("recovered"));
}

#[tokio::test]
async fn truncated_stream_does_not_corrupt_session() {
    let provider = ScriptedProvider::new(vec![vec![Ok(StreamEvent::TextDelta {
        text: "partial".into(),
    })]]);
    let agent = loop_with(provider, ToolRegistry::new(), AutoApprover::default());
    let mut session = Session::new();
    let err = run(&agent, &mut session, "hi").await.unwrap_err();
    assert!(matches!(
        err,
        forger_core::AgentError::Provider(ProviderError::TruncatedStream)
    ));
    // user message stays; no half assistant
    assert_eq!(session.len(), 1);
    assert_eq!(session.messages()[0].role, Role::User);
}

#[tokio::test]
async fn sensitive_tool_denied_without_approval() {
    let provider = ScriptedProvider::new(vec![
        tool_call("c1", "write_file", r#"{"path":"ok.txt","contents":"x"}"#),
        text_then("ok"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(SensitiveWrite));
    let agent = loop_with(
        provider,
        tools,
        AutoApprover {
            allow_sensitive: false,
            allow_denylist: false,
        },
    );
    let mut session = Session::new();
    run(&agent, &mut session, "write").await.unwrap();
    let tool_msg = session
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(tool_msg.content.contains("denied"));
}

#[tokio::test]
async fn denylist_layer_is_independent_of_sensitive_yes() {
    let provider = ScriptedProvider::new(vec![
        tool_call("c1", "write_file", r#"{"path":".env","contents":"SECRET=1"}"#),
        text_then("blocked"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(SensitiveWrite));
    // --yes analogue: sensitive allowed, denylist still denied
    let agent = loop_with(
        provider,
        tools,
        AutoApprover {
            allow_sensitive: true,
            allow_denylist: false,
        },
    );
    let mut session = Session::new();
    run(&agent, &mut session, "leak").await.unwrap();
    let tool_msg = session
        .messages()
        .iter()
        .find(|m| m.role == Role::Tool)
        .unwrap();
    assert!(
        tool_msg.content.contains("denylist"),
        "expected denylist denial, got {}",
        tool_msg.content
    );
}

#[tokio::test]
async fn cancel_mid_stream_leaves_session_reusable() {
    let provider = ScriptedProvider::new(vec![vec![
        Ok(StreamEvent::TextDelta {
            text: "aaa".into(),
        }),
        Ok(StreamEvent::Finished {
            reason: FinishReason::Stop,
        }),
    ]]);
    let agent = loop_with(provider, ToolRegistry::new(), AutoApprover::default());
    let mut session = Session::new();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut sink = |_e: AgentEvent| {};
    let outcome = agent
        .run_turn(
            &mut session,
            UserTurn {
                text: "hi".into(),
            },
            cancel,
            &mut sink,
        )
        .await
        .unwrap();
    assert_eq!(outcome, TurnOutcome::Cancelled);
    // user message is committed; no dangling assistant
    assert_eq!(session.len(), 1);
    assert_eq!(session.messages()[0].role, Role::User);

    // session is reusable
    let provider2 = ScriptedProvider::new(vec![text_then("after cancel")]);
    let agent2 = loop_with(provider2, ToolRegistry::new(), AutoApprover::default());
    run(&agent2, &mut session, "again").await.unwrap();
    assert_eq!(session.last_assistant_text(), Some("after cancel"));
}

#[tokio::test]
async fn cancel_after_tool_calls_still_writes_tool_results() {
    // If the assistant with tool_calls is committed, matching tool results
    // must exist even when cancel fires during execution.
    struct HangTool;
    #[async_trait]
    impl Tool for HangTool {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: "hang".into(),
                description: "hangs until cancelled".into(),
                parameters: json!({"type":"object"}),
            }
        }
        async fn execute(&self, _args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
            tokio::select! {
                _ = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
                _ = tokio::time::sleep(Duration::from_secs(30)) => Ok("slept".into()),
            }
        }
    }

    let provider = ScriptedProvider::new(vec![tool_call("c1", "hang", "{}")]);
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(HangTool));
    let agent = loop_with(provider, tools, AutoApprover {
        allow_sensitive: true,
        allow_denylist: false,
    });
    let mut session = Session::new();
    let cancel = CancellationToken::new();
    let cancel2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel2.cancel();
    });
    let mut sink = |_e: AgentEvent| {};
    let _ = timeout(
        Duration::from_secs(2),
        agent.run_turn(
            &mut session,
            UserTurn {
                text: "hang".into(),
            },
            cancel,
            &mut sink,
        ),
    )
    .await
    .expect("turn should not hang");

    let has_assistant_tools = session
        .messages()
        .iter()
        .any(|m| m.role == Role::Assistant && m.has_tool_calls());
    let tool_results = session
        .messages()
        .iter()
        .filter(|m| m.role == Role::Tool)
        .count();
    if has_assistant_tools {
        assert_eq!(
            tool_results, 1,
            "committed tool_calls must have matching results: {:?}",
            session.messages()
        );
    }
}

#[tokio::test]
async fn max_turns_executes_last_tool_calls_without_orphans_or_hard_error() {
    // Bug: the loop used to `continue` after tools on the last step, fall out
    // of `0..max_steps`, and return Err(StepLimit) *after* committing
    // assistant+tool messages. Callers then treated a well-formed transcript
    // as a crash.
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = AgentLoop::new(
        ScriptedProvider::new(vec![
            tool_call("c1", "echo", r#"{"text":"from-tool"}"#),
            text_then("must-not-run"),
        ]),
        tools,
        Arc::new(AutoApprover {
            allow_sensitive: true,
            allow_denylist: false,
        }),
        AgentLoopConfig::default().with_max_turns(1),
    );
    let mut session = Session::new();
    let outcome = run(&agent, &mut session, "go").await.unwrap();
    assert_eq!(outcome, TurnOutcome::StepLimit);
    let roles: Vec<_> = session.messages().iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::Tool]);
    assert!(session.messages()[1].has_tool_calls());
    assert_eq!(session.messages()[2].content, "from-tool");
    assert_eq!(
        session.last_assistant_text(),
        None,
        "no extra provider turn should have produced final text"
    );
}

#[tokio::test]
async fn max_turns_two_allows_tool_then_final_text() {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = AgentLoop::new(
        ScriptedProvider::new(vec![
            tool_call("c1", "echo", r#"{"text":"from-tool"}"#),
            text_then("done"),
        ]),
        tools,
        Arc::new(AutoApprover {
            allow_sensitive: true,
            allow_denylist: false,
        }),
        AgentLoopConfig::default().with_max_turns(2),
    );
    let mut session = Session::new();
    let outcome = run(&agent, &mut session, "go").await.unwrap();
    assert_eq!(outcome, TurnOutcome::Completed);
    assert_eq!(session.last_assistant_text(), Some("done"));
}
