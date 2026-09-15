//! Path denylist. Non-negotiable by default.
//!
//! Patterns (any path component, case-insensitive):
//! - `.env` and `.env.*`
//! - `.git`
//! - `.ssh`
//! - `credentials` (e.g. `.aws/credentials`, `.git-credentials`)

use crate::DenylistHit;
use std::path::Path;

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
}
