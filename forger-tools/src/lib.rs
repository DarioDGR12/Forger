//! Stock tools. Each captures a [`forger_sandbox::Sandbox`] at construction
//! so the agent loop never depends on a filesystem backend.

mod edit_file;
mod grep;
mod list_dir;
mod read_file;
mod rename_file;
mod run_command;
mod write_file;

pub use edit_file::EditFile;
pub use grep::Grep;
pub use list_dir::ListDir;
pub use read_file::ReadFile;
pub use rename_file::RenameFile;
pub use run_command::RunCommand;
pub use write_file::WriteFile;

use forger_core::{ToolError, ToolRegistry};
use forger_sandbox::{PathDecision, Sandbox, WritePermit};
use std::sync::Arc;
use std::time::Duration;

pub fn stock_tools(sandbox: Arc<dyn Sandbox>, run_timeout: Duration) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(ReadFile::new(sandbox.clone())));
    reg.register(Arc::new(ListDir::new(sandbox.clone())));
    reg.register(Arc::new(Grep::new(sandbox.clone())));
    reg.register(Arc::new(EditFile::new(sandbox.clone())));
    reg.register(Arc::new(WriteFile::new(sandbox.clone())));
    reg.register(Arc::new(RenameFile::new(sandbox.clone())));
    reg.register(Arc::new(RunCommand::new(sandbox, run_timeout)));
    reg
}

pub(crate) fn path_permit(
    tool: &str,
    decision: &PathDecision,
    denylist_override: bool,
) -> Result<WritePermit, ToolError> {
    match &decision.denylist {
        None => Ok(WritePermit::Normal),
        Some(_) if denylist_override => Ok(WritePermit::DenylistOverride {
            confirmed_resolved: decision.resolved.clone(),
        }),
        Some(hit) => Err(ToolError::Denied {
            name: tool.into(),
            reason: format!(
                "denylist blocked `{}` (pattern `{}`); this is independent of sensitive-tool confirmation",
                decision.resolved.display(),
                hit.pattern
            ),
        }),
    }
}

pub(crate) fn failed(tool: &str, e: impl ToString) -> ToolError {
    ToolError::Failed {
        name: tool.into(),
        reason: e.to_string(),
    }
}
