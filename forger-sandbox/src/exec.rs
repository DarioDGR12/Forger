use crate::CommandOutput;
use crate::SandboxError;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Timeout supervisor around a shell command. On timeout the whole process
/// group is killed so grandchildren cannot outlive the tool call and stall
/// the agent loop.
pub async fn run_command(
    workspace: &Path,
    command: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    landlock_warning: Option<&str>,
) -> Result<CommandOutput, SandboxError> {
    if cancel.is_cancelled() {
        return Err(SandboxError::Cancelled);
    }
    if let Some(w) = landlock_warning {
        tracing::warn!(target: "forger_sandbox::exec", "{w}");
    }

    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("FORGER_SANDBOX", "1");

    #[cfg(unix)]
    {
        cmd.process_group(0);
    }

    #[cfg(target_os = "linux")]
    {
        let ws_child = workspace.to_path_buf();
        unsafe {
            cmd.pre_exec(move || {
                if let Err(e) = crate::landlock_linux::restrict_self(&ws_child) {
                    eprintln!("forger-sandbox: Landlock restrict_self failed: {e}");
                }
                Ok(())
            });
        }
    }

    let mut child = cmd.spawn()?;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");

    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf).await;
        buf
    });
    let stderr_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        buf
    });

    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            kill_group(&mut child, pid).await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(SandboxError::Cancelled);
        }
        _ = tokio::time::sleep(timeout) => {
            kill_group(&mut child, pid).await;
            let _ = child.wait().await;
            stdout_task.abort();
            stderr_task.abort();
            return Err(SandboxError::Timeout {
                timeout_ms: timeout.as_millis() as u64,
            });
        }
        status = child.wait() => {
            status?
        }
    };

    let stdout = String::from_utf8_lossy(&stdout_task.await.unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_task.await.unwrap_or_default()).into_owned();
    Ok(CommandOutput {
        exit_code: status.code().unwrap_or(-1),
        stdout,
        stderr,
        timed_out: false,
    })
}

async fn kill_group(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    {
        if let Some(pid) = pid {
            let pgid = nix::unistd::Pid::from_raw(pid as i32);
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    let _ = child.kill().await;
}
