//! Local HTTP + SSE surface. **No authentication.** Bind loopback only.
//!
//! Exposing this on a non-loopback interface is local RCE (the agent can run
//! commands). We will not add a half-baked token check; auth is either a real
//! session cookie/token or it stays unimplemented.

use anyhow::{bail, Result};
use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse};
use axum::routing::{get, post};
use axum::{Json, Router};
use forger_core::approval::AutoApprover;
use forger_core::{
    Agent, AgentEvent, AgentLoop, AgentLoopConfig, CancellationToken, Session, UserTurn,
};
use forger_providers::{MockProvider, OpenAiCompatConfig, OpenAiCompatProvider};
use forger_sandbox::{FsSandbox, Sandbox};
use forger_tools::stock_tools;
use serde::Deserialize;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

const NO_AUTH_BANNER: &str = "\
forger-server is listening with NO authentication. This is acceptable only \
because the bind address is loopback. Do not proxy or expose this port. \
A real auth design (session token/cookie) is future work — we will not ship a \
half-implemented check just to look like we have one.";

#[derive(Clone)]
struct AppState {
    workspace: PathBuf,
    sessions: Arc<Mutex<std::collections::HashMap<String, Session>>>,
}

pub async fn run(addr: SocketAddr, workspace: PathBuf) -> Result<()> {
    if !addr.ip().is_loopback() {
        bail!("refusing to listen on {addr}. {NO_AUTH_BANNER}");
    }
    let workspace = std::fs::canonicalize(&workspace).unwrap_or(workspace);
    tracing::warn!("{NO_AUTH_BANNER}");
    eprintln!("warning: {NO_AUTH_BANNER}");
    eprintln!("Forger UI: http://{addr}/");

    let state = AppState {
        workspace,
        sessions: Arc::new(Mutex::new(std::collections::HashMap::new())),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/v1/session", post(new_session))
        .route("/v1/turn", post(turn_sse))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[derive(Deserialize)]
struct NewSessionBody {
    #[serde(default)]
    resume: Option<String>,
}

async fn new_session(
    State(state): State<AppState>,
    Json(body): Json<NewSessionBody>,
) -> impl IntoResponse {
    if let Some(id) = body.resume {
        Json(serde_json::json!({"id": id}))
    } else {
        let s = Session::new();
        let id = s.id.to_string();
        state.sessions.lock().await.insert(id.clone(), s);
        Json(serde_json::json!({"id": id}))
    }
}

#[derive(Deserialize)]
struct TurnBody {
    session_id: String,
    message: String,
    #[serde(default)]
    yes: bool,
    #[serde(default)]
    allow_denied_paths: bool,
    /// Optional turn budget. Defaults to AgentLoopConfig's 20.
    #[serde(default)]
    max_turns: Option<usize>,
}

async fn turn_sse(
    State(state): State<AppState>,
    Json(body): Json<TurnBody>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let workspace = state.workspace.clone();
    let sessions = state.sessions.clone();
    tokio::spawn(async move {
        let result = run_one_turn(workspace, sessions, body, tx.clone()).await;
        if let Err(e) = result {
            let _ =
                tx.send(serde_json::json!({"type":"error","message": e.to_string()}).to_string());
        }
        let _ = tx.send(serde_json::json!({"type":"done"}).to_string());
    });

    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|data| (Ok(Event::default().data(data)), rx))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn run_one_turn(
    workspace: PathBuf,
    sessions: Arc<Mutex<std::collections::HashMap<String, Session>>>,
    body: TurnBody,
    tx: mpsc::UnboundedSender<String>,
) -> anyhow::Result<()> {
    let sandbox = Arc::new(FsSandbox::new(&workspace)?);
    if let Some(w) = sandbox.landlock_warning() {
        let _ = tx.send(serde_json::json!({"type":"warning","message": w}).to_string());
    }
    let tools = stock_tools(sandbox, Duration::from_secs(30));
    let provider: Arc<dyn forger_core::Provider> = match OpenAiCompatConfig::from_env() {
        Some(cfg) => Arc::new(OpenAiCompatProvider::new(cfg)),
        None => Arc::new(MockProvider::single_text(
            "MockProvider: set FORGER_API_KEY for a real model.",
        )),
    };
    let approver = Arc::new(AutoApprover {
        allow_sensitive: body.yes,
        allow_denylist: body.allow_denied_paths,
    });
    let mut config = AgentLoopConfig {
        workspace,
        ..AgentLoopConfig::default()
    };
    if let Some(n) = body.max_turns {
        config = config.with_max_turns(n);
    }
    let agent = AgentLoop::new(provider, tools, approver, config);

    let mut guard = sessions.lock().await;
    let session = guard
        .entry(body.session_id.clone())
        .or_insert_with(Session::new);
    // Clone out so we don't hold the lock across the turn (cancel must not
    // leave a half-written session in the map).
    let mut local = session.clone();
    drop(guard);

    let cancel = CancellationToken::new();
    let mut sink = |ev: AgentEvent| {
        let v = serde_json::to_value(&ev).unwrap_or_else(|_| serde_json::json!({"type":"unknown"}));
        let _ = tx.send(v.to_string());
    };
    let outcome = agent
        .run_turn(
            &mut local,
            UserTurn { text: body.message },
            cancel,
            &mut sink,
        )
        .await;

    let mut guard = sessions.lock().await;
    match outcome {
        Ok(_) => {
            guard.insert(body.session_id, local);
            Ok(())
        }
        Err(e) => {
            // Even on error the loop restores checkpoints; persist that.
            guard.insert(body.session_id, local);
            Err(e.into())
        }
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>Forger</title>
<style>
  body { font: 15px/1.4 system-ui, sans-serif; max-width: 760px; margin: 2rem auto; padding: 0 1rem; }
  h1 { font-size: 1.4rem; }
  .warn { background: #fff3cd; padding: .75rem 1rem; border: 1px solid #e6c200; }
  #log { white-space: pre-wrap; background: #111; color: #eee; padding: 1rem; min-height: 12rem; }
  textarea { width: 100%; height: 5rem; }
  button { margin-top: .5rem; }
</style>
<h1>Forger</h1>
<p class="warn">This UI is loopback-only and has <strong>no authentication</strong>.
Do not expose the port. Auth will be a real session, or nothing.</p>
<p id="sid"></p>
<textarea id="msg" placeholder="Ask Forger to do something in the workspace…"></textarea><br>
<label><input type="checkbox" id="yes"> --yes (sensitive tools)</label>
<label><input type="checkbox" id="deny"> --allow-denied-paths (separate denylist override)</label><br>
<button id="send">Send</button>
<pre id="log"></pre>
<script>
let sessionId = null;
const log = (t) => { document.getElementById('log').textContent += t; };
fetch('/v1/session', {method:'POST', headers:{'content-type':'application/json'}, body:'{}'})
  .then(r => r.json()).then(j => { sessionId = j.id; document.getElementById('sid').textContent = 'session ' + sessionId; });
document.getElementById('send').onclick = async () => {
  const message = document.getElementById('msg').value;
  const yes = document.getElementById('yes').checked;
  const allow_denied_paths = document.getElementById('deny').checked;
  log('\n> ' + message + '\n');
  const res = await fetch('/v1/turn', {
    method:'POST',
    headers:{'content-type':'application/json'},
    body: JSON.stringify({session_id: sessionId, message, yes, allow_denied_paths})
  });
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = '';
  while (true) {
    const {value, done} = await reader.read();
    if (done) break;
    buf += dec.decode(value, {stream:true});
    const parts = buf.split('\n\n');
    buf = parts.pop();
    for (const p of parts) {
      const line = p.split('\n').filter(l => l.startsWith('data:')).map(l => l.slice(5).trim()).join('');
      if (!line) continue;
      try {
        const ev = JSON.parse(line);
        if (ev.type === 'text_delta') log(ev.text || '');
        else if (ev.type === 'tool_call') log('\n→ ' + (ev.call && ev.call.name) + '\n');
        else if (ev.type === 'finished') log('\n[' + (ev.outcome || 'done') + ']\n');
        else if (ev.type === 'warning' || ev.type === 'error') log('\n[' + ev.type + '] ' + (ev.message || '') + '\n');
      } catch (e) { log(line); }
    }
  }
};
</script>
"#;
