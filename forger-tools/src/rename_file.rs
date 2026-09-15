use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{PathDecision, Sandbox, WritePermit};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct RenameFile {
    sandbox: Arc<dyn Sandbox>,
}

impl RenameFile {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

fn required_named_path(args: &Value, field: &str) -> Result<PathBuf, ToolError> {
    let path = args
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::MalformedArgs(format!("missing string field `{field}`")))?;
    Ok(PathBuf::from(path))
}

fn permit_for_pair(
    from: &PathDecision,
    to: &PathDecision,
    denylist_override: bool,
) -> Result<WritePermit, ToolError> {
    // Prefer the destination hit: that is the usual `config` → `.env` case.
    // Source is still enforced inside Sandbox::rename.
    let decision = if to.denylist.is_some() { to } else { from };
    path_permit("rename_file", decision, denylist_override)
}

#[async_trait]
impl Tool for RenameFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "rename_file".into(),
            description: "Rename a file in the workspace. Sensitive. Destination and source both go through the denylist (canonical path).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"}
                },
                "required": ["from", "to"]
            }),
        }
    }

    fn is_sensitive(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let from = required_named_path(&args, "from")?;
        let to = required_named_path(&args, "to")?;
        let from_d = self
            .sandbox
            .inspect(&from)
            .map_err(|e| failed("rename_file", e))?;
        let to_d = self
            .sandbox
            .inspect(&to)
            .map_err(|e| failed("rename_file", e))?;
        let permit = permit_for_pair(&from_d, &to_d, ctx.denylist_override)?;
        self.sandbox
            .rename(&from, &to, permit, &ctx.cancel)
            .await
            .map_err(|e| failed("rename_file", e))?;
        Ok(format!(
            "renamed {} -> {}",
            from_d.resolved.display(),
            to_d.resolved.display()
        ))
    }
}
