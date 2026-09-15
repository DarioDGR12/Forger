use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use forger_core::approval::AutoApprover;
use forger_core::{
    Agent, AgentEvent, AgentLoop, AgentLoopConfig, Approver, CancellationToken, QualityConfig,
    QualityRunner, Session, TurnOutcome, UserTurn,
};
use forger_providers::{MockProvider, OpenAiCompatConfig, OpenAiCompatProvider};
use forger_sandbox::{FsSandbox, Sandbox};
use forger_tools::stock_tools;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::EnvFilter;

mod interactive;

#[derive(Parser, Debug)]
#[command(
    name = "forger",
    about = "Forger — modular coding agent (Rust). Provider, tools, sandbox, UI, and the agent loop are plugins.",
    version
)]
struct Cli {
    /// One-shot user message (non-interactive)
    #[arg(short, long)]
    message: Option<String>,

    /// Auto-approve sensitive tools. Does NOT override the .env/.git/.ssh/credentials denylist.
    #[arg(long)]
    yes: bool,

    /// Second, explicit confirmation to skip the path denylist. Independent of --yes.
    #[arg(long)]
    allow_denied_paths: bool,

    /// Workspace directory
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// mock | openai-compat (default: openai-compat if an API key is set, else mock)
    #[arg(long, value_enum)]
    provider: Option<ProviderKind>,

    #[arg(long)]
    model: Option<String>,

    #[arg(long)]
    base_url: Option<String>,

    /// Run 2–3 candidate loops and pick the best (token cost scales with N; default N=2)
    #[arg(long)]
    quality: bool,

    #[arg(long, default_value_t = 2)]
    quality_n: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Local web UI + SSE. Loopback only, no authentication.
    Serve {
        #[arg(long, default_value_t = 7420)]
        port: u16,
        /// Bind address. Non-loopback is refused: there is no auth, so exposing
        /// this is local RCE. We will not ship a half-implemented token check.
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProviderKind {
    Mock,
    OpenaiCompat,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("forger=info".parse()?))
        .with_writer(io::stderr)
        .init();

    let cli = Cli::parse();

    if let Some(Commands::Serve { port, bind }) = cli.command {
        if !is_loopback(&bind) {
            bail!(
                "refusing to bind {bind}:{port}. forger serve has NO authentication; \
                 exposing it off loopback is local RCE. Auth will be a real session \
                 token/cookie or it will stay unimplemented — not a half measure. \
                 Use 127.0.0.1."
            );
        }
        let addr: std::net::SocketAddr = format!("{bind}:{port}").parse()?;
        return forger_server::run(addr, cli.workspace).await;
    }

    let workspace = std::fs::canonicalize(&cli.workspace)
        .or_else(|_| {
            std::fs::create_dir_all(&cli.workspace)?;
            std::fs::canonicalize(&cli.workspace)
        })
        .with_context(|| format!("workspace {}", cli.workspace.display()))?;

    let sandbox = Arc::new(FsSandbox::new(&workspace)?);
    if let Some(w) = sandbox.landlock_warning() {
        eprintln!("warning: {w}");
    }

    let tools = stock_tools(sandbox.clone(), Duration::from_secs(30));
    let provider = build_provider(cli.provider, cli.model, cli.base_url)?;
    let approver: Arc<dyn Approver> = if cli.message.is_some() {
        Arc::new(AutoApprover {
            allow_sensitive: cli.yes,
            allow_denylist: cli.allow_denied_paths,
        })
    } else {
        Arc::new(interactive::PromptApprover {
            auto_sensitive: cli.yes,
            auto_denylist: cli.allow_denied_paths,
        })
    };

    let config = AgentLoopConfig {
        workspace: workspace.clone(),
        ..AgentLoopConfig::default()
    };

    if cli.quality {
        if cli.quality_n == 0 || cli.quality_n > 3 {
            bail!("--quality-n must be 1..=3 (default 2; extra candidates multiply token spend)");
        }
        let task = cli
            .message
            .clone()
            .context("--quality requires --message")?;
        let qcfg = QualityConfig {
            candidates: cli.quality_n,
        };
        let make = {
            let provider = provider.clone();
            let tools = tools.clone();
            let approver = approver.clone();
            let config = config.clone();
            move || AgentLoop::new(provider.clone(), tools.clone(), approver.clone(), config.clone())
        };
        let runner = QualityRunner::new(provider, qcfg);
        let cancel = CancellationToken::new();
        ctrlc_cancel(cancel.clone());
        let (session, report) = runner.run(make, &task, cancel).await?;
        println!(
            "quality: winner candidate {} / {}",
            report.winner_index, report.candidates.len()
        );
        for c in &report.candidates {
            println!("  [{}] score {} — {}", c.index, c.score, c.rationale);
        }
        if let Some(text) = session.last_assistant_text() {
            println!("{text}");
        }
        return Ok(());
    }

    if let Some(msg) = cli.message {
        let agent = AgentLoop::new(provider, tools, approver, config);
        let mut session = Session::new();
        let cancel = CancellationToken::new();
        ctrlc_cancel(cancel.clone());
        let mut sink = |ev: AgentEvent| match ev {
            AgentEvent::TextDelta { text } => {
                print!("{text}");
                let _ = io::stdout().flush();
            }
            AgentEvent::ToolCall { call } => eprintln!("\n→ tool {} {}", call.name, call.arguments),
            AgentEvent::ToolResult { name, output, .. } => {
                let preview: String = output.chars().take(200).collect();
                eprintln!("← {name}: {preview}");
            }
            AgentEvent::Warning { message } => eprintln!("warning: {message}"),
            AgentEvent::Cancelled => eprintln!("\n(cancelled)"),
            _ => {}
        };
        let outcome = agent
            .run_turn(
                &mut session,
                UserTurn { text: msg },
                cancel,
                &mut sink,
            )
            .await?;
        if outcome == TurnOutcome::StepLimit {
            eprintln!("stopped: max turns reached (tool results from the last turn were kept)");
        }
        println!();
        return Ok(());
    }

    interactive::repl(provider, tools, approver, config).await
}

fn is_loopback(bind: &str) -> bool {
    matches!(bind, "127.0.0.1" | "::1" | "localhost")
}

fn build_provider(
    kind: Option<ProviderKind>,
    model: Option<String>,
    base_url: Option<String>,
) -> Result<Arc<dyn forger_core::Provider>> {
    let kind = kind.unwrap_or_else(|| {
        if OpenAiCompatConfig::from_env().is_some() {
            ProviderKind::OpenaiCompat
        } else {
            ProviderKind::Mock
        }
    });
    match kind {
        ProviderKind::Mock => Ok(Arc::new(MockProvider::single_text(
            "MockProvider: set FORGER_API_KEY to use an OpenAI-compatible backend.",
        ))),
        ProviderKind::OpenaiCompat => {
            let mut cfg = OpenAiCompatConfig::from_env().context(
                "openai-compat requires FORGER_API_KEY (or OPENAI_API_KEY / DEEPSEEK_API_KEY)",
            )?;
            if let Some(m) = model {
                cfg.model = m;
            }
            if let Some(u) = base_url {
                cfg.base_url = u;
            }
            Ok(Arc::new(OpenAiCompatProvider::new(cfg)))
        }
    }
}

fn ctrlc_cancel(cancel: CancellationToken) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        cancel.cancel();
    });
}
