//! Stock tools. Each captures a [`forger_sandbox::Sandbox`] at construction
//! so the agent loop never depends on a filesystem backend.

mod read_file;
mod run_command;
mod write_file;

pub use read_file::ReadFile;
pub use run_command::RunCommand;
pub use write_file::WriteFile;

use forger_core::ToolRegistry;
use forger_sandbox::Sandbox;
use std::sync::Arc;
use std::time::Duration;

pub fn stock_tools(sandbox: Arc<dyn Sandbox>, run_timeout: Duration) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(ReadFile::new(sandbox.clone())));
    reg.register(Arc::new(WriteFile::new(sandbox.clone())));
    reg.register(Arc::new(RunCommand::new(sandbox, run_timeout)));
    reg
}
