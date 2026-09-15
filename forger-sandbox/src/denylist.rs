//! Path denylist. Non-negotiable by default.
//!
//! Patterns (any path component, case-insensitive):
//! - `.env` and `.env.*`
//! - `.git`
//! - `.ssh`
//! - `credentials` (e.g. `.aws/credentials`, `.git-credentials`)

use crate::DenylistHit;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Lexical denylist. Does **not** follow symlinks — callers must also run
/// this on the canonical destination (`canonicalize(parent)` + file name).
pub fn check(path: &Path) -> Option<DenylistHit> {
    for comp in path.components() {
        let Some(name) = comp.as_os_str().to_str() else {
            // Fail closed: a non-UTF-8 component cannot be proven safe.
            return Some(DenylistHit {
                path: path.to_path_buf(),
                pattern: "non-utf8",
            });
        };
        let lower = name.to_ascii_lowercase();
        if let Some(pattern) = match_component(&lower) {
            return Some(DenylistHit {
                path: path.to_path_buf(),
                pattern,
            });
        }
    }
    None
}

/// `true` when [`check`] would block `path`.
pub fn is_sensitive(path: &Path) -> bool {
    check(path).is_some()
}

/// Sensitive paths currently in `workspace`, excluding `.git` (so `git init`
/// via `run_command` still works). Directory symlinks are not followed.
pub fn snapshot_sensitive(workspace: &Path) -> HashSet<PathBuf> {
    let mut out = HashSet::new();
    walk_sensitive(workspace, workspace, &mut out);
    out
}

fn is_git_component(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .map(|s| s.eq_ignore_ascii_case(".git"))
        .unwrap_or(false)
}

fn walk_sensitive(workspace: &Path, dir: &Path, out: &mut HashSet<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let rel = path.strip_prefix(workspace).unwrap_or(&path);
        if rel.components().any(|c| is_git_component(c.as_os_str())) {
            continue;
        }
        if is_sensitive(rel) || is_sensitive(&path) {
            out.insert(path);
            continue;
        }
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            walk_sensitive(workspace, &path, out);
        }
    }
}

fn remove_any(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let _ = fs::remove_dir_all(path);
        }
        Ok(_) => {
            let _ = fs::remove_file(path);
        }
        Err(_) => {}
    }
}

/// Delete sensitive paths that appeared after a shell command. Returns the
/// first hit so `run_command` can fail closed instead of leaving a new `.env`.
pub fn rollback_new_sensitive(workspace: &Path, before: &HashSet<PathBuf>) -> Option<DenylistHit> {
    let after = snapshot_sensitive(workspace);
    let mut first = None;
    for path in after.difference(before) {
        if first.is_none() {
            first = check(path)
                .or_else(|| path.strip_prefix(workspace).ok().and_then(check))
                .or_else(|| {
                    Some(DenylistHit {
                        path: path.clone(),
                        pattern: "denylist",
                    })
                });
        }
        tracing::warn!(
            path = %path.display(),
            "removing sensitive path created by run_command"
        );
        remove_any(path);
    }
    first
}

fn match_component(lower: &str) -> Option<&'static str> {
    if lower == ".env" || lower.starts_with(".env.") {
        return Some(".env");
    }
    if lower == ".git" {
        return Some(".git");
    }
    if lower == ".ssh" {
        return Some(".ssh");
    }
    if lower == "credentials" || lower.ends_with("credentials") {
        return Some("credentials");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{check, is_sensitive};
    use std::path::Path;

    #[test]
    fn hits_env_git_ssh_credentials() {
        assert_eq!(check(Path::new("/ws/.env")).unwrap().pattern, ".env");
        assert_eq!(check(Path::new("/ws/.env.local")).unwrap().pattern, ".env");
        assert_eq!(check(Path::new("/ws/.git/config")).unwrap().pattern, ".git");
        assert_eq!(
            check(Path::new("/ws/.ssh/id_ed25519")).unwrap().pattern,
            ".ssh"
        );
        assert_eq!(
            check(Path::new("/ws/.aws/credentials")).unwrap().pattern,
            "credentials"
        );
        assert!(check(Path::new("/ws/src/main.rs")).is_none());
        assert!(check(Path::new("/ws/environment.txt")).is_none());
        assert!(is_sensitive(Path::new(".env")));
        assert!(!is_sensitive(Path::new("config")));
    }

    #[test]
    fn rollback_removes_new_dotenv() {
        use super::{rollback_new_sensitive, snapshot_sensitive};
        use std::fs;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        fs::write(ws.join("config"), "SECRET=1").unwrap();
        let before = snapshot_sensitive(ws);
        assert!(before.is_empty());
        fs::rename(ws.join("config"), ws.join(".env")).unwrap();
        let hit = rollback_new_sensitive(ws, &before).unwrap();
        assert_eq!(hit.pattern, ".env");
        assert!(!ws.join(".env").exists());
    }
}
