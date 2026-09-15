use async_trait::async_trait;
use forger_core::{required_path, Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{Sandbox, WritePermit};
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
            description: "Read a text file in the workspace. Not a sensitive tool: confirmation is the independent denylist layer when the resolved path is blocked.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path relative to the workspace"}
                },
                "required": ["path"]
            }),
        }
    }

    /// Intentionally `false`. Secret files are gated by the sandbox denylist
    /// (second layer), not by treating every read as a sensitive tool.
    fn is_sensitive(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = required_path(&args)?;
        let decision = self.sandbox.inspect(&path).map_err(|e| ToolError::Failed {
            name: "read_file".into(),
            reason: e.to_string(),
        })?;

        let permit = if let Some(hit) = &decision.denylist {
            if ctx.denylist_override {
                WritePermit::DenylistOverride {
                    confirmed_resolved: decision.resolved.clone(),
                }
            } else {
                return Err(ToolError::Denied {
                    name: "read_file".into(),
                    reason: format!(
                        "denylist blocked `{}` (pattern `{}`); this is independent of sensitive-tool confirmation",
                        decision.resolved.display(),
                        hit.pattern
                    ),
                });
            }
        } else {
            WritePermit::Normal
        };

        match self.sandbox.read(&path, permit, &ctx.cancel).await {
            Ok(s) => Ok(s),
            Err(e) => Err(crate::sandbox_to_tool_error("read_file", e)),
        }
    }
}
