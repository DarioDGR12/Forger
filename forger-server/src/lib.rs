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
use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};

const NO_AUTH_BANNER: &str = "\
forger-server is listening with NO authentication. This is acceptable only \
because the bind address is loopback. Do not proxy or expose this port. \
A real auth design (session token/cookie) is future work — we will not ship a \
half-implemented check just to look like we have one.";

struct SessionSlot {
    session: Session,
    busy: bool,
}

#[derive(Clone)]
struct AppState {
    workspace: PathBuf,
    sessions: Arc<Mutex<HashMap<String, SessionSlot>>>,
}

/// Cancels the in-flight turn when the SSE consumer goes away (browser tab
/// close, fetch abort, proxy drop). The agent task is *not* aborted: it
/// observes the token, restores the session checkpoint, then persists.
struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
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
        sessions: Arc::new(Mutex::new(HashMap::new())),
    };
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(state)).await?;
    Ok(())
}

fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/health", get(|| async { "ok" }))
        .route("/v1/session", post(new_session))
        .route("/v1/turn", post(turn_sse))
        .with_state(state)
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
        if let Ok(s) = Session::load_from_str(&state.workspace, &id) {
            let sid = s.id.to_string();
            state.sessions.lock().await.insert(
                sid.clone(),
                SessionSlot {
                    session: s,
                    busy: false,
                },
            );
            return Json(serde_json::json!({"id": sid})).into_response();
        }
        Json(serde_json::json!({"id": id})).into_response()
    } else {
        let s = Session::new();
        let id = s.id.to_string();
        let _ = s.save_to(&state.workspace);
        state.sessions.lock().await.insert(
            id.clone(),
            SessionSlot {
                session: s,
                busy: false,
            },
        );
        Json(serde_json::json!({"id": id})).into_response()
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
}

