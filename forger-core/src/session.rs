use crate::message::Message;
use uuid::Uuid;

/// In-memory conversation. The loop treats this as the source of model-visible
/// history. Cancellation never leaves a dangling `assistant` tool-call without
/// matching `tool` results: incomplete steps are rolled back to a checkpoint.
#[derive(Debug, Clone)]
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
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Checkpoint(usize);
