use crate::error::AgentError;
use crate::message::{StreamEvent, ToolCall};
use crate::session::Session;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct UserTurn {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TextDelta { text: String },
    ToolCall { call: ToolCall },
    ToolResult { call_id: String, name: String, output: String },
    Warning { message: String },
    Step { index: usize },
    Cancelled,
    Finished { outcome: TurnOutcome },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Cancelled,
    Failed,
}

/// Swappable agent driver. [`crate::agent_loop::AgentLoop`] is the default.
#[async_trait]
pub trait Agent: Send + Sync {
    async fn run_turn(
        &self,
        session: &mut Session,
        input: UserTurn,
        cancel: CancellationToken,
        sink: &mut (dyn FnMut(AgentEvent) + Send),
    ) -> Result<TurnOutcome, AgentError>;
}

pub fn emit(sink: &mut (dyn FnMut(AgentEvent) + Send), event: AgentEvent) {
    sink(event);
}

pub fn stream_event_to_agent(ev: StreamEvent) -> Option<AgentEvent> {
    match ev {
        StreamEvent::TextDelta { text } => Some(AgentEvent::TextDelta { text }),
        StreamEvent::ToolCallDelta { .. } | StreamEvent::Finished { .. } => None,
    }
}
