use anyhow::Result;
use forger_core::Session;
use std::path::{Path, PathBuf};

pub fn save(workspace: &Path, session: &Session) -> Result<PathBuf> {
    Ok(session.save(workspace)?)
}

pub fn load(workspace: &Path, id: &str) -> Result<Session> {
    Ok(Session::load_str(workspace, id)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use forger_core::Message;
    use tempfile::tempdir;

    #[test]
    fn roundtrip() {
        let dir = tempdir().unwrap();
        let mut s = Session::new();
        s.push(Message::user("hi"));
        let path = save(dir.path(), &s).unwrap();
        let loaded = load(dir.path(), &s.id.to_string()).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.messages().len(), 1);
        assert!(path.ends_with(format!("{}.json", s.id)));
    }
}
