//! Read-only git via the sandbox. Commit stays on `run_command` (sensitive).
//!
//! Paths passed to `diff` are rejected if they look like shell metacharacters
//! so the action cannot become an arbitrary command.

use crate::failed;
use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

const GIT_ENV: &str =
    "GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_TERMINAL_PROMPT=0";

pub struct Git {
    sandbox: Arc<dyn Sandbox>,
    timeout: Duration,
}

impl Git {
    pub fn new(sandbox: Arc<dyn Sandbox>, timeout: Duration) -> Self {
        Self { sandbox, timeout }
    }
}

fn shell_safe_relpath(path: &str) -> Result<&str, ToolError> {
    if path.is_empty() || path.len() > 512 {
        return Err(ToolError::MalformedArgs("invalid `path`".into()));
    }
    if path.starts_with('-') {
        return Err(ToolError::MalformedArgs(
            "`path` must not start with `-` (would be parsed as a git flag)".into(),
        ));
    }
    if path.contains("..") {
        return Err(ToolError::MalformedArgs(
            "`path` must not contain `..`".into(),
        ));
    }
    let ok = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '+'));
    if !ok {
        return Err(ToolError::MalformedArgs(
            "`path` may only contain letters, digits, `.`, `_`, `-`, `+`, `/`".into(),
        ));
    }
    Ok(path)
}

#[async_trait]
impl Tool for Git {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "git".into(),
            description: "Read-only git: status, diff, log. Commit / push / reset go through run_command (sensitive). Runs inside the sandbox.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["status", "diff", "log"],
                        "description": "status | diff | log"
                    },
                    "path": {"type": "string", "description": "Optional path for diff"},
                    "limit": {"type": "integer", "description": "Commit count for log (default 10, max 50)"}
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `action`".into()))?;

        let command = match action {
            "status" => format!("{GIT_ENV} git status --short --branch"),
            "diff" => match args.get("path").and_then(Value::as_str) {
                Some(p) if !p.is_empty() => {
                    let p = shell_safe_relpath(p)?;
                    format!("{GIT_ENV} git diff -- {p}")
                }
                _ => format!("{GIT_ENV} git diff"),
            },
            "log" => {
                let n = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(10)
                    .clamp(1, 50);
                format!("{GIT_ENV} git log -n {n} --oneline")
            }
            other => {
                return Err(ToolError::MalformedArgs(format!(
                    "unknown git action `{other}`; use status, diff, or log"
                )));
            }
        };

        match self.sandbox.run(&command, self.timeout, &ctx.cancel).await {
            Ok(out) => Ok(format!(
                "exit {}\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            )),
            Err(forger_sandbox::SandboxError::Timeout { timeout_ms }) => Err(ToolError::Timeout {
                name: "git".into(),
                timeout_ms,
            }),
            Err(forger_sandbox::SandboxError::Cancelled) => Err(ToolError::Cancelled),
            Err(e) => Err(failed("git", e)),
        }
    }
}
