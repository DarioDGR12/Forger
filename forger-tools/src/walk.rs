use crate::pattern::glob_ok;
use forger_core::ToolError;
use forger_sandbox::{DirEntry, Sandbox, WritePermit};
use std::path::{Path, PathBuf};

pub const MAX_FILES: usize = 400;
pub const SKIP_DIRS: &[&str] = &["target", "node_modules", "dist", ".venv"];

pub fn skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name)
}

/// Recursively collect file paths under `path`, honoring the sandbox list/denylist.
pub async fn collect_files(
    sandbox: &dyn Sandbox,
    path: &Path,
    permit: WritePermit,
    glob: Option<&str>,
    depth: usize,
    out: &mut Vec<PathBuf>,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<(), ToolError> {
    if out.len() >= MAX_FILES || depth > 12 {
        return Ok(());
    }
    if cancel.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let entries = match sandbox.list(path, permit.clone(), cancel).await {
        Ok(e) => e,
        Err(_) => {
            if glob_ok(path, glob) {
                out.push(path.to_path_buf());
            }
            return Ok(());
        }
    };
    for DirEntry {
        name,
        path: child,
        is_dir,
    } in entries
    {
        if is_dir {
            if skip_dir(&name) {
                continue;
            }
            Box::pin(collect_files(
                sandbox,
                &child,
                WritePermit::Normal,
                glob,
                depth + 1,
                out,
                cancel,
            ))
            .await?;
        } else if glob_ok(&child, glob) {
            out.push(child);
            if out.len() >= MAX_FILES {
                return Ok(());
            }
        }
    }
    Ok(())
}
