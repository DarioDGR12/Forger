//! Simple glob matching for stock tools (`*` / `**` / `?`).

use regex::Regex;
use std::path::Path;

/// Match a workspace-relative path against a glob.
///
/// - `*` — one path segment
/// - `**` — any depth (including zero)
/// - `?` — one character except `/`
///
/// `*.rs` also matches a bare filename (`main.rs`) so grep's suffix filter
/// keeps working.
pub fn glob_match(path: &str, pattern: &str) -> bool {
    let path = normalize(path);
    let pattern = normalize(pattern);
    if pattern_regex(&pattern).is_match(&path) {
        return true;
    }
    if let Some((_, name)) = path.rsplit_once('/') {
        pattern_regex(&pattern).is_match(name)
    } else {
        false
    }
}

pub fn glob_ok(path: &Path, glob: Option<&str>) -> bool {
    let Some(g) = glob else {
        return true;
    };
    glob_match(&path.to_string_lossy(), g)
}

fn normalize(s: &str) -> String {
    s.replace('\\', "/")
        .trim_start_matches("./")
        .trim_end_matches('/')
        .to_string()
}

fn pattern_regex(pattern: &str) -> Regex {
    // Compile per distinct pattern; tools typically pass one glob per call.
    thread_local! {
        static CACHE: std::cell::RefCell<Option<(String, Regex)>> =
            const { std::cell::RefCell::new(None) };
    }
    CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some((prev, re)) = slot.as_ref() {
            if prev == pattern {
                return re.clone();
            }
        }
        let re = Regex::new(&glob_to_regex(pattern)).unwrap_or_else(|_| Regex::new("^$").unwrap());
        *slot = Some((pattern.to_string(), re.clone()));
        re
    })
}

fn glob_to_regex(pattern: &str) -> String {
    let mut re = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    re.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    re.push_str(".*");
                    i += 2;
                }
            }
            '*' => {
                re.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                re.push_str("[^/]");
                i += 1;
            }
            c @ ('.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '{' | '}' | '\\') => {
                re.push('\\');
                re.push(c);
                i += 1;
            }
            c => {
                re.push(c);
                i += 1;
            }
        }
    }
    re.push('$');
    re
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn suffix_and_recursive() {
        assert!(glob_match("src/main.rs", "*.rs"));
        assert!(glob_match("main.rs", "*.rs"));
        assert!(!glob_match("src/main.rs", "*.toml"));
        assert!(glob_match("src/foo/bar.rs", "**/*.rs"));
        assert!(glob_match("bar.rs", "**/*.rs"));
        assert!(glob_match("src/lib.rs", "src/*.rs"));
        assert!(!glob_match("src/foo/lib.rs", "src/*.rs"));
    }
}
