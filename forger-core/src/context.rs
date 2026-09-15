//! Workspace-aware system prompt: instruction files + a truncated tree.
//!
//! This is context for the model, not a security boundary. Denylist names
//! are omitted from the tree the same way `list_dir` hides them.

use crate::paths::{path_hits_denylist, skip_tree_dir};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_RULES_CHARS: usize = 12_000;
const MAX_TREE_ENTRIES: usize = 80;
const MAX_TREE_DEPTH: usize = 4;
const MAX_FILE_CHARS: usize = 8_000;

const RULE_FILES: &[&str] = &["AGENTS.md", "FORGER.md", ".forger/rules.md"];

/// Build the system prompt: base instructions, then project rules, then tree.
pub fn compose_system_prompt(workspace: &Path, base: &str) -> String {
    let mut out = String::from(base.trim());
    out.push('\n');

    if let Some(rules) = load_rules(workspace) {
        out.push_str("\n# Project instructions\n");
        out.push_str(&rules);
        if !rules.ends_with('\n') {
            out.push('\n');
        }
    }

    if let Some(tree) = truncated_tree(workspace) {
        out.push_str("\n# Workspace tree (truncated; denylist names omitted)\n");
        out.push_str(&tree);
        if !tree.ends_with('\n') {
            out.push('\n');
        }
    }

    out
}

fn load_rules(workspace: &Path) -> Option<String> {
    let mut parts = Vec::new();
    let mut used = 0usize;
    for rel in RULE_FILES {
        if used >= MAX_RULES_CHARS {
            break;
        }
        let path = workspace.join(rel);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let budget = MAX_RULES_CHARS.saturating_sub(used);
        let body = take_chars(&text, budget.min(MAX_FILE_CHARS));
        used += body.len();
        parts.push(format!("## {rel}\n{body}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn take_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n… [truncated]");
    out
}

fn truncated_tree(workspace: &Path) -> Option<String> {
    let mut lines = Vec::new();
    walk_tree(workspace, workspace, 0, &mut lines);
    if lines.is_empty() {
        None
    } else {
        if lines.len() >= MAX_TREE_ENTRIES {
            lines.push("…".into());
        }
        Some(lines.join("\n"))
    }
}

fn walk_tree(workspace: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if out.len() >= MAX_TREE_ENTRIES || depth > MAX_TREE_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut kids: Vec<(String, PathBuf, bool)> = Vec::new();
    for ent in entries.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if path_hits_denylist(&name) || (depth == 0 && skip_tree_dir(&name)) {
            continue;
        }
        if depth > 0 && skip_tree_dir(&name) {
            continue;
        }
        let path = ent.path();
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        kids.push((name, path, is_dir));
    }
    kids.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path, is_dir) in kids {
        if out.len() >= MAX_TREE_ENTRIES {
            return;
        }
        let rel = path.strip_prefix(workspace).unwrap_or(&path);
        let rel_s = rel.to_string_lossy();
        if path_hits_denylist(&rel_s) {
            continue;
        }
        if is_dir {
            out.push(format!("{rel_s}/"));
            walk_tree(workspace, &path, depth + 1, out);
        } else {
            out.push(rel_s.into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::compose_system_prompt;
    use std::fs;

    #[test]
    fn injects_agents_md_and_hides_env() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("AGENTS.md"), "Prefer edit_file.").unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join(".env"), "SECRET=please-hide-me").unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();

        let prompt = compose_system_prompt(dir.path(), "You are Forger.");
        assert!(prompt.contains("You are Forger."));
        assert!(prompt.contains("Prefer edit_file."));
        assert!(prompt.contains("## AGENTS.md"));
        assert!(prompt.contains("main.rs"));
        assert!(prompt.contains("src/"));
        assert!(!prompt.contains(".env"));
        assert!(!prompt.contains("please-hide-me"));
    }

    #[test]
    fn missing_workspace_still_returns_base() {
        let prompt = compose_system_prompt(std::path::Path::new("/no/such/forger-ws"), "base");
        assert_eq!(prompt.trim(), "base");
    }
}
