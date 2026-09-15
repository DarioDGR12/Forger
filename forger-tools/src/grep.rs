use crate::walk::collect_files;
use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{Sandbox, WritePermit};
use regex::Regex;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_MATCHES: usize = 50;

pub struct Grep {
    sandbox: Arc<dyn Sandbox>,
}

impl Grep {
    pub fn new(sandbox: Arc<dyn Sandbox>) -> Self {
        Self { sandbox }
    }
}

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "grep".into(),
            description: "Search file contents in the workspace with a regex. Skips denylisted paths and common build dirs.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "File or directory to search", "default": "."},
                    "glob": {"type": "string", "description": "Glob filter, e.g. *.rs or **/*.toml"}
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
        let re = Regex::new(pattern)
            .map_err(|e| ToolError::MalformedArgs(format!("invalid regex: {e}")))?;
        let root = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or("."));
        let glob = args.get("glob").and_then(Value::as_str);

        let decision = self.sandbox.inspect(&root).map_err(|e| failed("grep", e))?;
        let permit = path_permit("grep", &decision, ctx.denylist_override)?;

        let mut files = Vec::new();
        collect_files(
            self.sandbox.as_ref(),
            &root,
            permit,
            glob,
            0,
            &mut files,
            &ctx.cancel,
        )
        .await?;

        let mut hits = Vec::new();
        for file in files {
            if ctx.cancel.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let text = match self
                .sandbox
                .read(&file, WritePermit::Normal, &ctx.cancel)
                .await
            {
                Ok(t) => t,
                Err(_) => continue,
            };
            if text.contains('\0') {
                continue;
            }
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let clipped: String = line.chars().take(200).collect();
                    hits.push(format!("{}:{}:{clipped}", file.display(), i + 1));
                    if hits.len() >= MAX_MATCHES {
                        hits.push(format!("… truncated at {MAX_MATCHES} matches"));
                        return Ok(hits.join("\n"));
                    }
                }
            }
        }
        if hits.is_empty() {
            Ok("no matches".into())
        } else {
            Ok(hits.join("\n"))
        }
    }
}
