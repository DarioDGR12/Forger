use forger_core::{Tool, ToolContext, ToolError};
use forger_sandbox::{FsSandbox, Sandbox};
use forger_tools::{
    EditFile, Git, Glob, Grep, ListDir, ReadFile, RenameFile, RunCommand, WriteFile,
};
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

#[tokio::test]
async fn list_dir_hides_env() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let list = ListDir::new(sb);
    let out = list
        .execute(json!({"path": "."}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert!(out.contains("main.rs"));
    assert!(!out.contains(".env"));
}

#[tokio::test]
async fn grep_finds_source_but_not_env() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("app.rs"), "let secret = 1;\n").unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1\n").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let grep = Grep::new(sb);
    let out = grep
        .execute(
            json!({"pattern": "secret", "glob": "*.rs"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    assert!(out.contains("app.rs"));
    assert!(!out.contains(".env"));
}

#[tokio::test]
async fn edit_file_replaces_unique_snippet() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    WriteFile::new(sb.clone())
        .execute(
            json!({"path":"a.rs","contents":"fn a() {}\nfn b() {}\n"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    let edit = EditFile::new(sb.clone());
    edit.execute(
        json!({"path":"a.rs","old_string":"fn b() {}","new_string":"fn b() { 1 }"}),
        &ctx(dir.path(), false),
    )
    .await
    .unwrap();
    let got = ReadFile::new(sb)
        .execute(json!({"path":"a.rs"}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert_eq!(got, "fn a() {}\nfn b() { 1 }\n");
}

#[tokio::test]
async fn edit_file_refuses_ambiguous_replace() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    WriteFile::new(sb.clone())
        .execute(
            json!({"path":"a.rs","contents":"x\nx\n"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    let err = EditFile::new(sb)
        .execute(
            json!({"path":"a.rs","old_string":"x","new_string":"y"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Failed { .. }));
}

#[tokio::test]
async fn rename_file_config_to_env_is_denied() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    WriteFile::new(sb.clone())
        .execute(
            json!({"path":"config","contents":"SECRET=1"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    let err = RenameFile::new(sb)
        .execute(
            json!({"from":"config","to":".env"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Denied { .. }));
    assert!(dir.path().join("config").exists());
    assert!(!dir.path().join(".env").exists());
}

#[tokio::test]
async fn rename_file_innocent_roundtrip() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    WriteFile::new(sb.clone())
        .execute(
            json!({"path":"a.txt","contents":"hi"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    RenameFile::new(sb.clone())
        .execute(
            json!({"from":"a.txt","to":"b.txt"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap();
    let got = ReadFile::new(sb)
        .execute(json!({"path":"b.txt"}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert_eq!(got, "hi");
    assert!(!dir.path().join("a.txt").exists());
}

#[tokio::test]
async fn glob_finds_rs_not_env() {
    let dir = tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "").unwrap();
    std::fs::write(dir.path().join("README.md"), "").unwrap();
    std::fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let glob = Glob::new(sb);
    let out = glob
        .execute(json!({"pattern": "**/*.rs"}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert!(out.contains("lib.rs"), "got {out}");
    assert!(!out.contains(".env"));
    assert!(!out.contains("README.md"));
}

#[tokio::test]
async fn git_status_in_fresh_repo() {
    let dir = tempdir().unwrap();
    let status = std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("git init");
    if !status.success() {
        return; // environment without git
    }
    std::fs::write(dir.path().join("a.txt"), "hi").unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let git = Git::new(sb, Duration::from_secs(10));
    let out = git
        .execute(json!({"action": "status"}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert!(
        out.contains("a.txt") || out.contains("No commits") || out.contains("exit 0"),
        "unexpected git status: {out}"
    );
}

#[tokio::test]
async fn git_diff_rejects_shell_metacharacters() {
    let dir = tempdir().unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let git = Git::new(sb, Duration::from_secs(5));
    let err = git
        .execute(
            json!({"action": "diff", "path": "a.txt; rm -rf /"}),
            &ctx(dir.path(), false),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::MalformedArgs(_)));
}

#[tokio::test]
async fn cat_env_via_run_command_is_redacted() {
    let dir = tempdir().unwrap();
    std::fs::write(
        dir.path().join(".env"),
        "API_KEY=sk-super-secret-value-do-not-leak\n",
    )
    .unwrap();
    let sb: Arc<dyn Sandbox> = Arc::new(FsSandbox::new(dir.path()).unwrap());
    let run = RunCommand::new(sb, Duration::from_secs(5));
    let out = run
        .execute(json!({"command": "cat .env"}), &ctx(dir.path(), false))
        .await
        .unwrap();
    assert!(!out.contains("sk-super-secret"), "leaked: {out}");
    assert!(out.contains("[redacted: denylist]"), "got {out}");
}
