use serde_json::Value;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApprovalError {
    #[error("approval cancelled")]
    Cancelled,
    #[error("approval failed: {0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalKind {
    /// Generic sensitive-tool confirmation (write_file, run_command, …).
    SensitiveTool,
    /// Independent second layer: denylist path (.env / .git / credentials / .ssh).
    DenylistOverride,
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub kind: ApprovalKind,
    pub tool_name: String,
    pub arguments: Value,
    pub path: Option<PathBuf>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

use async_trait::async_trait;

#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: &ApprovalRequest) -> Result<Decision, ApprovalError>;
}

/// Test/headless approver. Denylist overrides stay denied unless explicitly enabled —
/// `--yes` must not silently punch through the denylist.
#[derive(Debug, Clone, Default)]
pub struct AutoApprover {
    pub allow_sensitive: bool,
    pub allow_denylist: bool,
}

#[async_trait]
impl Approver for AutoApprover {
    async fn approve(&self, request: &ApprovalRequest) -> Result<Decision, ApprovalError> {
        let allowed = match request.kind {
            ApprovalKind::SensitiveTool => self.allow_sensitive,
            ApprovalKind::DenylistOverride => self.allow_denylist,
        };
        if allowed {
            Ok(Decision::Allow)
        } else {
            Ok(Decision::Deny {
                reason: format!(
                    "auto-approver denied {:?} for tool `{}`",
                    request.kind, request.tool_name
                ),
            })
        }
    }
}
