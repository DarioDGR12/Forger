//! Forger core: message types, swappable seams, and the default agent loop.
//!
//! Nothing in this crate constructs a concrete model backend, filesystem
//! sandbox, or UI. Those are registered at composition time (CLI / server)
//! behind the [`Provider`], [`Tool`], [`Approver`], and [`Agent`] traits.
//! The default [`crate::agent_loop::AgentLoop`] is one implementation of
//! [`Agent`] — it can be replaced without changing this crate's types.

pub mod agent;
pub mod agent_loop;
pub mod approval;
pub mod compact;
pub mod context;
pub mod error;
pub mod message;
pub mod paths;
pub mod plugin;
pub mod quality;
pub mod session;
pub mod tool;

pub use agent::{Agent, AgentEvent, TurnOutcome, UserTurn};
pub use agent_loop::{AgentLoop, AgentLoopConfig};
pub use approval::{
    ApprovalError, ApprovalKind, ApprovalRequest, Approver, AutoApprover, Decision,
};
pub use compact::compact_messages;
pub use context::compose_system_prompt;
pub use error::{AgentError, ProviderError, ToolError};
pub use message::{FinishReason, Message, Role, StreamEvent, ToolCall};
pub use plugin::{Plugin, PluginId, Provider};
pub use quality::{QualityConfig, QualityReport, QualityRunner, ScoredCandidate};
pub use session::Session;
pub use tool::{required_path, Tool, ToolContext, ToolRegistry, ToolSpec};

pub use tokio_util::sync::CancellationToken;
