//! Compact old tool results in the *model-visible* history.
//!
//! The session on disk stays complete. Only the copy sent to the provider
//! is shortened, so long `grep` / `run_command` dumps do not crowd out
//! the user's later turns.

use crate::message::{Message, Role};

/// Keep this many most-recent tool results verbatim.
pub const KEEP_RECENT_TOOL_RESULTS: usize = 6;
/// Older tool results longer than this are truncated.
pub const COMPACT_KEEP_CHARS: usize = 400;

pub fn compact_messages(messages: &[Message]) -> Vec<Message> {
    let tool_idxs: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::Tool)
        .map(|(i, _)| i)
        .collect();
    let keep_from = tool_idxs.len().saturating_sub(KEEP_RECENT_TOOL_RESULTS);
    let keep: std::collections::HashSet<usize> = tool_idxs[keep_from..].iter().copied().collect();

    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if m.role == Role::Tool
                && !keep.contains(&i)
                && m.content.chars().count() > COMPACT_KEEP_CHARS
            {
                let mut c = m.clone();
                let preview: String = c.content.chars().take(COMPACT_KEEP_CHARS).collect();
                c.content = format!("{preview}\n… [compacted old tool result]");
                c
            } else {
                m.clone()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{compact_messages, COMPACT_KEEP_CHARS, KEEP_RECENT_TOOL_RESULTS};
    use crate::message::Message;

    #[test]
    fn recent_tool_results_stay_intact() {
        let mut msgs = vec![Message::user("go")];
        for i in 0..(KEEP_RECENT_TOOL_RESULTS + 2) {
            msgs.push(Message::assistant_with_tools(
                "",
                vec![crate::message::ToolCall {
                    id: format!("c{i}"),
                    name: "grep".into(),
                    arguments: "{}".into(),
                }],
            ));
            msgs.push(Message::tool_result(
                format!("c{i}"),
                "grep",
                "x".repeat(COMPACT_KEEP_CHARS + 50),
            ));
        }
        let out = compact_messages(&msgs);
        let tools: Vec<_> = out
            .iter()
            .filter(|m| m.role == crate::message::Role::Tool)
            .collect();
        assert!(tools[0].content.contains("[compacted old tool result]"));
        assert!(tools[1].content.contains("[compacted old tool result]"));
        assert!(!tools.last().unwrap().content.contains("compacted"));
        assert_eq!(
            tools.last().unwrap().content.chars().count(),
            COMPACT_KEEP_CHARS + 50
        );
    }

    #[test]
    fn short_old_results_are_not_rewritten() {
        let msgs = vec![
            Message::user("go"),
            Message::tool_result("c0", "echo", "hi"),
        ];
        let out = compact_messages(&msgs);
        assert_eq!(out[1].content, "hi");
    }
}
