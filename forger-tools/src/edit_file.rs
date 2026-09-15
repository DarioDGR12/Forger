use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{required_path, Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct EditFile {
    sandbox: Arc<dyn Sandbox>,
}

impl EditFile {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "edit_file".into(),
            description: "Replace exact text in an existing file. Sensitive. Fails if old_string is missing or not unique (unless replace_all).".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean", "default": false}
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn is_sensitive(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let path = required_path(&args)?;
        let old = args
            .get("old_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `old_string`".into()))?;
        let new = args
            .get("new_string")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `new_string`".into()))?;
        if old.is_empty() {
            return Err(ToolError::MalformedArgs(
                "`old_string` must not be empty".into(),
            ));
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let decision = self
            .sandbox
            .inspect(&path)
            .map_err(|e| failed("edit_file", e))?;
        let permit = path_permit("edit_file", &decision, ctx.denylist_override)?;
        let contents = self
            .sandbox
            .read(&path, permit.clone(), &ctx.cancel)
            .await
            .map_err(|e| failed("edit_file", e))?;

        let count = contents.matches(old).count();
        if count == 0 {
            return Err(ToolError::Failed {
                name: "edit_file".into(),
                reason: "old_string not found in file".into(),
            });
        }
        if count > 1 && !replace_all {
            return Err(ToolError::Failed {
                name: "edit_file".into(),
                reason: format!(
                    "old_string occurs {count} times; pass replace_all=true or provide a unique snippet"
                ),
            });
        }
        let updated = if replace_all {
            contents.replace(old, new)
        } else {
            contents.replacen(old, new, 1)
        };
        self.sandbox
            .write(&path, &updated, permit, &ctx.cancel)
            .await
            .map_err(|e| failed("edit_file", e))?;
        Ok(format!(
            "edited {} ({} replacement{})",
            decision.resolved.display(),
            count,
            if count == 1 { "" } else { "s" }
        ))
    }
}
