//! Name-based denylist hints for the generic layer (agent loop, workspace
//! tree). Not a substitute for [`forger_sandbox`] canonical resolution.

use std::path::Path;

/// `true` when any path component matches `.env` / `.git` / `.ssh` / `credentials`.
pub fn path_hits_denylist(path: &str) -> bool {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    lower.split('/').any(component_hits_denylist)
}

pub fn component_hits_denylist(comp: &str) -> bool {
    let lower = comp.to_ascii_lowercase();
    if lower == ".env" || lower.starts_with(".env.") {
        return true;
    }
    if lower == ".git" || lower == ".ssh" {
        return true;
    }
    lower == "credentials" || lower.ends_with("credentials")
}

pub fn path_looks_like(lower: &str, pat: &str) -> bool {
    lower.split('/').any(|comp| {
        if pat == ".env" {
            comp == ".env" || comp.starts_with(".env.")
        } else if pat == "credentials" {
            comp == "credentials" || comp.ends_with("credentials")
        } else {
            comp == pat
        }
    })
}

pub fn skip_tree_dir(name: &str) -> bool {
    matches!(
        name,
        "target" | "node_modules" | "dist" | ".venv" | ".git" | ".forger"
    ) || component_hits_denylist(name)
}

pub fn first_denylist_pattern(path: &str) -> Option<&'static str> {
    let lower = path.replace('\\', "/").to_ascii_lowercase();
    for pat in [".env", ".git", ".ssh", "credentials"] {
        if path_looks_like(&lower, pat) {
            return Some(pat);
        }
    }
    None
}

pub fn path_display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{component_hits_denylist, path_hits_denylist};

    #[test]
    fn denylist_names() {
        assert!(path_hits_denylist(".env"));
        assert!(path_hits_denylist("foo/.env.local"));
        assert!(path_hits_denylist("src/.git/config"));
        assert!(path_hits_denylist(".ssh/id_rsa"));
        assert!(path_hits_denylist(".aws/credentials"));
        assert!(!path_hits_denylist("src/main.rs"));
        assert!(!path_hits_denylist("env.txt"));
        assert!(component_hits_denylist("credentials"));
    }
}
