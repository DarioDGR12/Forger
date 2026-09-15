use crate::{failed, path_permit};
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::{DirEntry, Sandbox, WritePermit};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
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
            description: "Search file contents in the workspace with a regex. Skips denylisted paths, .gitignore matches, and common build dirs.".into(),
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
        let re = Regex::new(pattern)
            .map_err(|e| ToolError::MalformedArgs(format!("invalid regex: {e}")))?;
        let root = PathBuf::from(args.get("path").and_then(Value::as_str).unwrap_or("."));
        let glob = args.get("glob").and_then(Value::as_str);

        let decision = self.sandbox.inspect(&root).map_err(|e| failed("grep", e))?;
        let permit = path_permit("grep", &decision, ctx.denylist_override)?;
        let gitignore = load_gitignore(self.sandbox.workspace());

        let mut files = Vec::new();
        let walk = CollectCtx {
            sandbox: self.sandbox.as_ref(),
            glob,
            gitignore: gitignore.as_ref(),
            cancel: &ctx.cancel,
        };
        collect_files(&walk, &root, permit, 0, &mut files).await?;

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

fn load_gitignore(workspace: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(workspace);
    let gi = workspace.join(".gitignore");
    if gi.is_file() {
        builder.add(&gi);
    }
    builder.build().ok()
}

fn ignored(gi: Option<&Gitignore>, path: &Path, is_dir: bool) -> bool {
    gi.map(|g| g.matched(path, is_dir).is_ignore())
        .unwrap_or(false)
}

struct CollectCtx<'a> {
    sandbox: &'a dyn Sandbox,
    glob: Option<&'a str>,
    gitignore: Option<&'a Gitignore>,
    cancel: &'a tokio_util::sync::CancellationToken,
}

async fn collect_files(
    walk: &CollectCtx<'_>,
    path: &Path,
    permit: WritePermit,
    depth: usize,
    out: &mut Vec<PathBuf>,
) -> Result<(), ToolError> {
    if out.len() >= MAX_FILES || depth > 12 {
        return Ok(());
    }
    if walk.cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let entries = match walk.sandbox.list(path, permit, walk.cancel).await {
        Ok(e) => e,
        Err(_) => {
            if glob_ok(path, walk.glob) && !ignored(walk.gitignore, path, false) {
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
            if skip_dir(&name) || ignored(walk.gitignore, &child, true) {
                continue;
            }
            Box::pin(collect_files(
                walk,
                &child,
                WritePermit::Normal,
                depth + 1,
                out,
            ))
            .await?;
        } else if glob_ok(&child, walk.glob) && !ignored(walk.gitignore, &child, false) {
            out.push(child);
            if out.len() >= MAX_FILES {
                return Ok(());
            }
        }
    }
    Ok(())
}
