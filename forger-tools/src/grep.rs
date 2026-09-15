use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{DirEntry, Sandbox, WritePermit};
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MAX_FILES: usize = 400;
const MAX_MATCHES: usize = 50;
const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist", ".venv"];

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
                    "glob": {"type": "string", "description": "Simple suffix filter, e.g. *.rs"}
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
        let re = Regex::new(pattern).map_err(|e| ToolError::MalformedArgs(format!("invalid regex: {e}")))?;
        let root = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or("."));
        let glob = args.get("glob").and_then(Value::as_str);

        let decision = self
            .sandbox
            .inspect(&root)
            .map_err(|e| failed("grep", e))?;
        let permit = path_permit("grep", &decision, ctx.denylist_override)?;

        let mut files = Vec::new();
        collect_files(
            self.sandbox.as_ref(),
            &root,
            permit.clone(),
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

fn glob_ok(path: &Path, glob: Option<&str>) -> bool {
    let Some(g) = glob else {
        return true;
    };
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(suffix) = g.strip_prefix('*') {
        name.ends_with(suffix)
    } else {
        name == g
    }
}

fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

async fn collect_files(
    sandbox: &dyn Sandbox,
    path: &Path,
    permit: WritePermit,
    glob: Option<&str>,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), ToolError> {
    if out.len() >= MAX_FILES || depth > 12 {
        return Ok(());
    }
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let entries = match sandbox.list(path, permit.clone(), cancel).await {
        Ok(e) => e,
        Err(_) => {
            // Might be a file. Try as a single path.
            if glob_ok(path, glob) {
                out.push(path.to_path_buf());
            }
            return Ok(());
        }
    };
    for DirEntry {
        name,
        path: child,
        is_dir,
    } in entries
    {
        if is_dir {
            if skip_dir(&name) {
                continue;
            }
            Box::pin(collect_files(
                sandbox,
                &child,
                WritePermit::Normal,
                glob,
                depth + 1,
                out,
                cancel,
            ))
            .await?;
        } else if glob_ok(&child, glob) {
            out.push(child);
            if out.len() >= MAX_FILES {
                return Ok(());
            }
        }
    }
    Ok(())
}
