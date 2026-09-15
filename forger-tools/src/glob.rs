use crate::walk::{collect_files, MAX_FILES};
use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

pub struct Glob {
    sandbox: Arc<dyn Sandbox>,
}

impl Glob {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "glob".into(),
            description: "Find files by glob (e.g. **/*.rs, src/*.toml). Skips denylisted names and build dirs. Prefer this over run_command find/ls.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern"},
                    "path": {"type": "string", "description": "Directory to search", "default": "."}
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let pattern = args
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `pattern`".into()))?;
        if pattern.is_empty() {
            return Err(ToolError::MalformedArgs(
                "`pattern` must not be empty".into(),
            ));
        }
        let root = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or("."));
        let decision = self.sandbox.inspect(&root).map_err(|e| failed("glob", e))?;
        let permit = path_permit("glob", &decision, ctx.denylist_override)?;

        let mut files = Vec::new();
        collect_files(
            self.sandbox.as_ref(),
            &root,
            permit,
            Some(pattern),
            0,
            &mut files,
            &ctx.cancel,
        )
        .await?;
        files.sort();
        if files.is_empty() {
            return Ok("no matches".into());
        }
        let truncated = files.len() >= MAX_FILES;
        let mut lines: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
        if truncated {
            lines.push(format!("… truncated at {MAX_FILES} files"));
        }
        Ok(lines.join("\n"))
    }
}
