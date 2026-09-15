use crate::message::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub fn sessions_dir(workspace: &Path) -> PathBuf {
    workspace.join(".forger").join("sessions")
}

#[derive(Debug, thiserror::Error)]
pub enum SessionIoError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("session id must be a UUID: {0}")]
    InvalidId(String),
}

/// In-memory conversation. The loop treats this as the source of model-visible
/// history. Cancellation never leaves a dangling `assistant` tool-call without
/// matching `tool` results: incomplete steps are rolled back to a checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
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

    pub fn from_json(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn save(&self, workspace: &Path) -> Result<PathBuf, SessionIoError> {
        let dir = sessions_dir(workspace);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", self.id));
        std::fs::write(&path, self.to_json()?)?;
        Ok(path)
    }

    pub fn load(workspace: &Path, id: Uuid) -> Result<Self, SessionIoError> {
        let path = sessions_dir(workspace).join(format!("{id}.json"));
        let bytes = std::fs::read(&path)?;
        Ok(Self::from_json(&bytes)?)
    }

    pub fn load_str(workspace: &Path, id: &str) -> Result<Self, SessionIoError> {
        let uuid = Uuid::parse_str(id).map_err(|e| SessionIoError::InvalidId(e.to_string()))?;
        Self::load(workspace, uuid)
    }

    /// Load from disk, or start an empty session with this UUID if the file is missing.
    pub fn load_or_create(workspace: &Path, id: &str) -> Result<Self, SessionIoError> {
        match Self::load_str(workspace, id) {
            Ok(s) => Ok(s),
            Err(SessionIoError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                let uuid =
                    Uuid::parse_str(id).map_err(|e| SessionIoError::InvalidId(e.to_string()))?;
                Ok(Self::with_id(uuid))
            }
            Err(e) => Err(e),
        }
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
    use super::*;
    use crate::message::Message;

    #[test]
    fn json_roundtrip_keeps_id_and_messages() {
        let mut s = Session::new();
        s.push(Message::user("hi"));
        let loaded = Session::from_json(&s.to_json().unwrap()).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.messages().len(), 1);
        assert_eq!(loaded.messages()[0].content, "hi");
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new();
        s.push(Message::user("persist me"));
        s.save(dir.path()).unwrap();
        let loaded = Session::load_str(dir.path(), &s.id.to_string()).unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.messages()[0].content, "persist me");
    }

    #[test]
    fn load_or_create_missing_uuid_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let s = Session::load_or_create(dir.path(), &id.to_string()).unwrap();
        assert_eq!(s.id, id);
        assert!(s.is_empty());
    }
}
