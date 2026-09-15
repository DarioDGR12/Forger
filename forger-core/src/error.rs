use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum ProviderError {
    #[error("provider request failed: {0}")]
    Transport(String),
    #[error("provider returned invalid payload: {0}")]
    InvalidResponse(String),
    #[error("stream ended before a finish event")]
    TruncatedStream,
    #[error("provider cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("unknown tool `{0}`")]
    Unknown(String),
    #[error("malformed tool arguments: {0}")]
    MalformedArgs(String),
    #[error("tool `{name}` denied: {reason}")]
    Denied { name: String, reason: String },
    #[error("tool `{name}` timed out after {timeout_ms}ms")]
    Timeout { name: String, timeout_ms: u64 },
    #[error("tool `{name}` failed: {reason}")]
    Failed { name: String, reason: String },
    #[error("tool cancelled")]
    Cancelled,
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error(transparent)]
    Tool(#[from] ToolError),
    #[error("approval: {0}")]
    Approval(String),
    #[error("turn cancelled")]
    Cancelled,
    #[error("agent exceeded {0} steps in a single turn")]
    StepLimit(usize),
    #[error("{0}")]
    Other(String),
}
