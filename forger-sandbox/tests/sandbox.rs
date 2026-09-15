use forger_sandbox::{FsSandbox, Sandbox, SandboxError, WritePermit};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::time::{Duration, Instant};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

fn sandbox() -> (tempfile::TempDir, FsSandbox) {
    let dir = tempdir().unwrap();
    let sb = FsSandbox::new(dir.path()).unwrap();
    (dir, sb)
}

#[tokio::test]
async fn write_to_env_is_blocked() {
    let (_dir, sb) = sandbox();
    let err = sb
        .write(
            Path::new(".env"),
            "SECRET=1",
            WritePermit::Normal,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        SandboxError::Denylist {
            pattern: ".env",
            ..
        }
    ));
}

#[tokio::test]
async fn write_via_symlink_to_env_is_blocked() {
    let (dir, sb) = sandbox();
    fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    symlink(dir.path().join(".env"), dir.path().join("innocent")).unwrap();
    let err = sb
        .write(
            Path::new("innocent"),
            "SECRET=2",
            WritePermit::Normal,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SandboxError::Denylist {
                pattern: ".env",
                ..
            }
        ),
        "symlink must be resolved before the denylist check, got {err:?}"
    );
}

/// Previously accepted bypass: write under a harmless name, then rename to `.env`.
/// The destination is now canonicalized and run through `is_sensitive` / the denylist.
#[tokio::test]
async fn write_config_then_rename_to_env_is_blocked() {
    let (_dir, sb) = sandbox();
    let cancel = CancellationToken::new();
    sb.write(
        Path::new("config"),
        "SECRET=1",
        WritePermit::Normal,
        &cancel,
    )
    .await
    .unwrap();
    assert!(sb.workspace().join("config").exists());

    let err = sb
        .rename(
            Path::new("config"),
            Path::new(".env"),
            WritePermit::Normal,
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SandboxError::Denylist {
                pattern: ".env",
                ..
            }
        ),
        "rename to .env must fail as denylist, got {err:?}"
    );
    assert!(
        !sb.workspace().join(".env").exists(),
        "`.env` must not appear after a refused rename"
    );
    assert!(
        sb.workspace().join("config").exists(),
        "source `config` must still exist after a refused rename"
    );
    assert_eq!(
        fs::read_to_string(sb.workspace().join("config")).unwrap(),
        "SECRET=1"
    );
}

#[tokio::test]
async fn write_config_then_shell_mv_to_env_is_rolled_back() {
    let (_dir, sb) = sandbox();
    let cancel = CancellationToken::new();
    sb.write(
        Path::new("config"),
        "SECRET=1",
        WritePermit::Normal,
        &cancel,
    )
    .await
    .unwrap();

    let err = sb
        .run("mv config .env", Duration::from_secs(5), &cancel)
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SandboxError::Denylist {
                pattern: ".env",
                ..
            }
        ),
        "shell mv to .env must be rolled back as denylist, got {err:?}"
    );
    assert!(
        !sb.workspace().join(".env").exists(),
        "`.env` must not remain after a shell rename"
    );
}

#[tokio::test]
async fn denylist_override_must_match_resolved_path() {
    let (_dir, sb) = sandbox();
    let decision = sb.inspect(Path::new(".env")).unwrap();
    assert!(decision.denylist.is_some());
    sb.write(
        Path::new(".env"),
        "SECRET=1",
        WritePermit::DenylistOverride {
            confirmed_resolved: decision.resolved.clone(),
        },
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    let err = sb
        .write(
            Path::new(".env"),
            "nope",
            WritePermit::DenylistOverride {
                confirmed_resolved: decision.resolved.join("other"),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SandboxError::Denylist { .. }));
}

#[tokio::test]
async fn hung_command_is_killed_by_timeout_supervisor() {
    let (_dir, sb) = sandbox();
    let start = Instant::now();
    let err = sb
        .run(
            "sleep 30",
            Duration::from_millis(400),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, SandboxError::Timeout { .. }),
        "expected timeout, got {err:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "timeout supervisor took too long: {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn echo_command_works() {
    let (_dir, sb) = sandbox();
    let out = sb
        .run(
            "echo hello",
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.exit_code, 0);
    assert!(out.stdout.contains("hello"));
}

#[tokio::test]
async fn git_and_ssh_paths_blocked() {
    let (_dir, sb) = sandbox();
    for p in [".git/config", ".ssh/id_rsa", ".aws/credentials"] {
        let err = sb
            .write(
                Path::new(p),
                "x",
                WritePermit::Normal,
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, SandboxError::Denylist { .. }),
            "{p} should be denylisted, got {err:?}"
        );
    }
}

#[tokio::test]
async fn list_omits_denylisted_names() {
    let (dir, sb) = sandbox();
    fs::write(dir.path().join("ok.txt"), "x").unwrap();
    fs::write(dir.path().join(".env"), "SECRET=1").unwrap();
    fs::create_dir(dir.path().join(".ssh")).unwrap();
    let entries = sb
        .list(Path::new("."), WritePermit::Normal, &CancellationToken::new())
        .await
        .unwrap();
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"ok.txt"));
    assert!(!names.contains(&".env"), "denylist names must not be advertised");
    assert!(!names.contains(&".ssh"));
}

#[tokio::test]
async fn list_of_git_dir_is_blocked() {
    let (dir, sb) = sandbox();
    fs::create_dir(dir.path().join(".git")).unwrap();
    let err = sb
        .list(
            Path::new(".git"),
            WritePermit::Normal,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, SandboxError::Denylist { pattern: ".git", .. }));
}
