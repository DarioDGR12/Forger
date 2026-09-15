//! Resolve the *final* path a write/read would hit, not the name the tool asked for.
//!
//! Before every mutation we:
//! 1. `std::fs::canonicalize` the parent directory (follows directory symlinks)
//! 2. append the requested file name
//! 3. if that path already exists, canonicalize it too (follows a file symlink
//!    such as `config` → `.env`)
//!
//! [`crate::denylist::is_sensitive`] / [`crate::denylist::check`] then run on
//! that result, not only on the original requested name. `Sandbox::rename`
//! applies the same check to **both** source and destination.

use crate::SandboxError;
use std::io;
use std::path::{Component, Path, PathBuf};

/// Canonical write target: `canonicalize(parent)` + file name.
///
/// The file itself need not exist. If it does (including as a symlink), the
/// full path is canonicalized so the denylist sees the real destination.
pub fn resolve_final_path(workspace: &Path, requested: &Path) -> Result<PathBuf, SandboxError> {
    let joined = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        workspace.join(requested)
    };

    let file_name = joined.file_name().ok_or_else(|| {
        SandboxError::Other(format!("path `{}` has no file name", joined.display()))
    })?;

    let parent = match joined.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => workspace,
    };

    let canonical_parent = canonicalize_existing_prefix(parent)?;
    let candidate = canonical_parent.join(file_name);

    match std::fs::canonicalize(&candidate) {
        Ok(real) => Ok(real),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(candidate),
        Err(err) => Err(err.into()),
    }
}

/// `canonicalize` the longest existing prefix of `path`, then re-append any
/// missing components. Used when the parent of a new file does not exist yet.
fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, SandboxError> {
    if path.exists() {
        return Ok(std::fs::canonicalize(path)?);
    }

    let mut existing = PathBuf::new();
    let mut missing: Vec<Component<'_>> = Vec::new();
    let mut past_missing = false;
    for comp in path.components() {
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

    #[test]
    fn symlink_parent_is_canonicalized_before_joining_name() {
        let dir = tempdir().unwrap();
        let ws = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir(ws.join("real")).unwrap();
        symlink(ws.join("real"), ws.join("link")).unwrap();
        let resolved = resolve_final_path(&ws, Path::new("link/config")).unwrap();
        assert_eq!(resolved, ws.join("real").join("config"));
    }
}
