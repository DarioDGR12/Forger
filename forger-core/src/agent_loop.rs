//! Default agent driver. Replace by implementing [`crate::agent::Agent`].
//!
//! Turn protocol:
//! 1. Append the user message (committed).
//! 2. Stream the model. Buffer the assistant locally.
//! 3. On a clean finish, commit the assistant message.
//! 4. If there are tool calls, execute them (sensitive + denylist approvals
//!    are independent) and commit each tool result before the next step.
//! 5. On cancel or a truncated stream, restore the session to the last
//!    checkpoint so it is never left with a half-written assistant turn or
//!    a tool_call without a matching result.

use crate::agent::{emit, stream_event_to_agent, Agent, AgentEvent, TurnOutcome, UserTurn};
use crate::approval::{ApprovalKind, ApprovalRequest, Approver, Decision};
use crate::error::{AgentError, ProviderError, ToolError};
use crate::message::{FinishReason, Message, StreamEvent, ToolCall};
use crate::plugin::Provider;
use crate::session::Session;
use crate::tool::{ToolContext, ToolRegistry};
use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const DEFAULT_MAX_STEPS: usize = 20;
const DEFAULT_SYSTEM_PROMPT: &str = "You are Forger, a coding agent. Use list_dir and grep to explore, read_file to inspect, edit_file for targeted patches, write_file only for new files, and rename_file to rename. run_command is for builds and tests, not for reading or renaming files. Never exfiltrate secrets; .env/.git/.ssh/credentials are blocked unless the user explicitly overrides the denylist.";

#[derive(Clone)]
pub struct AgentLoopConfig {
    pub system_prompt: String,
    pub max_steps: usize,
    pub workspace: PathBuf,
}

impl Default for AgentLoopConfig {
    fn default() -> Self {
        Self {
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            max_steps: DEFAULT_MAX_STEPS,
            workspace: PathBuf::from("."),
        }
    }
}

pub struct AgentLoop {
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    approver: Arc<dyn Approver>,
    config: AgentLoopConfig,
}

