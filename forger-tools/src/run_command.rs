use async_trait::async_trait;
use forger_core::{Tool, ToolContext, ToolError, ToolSpec};
use forger_sandbox::Sandbox;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub struct RunCommand {
    sandbox: Arc<dyn Sandbox>,
    timeout: Duration,
}

impl RunCommand {
    pub fn new(sandbox: Arc<dyn Sandbox>, timeout: Duration) -> Self {
        Self { sandbox, timeout }
    }
}

#[async_trait]
impl Tool for RunCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "Run a shell command in the workspace. Sensitive: requires confirmation. Hung commands are killed by a timeout supervisor.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command"}
                },
                "required": ["command"]
            }),
        }
    }

    fn is_sensitive(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError> {
        let command = args
            .get("command")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::MalformedArgs("missing string field `command`".into()))?;

        match self.sandbox.run(command, self.timeout, &ctx.cancel).await {
            Ok(out) => Ok(format!(
                "exit {}\nstdout:\n{}\nstderr:\n{}",
                out.exit_code, out.stdout, out.stderr
            )),
            Err(forger_sandbox::SandboxError::Timeout { timeout_ms }) => Err(ToolError::Timeout {
                name: "run_command".into(),
                timeout_ms,
            }),
            Err(forger_sandbox::SandboxError::Cancelled) => Err(ToolError::Cancelled),
            Err(e) => Err(ToolError::Failed {
                name: "run_command".into(),
                reason: e.to_string(),
            }),
        }
    }
}
