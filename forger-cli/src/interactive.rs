use anyhow::Result;
use async_trait::async_trait;
use forger_core::{
    Agent, AgentEvent, AgentLoop, AgentLoopConfig, ApprovalKind, ApprovalRequest, Approver,
    CancellationToken, Decision, Provider, Session, ToolRegistry, UserTurn,
};
use std::io::{self, BufRead, Write};
use std::sync::Arc;

pub struct PromptApprover {
    pub auto_sensitive: bool,
    pub auto_denylist: bool,
}

#[async_trait]
impl Approver for PromptApprover {
    async fn approve(
        &self,
        request: &ApprovalRequest,
    ) -> Result<Decision, forger_core::ApprovalError> {
        match request.kind {
            ApprovalKind::SensitiveTool if self.auto_sensitive => return Ok(Decision::Allow),
            ApprovalKind::DenylistOverride if self.auto_denylist => return Ok(Decision::Allow),
            _ => {}
        }
        let prompt = match request.kind {
            ApprovalKind::SensitiveTool => format!(
                "Allow sensitive tool `{}`?\n  {}\n[y/N] ",
                request.tool_name, request.reason
            ),
            ApprovalKind::DenylistOverride => format!(
                "DENYLIST override requested for `{}` ({})\n\
                 This is independent of sensitive-tool confirmation.\n\
                 Type ALLOW DENIED PATH to proceed, anything else to deny: ",
                request
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                request.reason
            ),
        };
        let kind = request.kind;
        let answer = tokio::task::spawn_blocking(move || {
            eprint!("{prompt}");
            let _ = io::stderr().flush();
            let mut line = String::new();
            let _ = io::stdin().read_line(&mut line);
            line
        })
        .await
        .map_err(|e| forger_core::ApprovalError::Failed(e.to_string()))?;

        let ok = match kind {
            ApprovalKind::SensitiveTool => {
                matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
            }
            ApprovalKind::DenylistOverride => answer.trim() == "ALLOW DENIED PATH",
        };
        if ok {
            Ok(Decision::Allow)
        } else {
            Ok(Decision::Deny {
                reason: "user denied".into(),
            })
        }
    }
}

pub async fn repl(
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    approver: Arc<dyn Approver>,
    config: AgentLoopConfig,
) -> Result<()> {
    let agent = AgentLoop::new(provider, tools, approver, config);
    let mut session = Session::new();
    println!(
        "Forger 0.1 — session {}. Ctrl+C cancels a turn; /quit exits.",
        session.id
    );
    println!("Commands: /quit  /reset  /help");
    println!("(no API key → MockProvider. Set FORGER_API_KEY for openai-compat.)");

    let stdin = io::stdin();
    loop {
        print!("forger> ");
        io::stdout().flush()?;
        let mut line = String::new();
        let n = stdin.lock().read_line(&mut line)?;
        if n == 0 {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match line {
            "/quit" | "/exit" => break,
            "/reset" => {
                session = Session::new();
                println!("new session {}", session.id);
                continue;
            }
            "/help" => {
                println!("Type a task. /reset starts a new session. /quit exits.");
                println!("Ctrl+C during a turn cancels without corrupting the session.");
                continue;
            }
            _ => {}
        }

        let cancel = CancellationToken::new();
        let cancel_bg = cancel.clone();
        let ctrlc = tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            cancel_bg.cancel();
        });

        let mut sink = |ev: AgentEvent| match ev {
            AgentEvent::TextDelta { text } => {
                print!("{text}");
                let _ = io::stdout().flush();
            }
            AgentEvent::ToolCall { call } => {
                eprintln!("\n→ {} {}", call.name, call.arguments);
            }
            AgentEvent::ToolResult { name, output, .. } => {
                let preview: String = output.chars().take(240).collect();
                eprintln!("← {name}: {preview}");
            }
            AgentEvent::Cancelled => eprintln!("\n(cancelled — session kept)"),
            AgentEvent::Warning { message } => eprintln!("warning: {message}"),
            _ => {}
        };

        match agent
            .run_turn(
                &mut session,
                UserTurn {
                    text: line.to_string(),
                },
                cancel,
                &mut sink,
            )
            .await
        {
            Ok(_) => println!(),
            Err(e) => eprintln!("\nerror: {e}"),
        }
        ctrlc.abort();
    }
    Ok(())
}
