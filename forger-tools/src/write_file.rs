use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{required_path, Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct WriteFile {
    sandbox: Arc<dyn Sandbox>,
}

impl WriteFile {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "write_file".into(),
            description: "Create or overwrite a text file. Sensitive: requires confirmation. Prefer edit_file for existing files.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "contents": {"type": "string"}
                },
                "required": ["path", "contents"]
            }),
        }
    }

    fn is_sensitive(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = required_path(&args)?;
        let contents = args
            .get("contents")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `contents`".into()))?;

        let decision = self
            .sandbox
            .inspect(&path)
            .map_err(|e| failed("write_file", e))?;
        let permit = path_permit("write_file", &decision, ctx.denylist_override)?;
        self.sandbox
            .write(&path, contents, permit, &ctx.cancel)
            .await
            .map_err(|e| failed("write_file", e))?;
        Ok(format!("wrote {}", decision.resolved.display()))
    }
}
