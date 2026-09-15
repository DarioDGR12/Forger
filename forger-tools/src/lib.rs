//! Stock tools. Each captures a [`forger_sandbox::Sandbox`] at construction
//! so the agent loop never depends on a filesystem backend.

mod read_file;
mod run_command;
mod write_file;

pub use read_file::ReadFile;
pub use run_command::RunCommand;
pub use write_file::WriteFile;

use forger_core::{ToolError, ToolRegistry};
use forger_sandbox::{Sandbox, SandboxError};
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn sandbox_to_tool_error(name: &str, err: SandboxError) -> ToolError {
    match err {
        SandboxError::Denylist { path, pattern } => ToolError::Denied {
            name: name.into(),
            reason: format!(
                "denylist blocked `{path}` (pattern `{pattern}`); this is independent of sensitive-tool confirmation"
            ),
        },
        SandboxError::Cancelled => ToolError::Cancelled,
        SandboxError::Timeout { timeout_ms } => ToolError::Timeout {
            name: name.into(),
            timeout_ms,
        },
        other => ToolError::Failed {
            name: name.into(),
            reason: other.to_string(),
        },
    }
}

pub fn stock_tools(sandbox: Arc<dyn Sandbox>, run_timeout: Duration) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(ReadFile::new(sandbox.clone())));
    reg.register(Arc::new(WriteFile::new(sandbox.clone())));
    reg.register(Arc::new(RunCommand::new(sandbox, run_timeout)));
    reg
}