impl AgentLoop {
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: ToolRegistry,
        approver: Arc<dyn Approver>,
        config: AgentLoopConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            approver,
            config,
        }
    }

    pub fn config(&self) -> &AgentLoopConfig {
        &self.config
    }

    fn model_messages(&self, session: &Session) -> Vec<Message> {
        let mut out = Vec::with_capacity(session.len() + 1);
        out.push(Message::system(&self.config.system_prompt));
        out.extend(session.messages().iter().cloned());
        out
    }

    async fn collect_assistant(
        &self,
        session: &Session,
        cancel: &CancellationToken,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<(String, Vec<ToolCall>, FinishReason), AgentError> {
        let specs = self.tools.specs();
        let messages = self.model_messages(session);
        let mut stream = self
            .provider
            .stream(&messages, &specs, cancel.clone())
            .await?;

        let mut text = String::new();
        let mut builders: Vec<ToolCallBuilder> = Vec::new();
        let mut finish = None;

        loop {
            if cancel.is_cancelled() {
                return Err(AgentError::Cancelled);
            }
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    return Err(AgentError::Cancelled);
                }
                item = stream.next() => item,
            };
            match next {
                None => {
                    if finish.is_some() {
                        break;
                    }
                    return Err(AgentError::Provider(ProviderError::TruncatedStream));
                }
                Some(Err(e)) => return Err(AgentError::Provider(e)),
                Some(Ok(ev)) => {
                    if let Some(ae) = stream_event_to_agent(ev.clone()) {
                        emit(sink, ae);
                    }
                    match ev {
                        StreamEvent::TextDelta { text: delta } => text.push_str(&delta),
                        StreamEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments,
                        } => {
                            while builders.len() <= index {
                                builders.push(ToolCallBuilder::default());
                            }
                            let b = &mut builders[index];
                            if let Some(id) = id {
                                b.id = id;
                            }
                            if let Some(name) = name {
                                b.name = name;
                            }
                            if let Some(arguments) = arguments {
                                b.arguments.push_str(&arguments);
                            }
                        }
                        StreamEvent::Finished { reason } => {
                            finish = Some(reason);
                        }
                    }
                }
            }
        }

        let reason = finish.unwrap_or(FinishReason::Stop);
        let calls: Vec<ToolCall> = builders
            .into_iter()
            .filter_map(|b| b.into_tool_call())
            .collect();
        Ok((text, calls, reason))
    }

    async fn execute_tools(
        &self,
        calls: &[ToolCall],
        cancel: &CancellationToken,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<Vec<Message>, AgentError> {
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            if cancel.is_cancelled() {
                results.push(Message::tool_result(
                    &call.id,
                    &call.name,
                    "tool call cancelled before execution",
                ));
                continue;
            }
            emit(sink, AgentEvent::ToolCall { call: call.clone() });
            let output = self.execute_one(call, cancel).await;
            emit(
                sink,
                AgentEvent::ToolResult {
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    output: output.clone(),
                },
            );
            results.push(Message::tool_result(&call.id, &call.name, output));
        }
        Ok(results)
    }

    async fn execute_one(&self, call: &ToolCall, cancel: &CancellationToken) -> String {
        let tool = match self.tools.get(&call.name) {
            Some(t) => t,
            None => {
                return format!(
                    "error: unknown tool `{}`. Available: {}",
                    call.name,
                    self.tools
                        .specs()
                        .into_iter()
                        .map(|s| s.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        };

        let args: Value = match serde_json::from_str(&call.arguments) {
            Ok(v) => v,
            Err(e) => {
                return format!(
                    "error: malformed tool arguments (not valid JSON): {e}. arguments were: {}",
                    call.arguments
                );
            }
        };

        if tool.is_sensitive() {
            match self
                .confirm(
                    ApprovalKind::SensitiveTool,
                    &call.name,
                    &args,
                    None,
                    "sensitive tool invocation",
                )
                .await
            {
                Ok(Decision::Allow) => {}
                Ok(Decision::Deny { reason }) => {
                    return format!("error: tool denied ({reason})");
                }
                Err(e) => return format!("error: approval failed ({e})"),
            }
        }

        let denylist_hit = args
            .get("path")
            .and_then(Value::as_str)
            .and_then(denylist_hint);
        let mut denylist_override = false;
        if let Some((path, reason)) = denylist_hit {
            match self
                .confirm(
                    ApprovalKind::DenylistOverride,
                    &call.name,
                    &args,
                    Some(path.clone()),
                    &reason,
                )
                .await
            {
                Ok(Decision::Allow) => denylist_override = true,
                Ok(Decision::Deny { reason }) => {
                    return format!("error: denylist blocked write/read ({reason})");
                }
                Err(e) => return format!("error: denylist approval failed ({e})"),
            }
        }

        let ctx = ToolContext {
            workspace: &self.config.workspace,
            cancel: cancel.clone(),
            denylist_override,
        };
        match tool.execute(args, &ctx).await {
            Ok(out) => out,
            Err(ToolError::Cancelled) => "error: tool cancelled".into(),
            Err(e) => format!("error: {e}"),
        }
    }

    async fn confirm(
        &self,
        kind: ApprovalKind,
        tool_name: &str,
        args: &Value,
        path: Option<PathBuf>,
        reason: &str,
    ) -> Result<Decision, AgentError> {
        let req = ApprovalRequest {
            kind,
            tool_name: tool_name.into(),
            arguments: args.clone(),
            path,
            reason: reason.into(),
        };
        self.approver
            .approve(&req)
            .await
            .map_err(|e| AgentError::Approval(e.to_string()))
    }
}

/// Cheap name-based hint so the loop can ask for a denylist override *before*
/// invoking the tool. The sandbox still resolves the *final* path (symlinks,
/// `..`) independently — this is the generic layer, not the sandbox layer.
pub(crate) fn denylist_hint(path: &str) -> Option<(PathBuf, String)> {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    let patterns = [".env", ".git", ".ssh", "credentials"];
    for pat in patterns {
        if path_looks_like(&lower, pat) {
            return Some((
                PathBuf::from(path),
                format!("path `{path}` matches denylist pattern `{pat}`"),
            ));
        }
    }
    None
}

fn path_looks_like(lower: &str, pat: &str) -> bool {
    lower.split('/').any(|comp| {
        if pat == ".env" {
            comp == ".env" || comp.starts_with(".env.")
        } else if pat == "credentials" {
            comp == "credentials" || comp.ends_with("credentials")
        } else {
            comp == pat
        }
    })
}

#[derive(Default)]
struct ToolCallBuilder {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallBuilder {
    fn into_tool_call(self) -> Option<ToolCall> {
        if self.name.is_empty() && self.id.is_empty() {
            return None;
        }
        Some(ToolCall {
            id: if self.id.is_empty() {
                format!("call-{}", self.name)
            } else {
                self.id
            },
            name: self.name,
            arguments: self.arguments,
        })
    }
}

#[async_trait]
impl Agent for AgentLoop {
    async fn run_turn(
        &self,
        session: &mut Session,
        input: UserTurn,
        cancel: CancellationToken,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnOutcome, AgentError> {
        session.push(Message::user(input.text));

        for step in 0..self.config.max_steps {
            if cancel.is_cancelled() {
                emit(sink, AgentEvent::Cancelled);
                return Ok(TurnOutcome::Cancelled);
            }
            emit(sink, AgentEvent::Step { index: step });
            let checkpoint = session.checkpoint();

            let collected = self.collect_assistant(session, &cancel, sink).await;
            let (text, calls, reason) = match collected {
                Ok(v) => v,
                Err(AgentError::Cancelled) => {
                    session.restore(checkpoint);
                    emit(sink, AgentEvent::Cancelled);
                    return Ok(TurnOutcome::Cancelled);
                }
                Err(AgentError::Provider(ProviderError::TruncatedStream)) => {
                    session.restore(checkpoint);
                    return Err(AgentError::Provider(ProviderError::TruncatedStream));
                }
                Err(e) => {
                    session.restore(checkpoint);
                    return Err(e);
                }
            };

            if !calls.is_empty() {
                session.push(Message::assistant_with_tools(text, calls.clone()));
                let results = self.execute_tools(&calls, &cancel, sink).await?;
                for r in results {
                    session.push(r);
                }
                if cancel.is_cancelled() {
                    emit(sink, AgentEvent::Cancelled);
                    return Ok(TurnOutcome::Cancelled);
                }
                continue;
            }

            session.push(Message::assistant(text));
            let outcome = match reason {
                FinishReason::Cancelled => TurnOutcome::Cancelled,
                FinishReason::Error => TurnOutcome::Failed,
                _ => TurnOutcome::Completed,
            };
            emit(sink, AgentEvent::Finished { outcome });
            return Ok(outcome);
        }

        Err(AgentError::StepLimit(self.config.max_steps))
    }
}

impl AgentLoop {
    pub fn workspace(&self) -> &Path {
        &self.config.workspace
    }
}

#[cfg(test)]
mod tests {
    use super::path_looks_like;

    #[test]
    fn denylist_hint_matches_env_and_git() {
        assert!(path_looks_like(".env", ".env"));
        assert!(path_looks_like("foo/.env.local", ".env"));
        assert!(path_looks_like("src/.git/config", ".git"));
        assert!(path_looks_like(".ssh/id_rsa", ".ssh"));
        assert!(path_looks_like(".aws/credentials", "credentials"));
        assert!(!path_looks_like("src/main.rs", ".env"));
        assert!(!path_looks_like("env.txt", ".env"));
    }
}
