use crate::message::Message;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Conversation history. The loop treats this as the source of model-visible
/// history (after compacting old tool results). Cancellation never leaves a
/// dangling `assistant` tool-call without matching `tool` results: incomplete
/// steps are rolled back to a checkpoint.
///
/// Persisted under `{workspace}/.forger/sessions/{id}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    #[serde(default)]
    messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            messages: Vec::new(),
        }
    }

    pub fn with_id(id: Uuid) -> Self {
        Self {
            id,
            messages: Vec::new(),
        }
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn truncate(&mut self, len: usize) {
        self.messages.truncate(len);
    }

    pub fn checkpoint(&self) -> Checkpoint {
        Checkpoint(self.messages.len())
    }

    pub fn restore(&mut self, checkpoint: Checkpoint) {
        self.messages.truncate(checkpoint.0);
    }

    pub fn last_assistant_text(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|m| m.role == crate::message::Role::Assistant && !m.has_tool_calls())
            .map(|m| m.content.as_str())
    }

    pub fn store_dir(workspace: &Path) -> PathBuf {
        workspace.join(".forger").join("sessions")
    }

    pub fn file_path(workspace: &Path, id: Uuid) -> PathBuf {
        Self::store_dir(workspace).join(format!("{id}.json"))
    }

    /// Atomic-ish write: temp file then rename.
    pub fn save_to(&self, workspace: &Path) -> io::Result<PathBuf> {
        let dir = Self::store_dir(workspace);
        fs::create_dir_all(&dir)?;
        let path = Self::file_path(workspace, self.id);
        let tmp = dir.join(format!("{}.json.tmp", self.id));
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn load_from(workspace: &Path, id: Uuid) -> io::Result<Self> {
        let path = Self::file_path(workspace, id);
        let bytes = fs::read(&path)?;
        let session: Session = serde_json::from_slice(&bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if session.id != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("session file id {} does not match {id}", session.id),
            ));
        }
        Ok(session)
    }

    pub fn load_from_str(workspace: &Path, id: &str) -> io::Result<Self> {
        let id = Uuid::parse_str(id.trim()).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid session id: {e}"),
            )
        })?;
        Self::load_from(workspace, id)
    }

    /// Load from disk, or start an empty session with `id` when the file is missing.
    pub fn load_or_create(workspace: &Path, id: &str) -> Self {
        if let Ok(s) = Self::load_from_str(workspace, id) {
            return s;
        }
        Uuid::parse_str(id.trim())
            .map(Self::with_id)
            .unwrap_or_else(|_| Self::new())
    }

    pub fn list_saved(workspace: &Path) -> io::Result<Vec<Uuid>> {
        let dir = Self::store_dir(workspace);
        let rd = match fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut ids = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name();
            let Some(s) = name.to_str() else {
                continue;
            };
            let Some(stem) = s.strip_suffix(".json") else {
                continue;
            };
            if stem.ends_with(".tmp") {
                continue;
            }
            if let Ok(id) = Uuid::parse_str(stem) {
                ids.push(id);
            }
        }
        ids.sort();
        Ok(ids)
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Checkpoint(usize);

#[cfg(test)]
mod tests {
    use super::Session;
    use crate::message::Message;

    #[test]
    fn roundtrip_json() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new();
        s.push(Message::user("hi"));
        s.push(Message::assistant("hello"));
        s.save_to(dir.path()).unwrap();
        let loaded = Session::load_from(dir.path(), s.id).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.messages().len(), 2);
        assert_eq!(loaded.messages()[0].content, "hi");
        assert_eq!(Session::list_saved(dir.path()).unwrap(), vec![s.id]);
    }

    #[test]
    fn missing_session_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let err = Session::load_from(dir.path(), uuid::Uuid::new_v4()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