async fn turn_sse(
    State(state): State<AppState>,
    Json(body): Json<TurnBody>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let cancel = CancellationToken::new();
    let workspace = state.workspace.clone();
    let sessions = state.sessions.clone();
    let cancel_task = cancel.clone();
    tokio::spawn(async move {
        let result = run_one_turn(workspace, sessions, body, tx.clone(), cancel_task).await;
        if let Err(e) = result {
            let _ =
                tx.send(serde_json::json!({"type":"error","message": e.to_string()}).to_string());
        }
        let _ = tx.send(serde_json::json!({"type":"done"}).to_string());
    });

    let stream =
        futures::stream::unfold((rx, CancelOnDrop(cancel)), |(mut rx, guard)| async move {
            rx.recv()
                .await
                .map(|data| (Ok(Event::default().data(data)), (rx, guard)))
        });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn run_one_turn(
    workspace: PathBuf,
    sessions: Arc<Mutex<HashMap<String, SessionSlot>>>,
    body: TurnBody,
    tx: mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let mut map = sessions.lock().await;
    let slot = map
        .entry(body.session_id.clone())
        .or_insert_with(|| SessionSlot {
            session: Session::load_or_create(&workspace, &body.session_id),
            busy: false,
        });
    if slot.busy {
        drop(map);
        anyhow::bail!("session {} is already running a turn", body.session_id);
    }
    slot.busy = true;
    let mut local = slot.session.clone();
    drop(map);

    let result = execute_turn(&workspace, &body, &tx, cancel, &mut local).await;

    let mut map = sessions.lock().await;
    if let Some(slot) = map.get_mut(&body.session_id) {
        slot.session = local;
        slot.busy = false;
        let _ = slot.session.save_to(&workspace);
    }
    result
}

async fn execute_turn(
    workspace: &Path,
    body: &TurnBody,
    tx: &mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
    local: &mut Session,
) -> anyhow::Result<()> {
    let sandbox = Arc::new(FsSandbox::new(workspace)?);
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
    let agent = AgentLoop::new(
        provider,
        tools,
        approver,
        AgentLoopConfig::for_workspace(workspace.to_path_buf()),
    );

    let mut sink = |ev: AgentEvent| {
        let v = serde_json::to_value(&ev).unwrap_or_else(|_| serde_json::json!({"type":"unknown"}));
        let _ = tx.send(v.to_string());
    };
    agent
        .run_turn(
            local,
            UserTurn {
                text: body.message.clone(),
            },
            cancel,
            &mut sink,
        )
        .await?;
    Ok(())
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
  button { margin-top: .5rem; margin-right: .5rem; }
</style>
<h1>Forger</h1>
<p class="warn">This UI is loopback-only and has <strong>no authentication</strong>.
Do not expose the port. Auth will be a real session, or nothing.</p>
<p id="sid"></p>
<textarea id="msg" placeholder="Ask Forger to do something in the workspace…"></textarea><br>
<label><input type="checkbox" id="yes"> --yes (sensitive tools)</label>
<label><input type="checkbox" id="deny"> --allow-denied-paths (separate denylist override)</label><br>
<button id="send">Send</button>
<button id="cancel" disabled>Cancel</button>
<pre id="log"></pre>
<script>
let sessionId = null;
let inflight = null;
const log = (t) => { document.getElementById('log').textContent += t; };
const setBusy = (b) => {
  document.getElementById('send').disabled = b;
  document.getElementById('cancel').disabled = !b;
};
fetch('/v1/session', {method:'POST', headers:{'content-type':'application/json'}, body:'{}'})
  .then(r => r.json()).then(j => { sessionId = j.id; document.getElementById('sid').textContent = 'session ' + sessionId; });
document.getElementById('cancel').onclick = () => { if (inflight) inflight.abort(); };
document.getElementById('send').onclick = async () => {
  const message = document.getElementById('msg').value;
  const yes = document.getElementById('yes').checked;
  const allow_denied_paths = document.getElementById('deny').checked;
  log('\n> ' + message + '\n');
  const ac = new AbortController();
  inflight = ac;
  setBusy(true);
  try {
    const res = await fetch('/v1/turn', {
      method:'POST',
      headers:{'content-type':'application/json'},
      body: JSON.stringify({session_id: sessionId, message, yes, allow_denied_paths}),
      signal: ac.signal
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
          else if (ev.type === 'cancelled') log('\n(cancelled)\n');
          else if (ev.type === 'warning' || ev.type === 'error') log('\n[' + ev.type + '] ' + (ev.message || '') + '\n');
        } catch (e) { log(line); }
      }
    }
  } catch (e) {
    if (e && e.name === 'AbortError') log('\n(cancelled)\n');
    else log('\n[error] ' + e + '\n');
  } finally {
    inflight = null;
    setBusy(false);
  }
};
</script>
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn test_harness() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState {
            workspace: dir.path().to_path_buf(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        };
        (dir, state)
    }

    #[test]
    fn cancel_on_drop_fires() {
        let token = CancellationToken::new();
        let watch = token.clone();
        assert!(!watch.is_cancelled());
        {
            let _guard = CancelOnDrop(token);
            assert!(!watch.is_cancelled());
        }
        assert!(watch.is_cancelled());
    }

    #[tokio::test]
    async fn health_ok() {
        let (_dir, state) = test_harness();
        let app = app(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"ok");
    }

    #[tokio::test]
    async fn new_session_and_turn_with_mock() {
        let (_dir, state) = test_harness();
        let app = app(state);
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = res.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["id"].as_str().unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/turn")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"session_id":"{id}","message":"hi"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec())
            .unwrap();
        assert!(
            body.contains("text_delta") || body.contains("MockProvider"),
            "unexpected SSE body: {body}"
        );
    }

    #[tokio::test]
    async fn dropping_sse_body_cancels_without_sticking_busy() {
        let dir = tempfile::tempdir().unwrap();
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let state = AppState {
            workspace: dir.path().to_path_buf(),
            sessions: sessions.clone(),
        };
        let app = app(state);
        let created = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/session")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = created.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let id = v["id"].as_str().unwrap().to_string();

        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/turn")
                    .header("content-type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"session_id":"{id}","message":"hi"}}"#
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        // Drop the body without reading it — this is the SSE disconnect.
        drop(res);

        // Give the agent task time to observe cancel and clear `busy`.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let map = sessions.lock().await;
        let slot = map.get(&id).expect("session kept");
        assert!(!slot.busy, "disconnect must not leave the session busy");
    }
}
