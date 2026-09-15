//! Resolve the *final* path a write/read would hit, not the name the tool asked for.
//!
//! This closes the "write to `innocent` which is a symlink to `.env`" bypass.
//! A later `mv innocent .env` from the shell is a documented remaining gap.

use crate::SandboxError;
use std::path::{Component, Path, PathBuf};

pub fn resolve_final_path(workspace: &Path, requested: &Path) -> Result<PathBuf, SandboxError> {
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };

    // Existing path (including a symlink): canonicalize follows the link.
    // That *is* the final path the write would hit.
    if joined.exists() {
        return Ok(std::fs::canonicalize(&joined)?);
    }

    // Walk existing prefix, canonicalize it (follows symlinks), then append
    // the missing tail literally.
    let mut existing = PathBuf::new();
    let mut missing: Vec<Component<'_>> = Vec::new();
    let mut past_missing = false;
    for comp in joined.components() {
        if past_missing {
            missing.push(comp);
            continue;
        }
        let candidate = existing.join(comp.as_os_str());
        if candidate.exists() {
            existing = candidate;
        } else {
            past_missing = true;
            missing.push(comp);
        }
    }

    let mut resolved = if existing.as_os_str().is_empty() {
        PathBuf::new()
    } else if existing.exists() {
        std::fs::canonicalize(&existing)?
    } else {
        existing
    };
    for comp in missing {
        resolved.push(comp.as_os_str());
    }
    Ok(resolved)
}

pub fn is_inside(workspace: &Path, resolved: &Path) -> bool {
    resolved.starts_with(workspace)
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_final_path;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::Path;
    use tempfile::tempdir;

    #[test]
    fn symlink_to_env_resolves_to_env() {
        let dir = tempdir().unwrap();
        let ws = dir.path();
        fs::write(ws.join(".env"), "SECRET=1").unwrap();
        symlink(ws.join(".env"), ws.join("innocent")).unwrap();
        let resolved = resolve_final_path(ws, Path::new("innocent")).unwrap();
        assert_eq!(resolved, fs::canonicalize(ws.join(".env")).unwrap());
    }

    #[test]
    fn missing_file_keeps_name_under_canonical_parent() {
        let dir = tempdir().unwrap();
        let ws = fs::canonicalize(dir.path()).unwrap();
        let resolved = resolve_final_path(&ws, Path::new("new.txt")).unwrap();
        assert_eq!(resolved, ws.join("new.txt"));
    }
}
