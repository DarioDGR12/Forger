use forger_core::{Tool, ToolContext, ToolError};
use forger_sandbox::{FsSandbox, Sandbox};
use forger_tools::{ReadFile, RunCommand, WriteFile};
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn ctx<'a>(ws: &'a std::path::Path, override_: bool) -> ToolContext<'a> {
    ToolContext {
        workspace: ws,
        cancel: CancellationToken::new(),
        denylist_override: override_,
    }
}

#[tokio::test]
async fn write_then_read_roundtrip() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let write = WriteFile::new(sb.clone());
    let read = ReadFile::new(sb);
    let c = ctx(dir.path(), false);
    write
        .execute(json!({"path":"a.txt","contents":"hi"}), &c)
        .await
        .unwrap();
    let got = read.execute(json!({"path":"a.txt"}), &c).await.unwrap();
    assert_eq!(got, "hi");
}

#[tokio::test]
async fn write_env_denied_without_override() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let write = WriteFile::new(sb);
    let err = write
        .execute(
            json!({"path":".env","contents":"SECRET=1"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_env_denied_without_override() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let read = ReadFile::new(sb);
    let err = read
        .execute(json!({"path":".env"}), &ctx(dir.path(), false))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
}

#[tokio::test]
async fn read_env_honors_denylist_override() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let read = ReadFile::new(sb);
    let got = read
        .execute(json!({"path":".env"}), &ctx(dir.path(), true))
        .await
        .unwrap();
    assert_eq!(got, "SECRET=1");
}

#[tokio::test]
async fn hung_run_command_times_out() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let run = RunCommand::new(sb, Duration::from_millis(400));
    let start = Instant::now();
    let err = run
        .execute(json!({"command":"sleep 30"}), &ctx(dir.path(), false))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Timeout { .. }));
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn malformed_args_are_tool_errors() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let write = WriteFile::new(sb);
    let err = write
        .execute(json!({"contents":"x"}), &ctx(dir.path(), false))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::MalformedArgs(_)));
}
