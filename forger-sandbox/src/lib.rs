//! Execution sandbox.
//!
//! Two independent layers:
//! 1. Userspace denylist on the *resolved final path* (follows symlinks and
//!    `..`) for every read/write. This is the `.env` / `.git` / `credentials`
//!    / `.ssh` policy. It is not skipped by the generic "sensitive tool"
//!    confirmation — that is a different [`forger_core::ApprovalKind`].
//! 2. Landlock confinement of `run_command` children on Linux, plus a timeout
//!    supervisor in the parent so a hung process cannot stall the agent loop.
//!
//! Accepted, documented risks — see [`crate::risks`]:
//! - Filename denylist cannot see a later `mv innocent .env` performed inside
//!   a shell. Writes through [`Sandbox::write`] always re-resolve the final
//!   path; shell renames remain an accepted gap.
//! - Linux kernels without Landlock ABI 6 (`SCOPE_SIGNAL`, 6.12+) can let a
//!   sandboxed process `kill -9` other same-user processes, including Forger.
//!   We **warn at runtime** and never pretend this is closed.

pub mod denylist;
pub mod exec;
pub mod path;
pub mod risks;

#[cfg(target_os = "linux")]
pub mod landlock_linux;

use async_trait::async_trait;
use forger_core::plugin::{Plugin, PluginId};
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("path `{path}` is outside the workspace")]
    OutsideWorkspace { path: String },
    #[error("denylist blocked `{path}` ({pattern})")]
    Denylist { path: String, pattern: &'static str },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("command timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },
    #[error("command cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

#[derive(Debug, Clone)]
pub struct DenylistHit {
    pub path: PathBuf,
    pub pattern: &'static str,
}

#[derive(Debug, Clone)]
pub struct PathDecision {
    pub resolved: PathBuf,
    pub denylist: Option<DenylistHit>,
}

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Permit that must match the *resolved* path. Approving `.env` does not
/// authorize a write to a different resolved target.
#[derive(Debug, Clone)]
pub enum WritePermit {
    Normal,
    DenylistOverride { confirmed_resolved: PathBuf },
}

#[async_trait]
pub trait Sandbox: Plugin {
    fn workspace(&self) -> &Path;

    fn inspect(&self, requested: &Path) -> Result<PathDecision, SandboxError>;

    async fn read(
        &self,
        requested: &Path,
        permit: WritePermit,
        cancel: &CancellationToken,
    ) -> Result<String, SandboxError>;

    async fn write(
        &self,
        requested: &Path,
        contents: &str,
        permit: WritePermit,
        cancel: &CancellationToken,
    ) -> Result<(), SandboxError>;

    async fn run(
        &self,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<CommandOutput, SandboxError>;

    fn landlock_warning(&self) -> Option<String>;
}

pub struct FsSandbox {
    workspace: PathBuf,
    landlock_warning: Option<String>,
}

impl FsSandbox {
    pub fn new(workspace: impl Into<PathBuf>) -> Result<Self, SandboxError> {
        let workspace = workspace.into();
        std::fs::create_dir_all(&workspace)?;
        let workspace = std::fs::canonicalize(&workspace)?;
        let landlock_warning = risks::signal_scope_warning(risks::probe_landlock_abi());
        if let Some(ref w) = landlock_warning {
            tracing::warn!("{w}");
        } else {
            tracing::info!("Landlock ABI 6+ detected: SCOPE_SIGNAL is available");
        }
        Ok(Self {
            workspace,
            landlock_warning,
        })
    }

    fn decide(&self, requested: &Path) -> Result<PathDecision, SandboxError> {
        let resolved = path::resolve_final_path(&self.workspace, requested)?;
        if !path::is_inside(&self.workspace, &resolved) {
            return Err(SandboxError::OutsideWorkspace {
                path: resolved.display().to_string(),
            });
        }
        let denylist = denylist::check(&resolved);
        Ok(PathDecision { resolved, denylist })
    }

    fn enforce_denylist(
        &self,
        decision: &PathDecision,
        permit: &WritePermit,
    ) -> Result<(), SandboxError> {
        let Some(hit) = &decision.denylist else {
            return Ok(());
        };
        match permit {
            WritePermit::DenylistOverride { confirmed_resolved }
                if path::same_path(confirmed_resolved, &decision.resolved) =>
            {
                tracing::warn!(
                    path = %decision.resolved.display(),
                    pattern = hit.pattern,
                    "denylist override confirmed for resolved path"
                );
                Ok(())
            }
            _ => Err(SandboxError::Denylist {
                path: decision.resolved.display().to_string(),
                pattern: hit.pattern,
            }),
        }
    }
}

impl Plugin for FsSandbox {
    fn id(&self) -> PluginId {
        PluginId("fs-sandbox")
    }
}

#[async_trait]
impl Sandbox for FsSandbox {
    fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn inspect(&self, requested: &Path) -> Result<PathDecision, SandboxError> {
        self.decide(requested)
    }

    async fn read(
        &self,
        requested: &Path,
        permit: WritePermit,
        cancel: &CancellationToken,
    ) -> Result<String, SandboxError> {
        if cancel.is_cancelled() {
            return Err(SandboxError::Cancelled);
        }
        let decision = self.decide(requested)?;
        self.enforce_denylist(&decision, &permit)?;
        Ok(tokio::fs::read_to_string(&decision.resolved).await?)
    }

    async fn write(
        &self,
        requested: &Path,
        contents: &str,
        permit: WritePermit,
        cancel: &CancellationToken,
    ) -> Result<(), SandboxError> {
        if cancel.is_cancelled() {
            return Err(SandboxError::Cancelled);
        }
        let decision = self.decide(requested)?;
        self.enforce_denylist(&decision, &permit)?;
        if let Some(parent) = decision.resolved.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&decision.resolved, contents).await?;
        Ok(())
    }

    async fn run(
        &self,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<CommandOutput, SandboxError> {
        exec::run_command(&self.workspace, command, timeout, cancel, self.landlock_warning.as_deref())
            .await
    }

    fn landlock_warning(&self) -> Option<String> {
        self.landlock_warning.clone()
    }
}
