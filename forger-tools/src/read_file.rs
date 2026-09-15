use async_trait::async_trait;
use forger_core::{required_path, Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ReadFile {
    sandbox: Arc<dyn Sandbox>,
}

impl ReadFile {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "read_file".into(),
            description: "Read a text file in the workspace.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the workspace"}
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = required_path(&args)?;
        match self.sandbox.read(&path, &ctx.cancel).await {
            Ok(s) => Ok(s),
            Err(e) => Err(ToolError::Failed {
                name: "read_file".into(),
                reason: e.to_string(),
            }),
        }
    }
}
