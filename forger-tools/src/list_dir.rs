use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct ListDir {
    sandbox: Arc<dyn Sandbox>,
}

impl ListDir {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "list_dir".into(),
            description: "List files in a workspace directory. Denylisted names (.env, .git, .ssh, credentials) are omitted.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory relative to the workspace", "default": "."}
                }
            }),
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let path = PathBuf::from(path);
        let decision = self
            .sandbox
            .inspect(&path)
            .map_err(|e| failed("list_dir", e))?;
        let permit = path_permit("list_dir", &decision, ctx.denylist_override)?;
        let entries = self
            .sandbox
            .list(&path, permit, &ctx.cancel)
            .await
            .map_err(|e| failed("list_dir", e))?;
        if entries.is_empty() {
            return Ok("(empty)".into());
        }
        let mut lines = Vec::with_capacity(entries.len());
        for e in entries {
            let kind = if e.is_dir { "dir " } else { "file" };
            lines.push(format!("{kind} {}", e.path.display()));
        }
        Ok(lines.join("\n"))
    }
}
