use async_trait::async_trait;
use forger_core::{required_path, Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{Sandbox, WritePermit};
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
            description: "Write a text file in the workspace. Sensitive: requires confirmation.".into(),
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

        let decision = self.sandbox.inspect(&path).map_err(|e| ToolError::Failed {
            name: "write_file".into(),
            reason: e.to_string(),
        })?;

        let permit = if let Some(hit) = &decision.denylist {
            if ctx.denylist_override {
                WritePermit::DenylistOverride {
                    confirmed_resolved: decision.resolved.clone(),
                }
            } else {
                return Err(ToolError::Denied {
                    name: "write_file".into(),
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

        self.sandbox
            .write(&path, contents, permit, &ctx.cancel)
            .await
            .map_err(|e| ToolError::Failed {
                name: "write_file".into(),
                reason: e.to_string(),
            })?;
        Ok(format!("wrote {}", decision.resolved.display()))
    }
}
