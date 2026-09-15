//! Assemble OpenAI-style streaming `tool_calls` fragments by `index`.
//!
//! Chat Completions SSE chunks carry `choices[0].delta.tool_calls[]`. Each
//! entry has:
//!
//! - `index` (required in the spec; default 0 if a backend omits it): which
//!   parallel call this fragment belongs to
//! - `id` / `function.name`: usually on the first fragment; may still be
//!   split across chunks as strings
//! - `function.arguments`: JSON **string fragments** that MUST be concatenated
//!   in arrival order and parsed only after the stream finishes
//!
//! Parallel calls are keyed **only** by `index`. Chunks for different indices
//! may arrive interleaved; a single chunk may contain several indices.

use crate::message::{StreamEvent, ToolCall};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: String,
    name: String,
    arguments: String,
}

/// Accumulates [`StreamEvent::ToolCallDelta`] fragments into complete tool calls.
#[derive(Debug, Default, Clone)]
pub struct ToolCallAccumulator {
    slots: BTreeMap<usize, PartialToolCall>,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn apply(
        &mut self,
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    ) {
        let slot = self.slots.entry(index).or_default();
        if let Some(id) = id {
            slot.id.push_str(&id);
        }
        if let Some(name) = name {
            slot.name.push_str(&name);
        }
        if let Some(arguments) = arguments {
            slot.arguments.push_str(&arguments);
        }
    }

    pub fn apply_event(&mut self, event: &StreamEvent) {
        if let StreamEvent::ToolCallDelta {
            index,
            id,
            name,
            arguments,
        } = event
        {
            self.apply(*index, id.clone(), name.clone(), arguments.clone());
        }
    }

    /// Stable order by `index` (0, 1, 2, …).
    pub fn finish(self) -> Vec<ToolCall> {
        self.slots
            .into_iter()
            .filter(|(_, partial)| {
                !(partial.id.is_empty() && partial.name.is_empty() && partial.arguments.is_empty())
            })
            .map(|(index, partial)| ToolCall {
                id: if partial.id.is_empty() {
                    format!("call_{index}")
                } else {
                    partial.id
                },
                name: partial.name,
                arguments: partial.arguments,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concatenates_argument_fragments_for_one_index() {
        let mut acc = ToolCallAccumulator::new();
        acc.apply(
            0,
            Some("call_weather".into()),
            Some("get_weather".into()),
            Some(String::new()),
        );
        acc.apply(0, None, None, Some("{\"location\":".into()));
        acc.apply(0, None, None, Some("\"Tokyo\"}".into()));
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_weather");
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments, "{\"location\":\"Tokyo\"}");
    }

    #[test]
    fn interleaves_parallel_tool_calls_by_index() {
        let mut acc = ToolCallAccumulator::new();
        acc.apply(
            1,
            Some("call_time".into()),
            Some("get_time".into()),
            Some(String::new()),
        );
        acc.apply(
            0,
            Some("call_weather".into()),
            Some("get_weather".into()),
            Some(String::new()),
        );
        acc.apply(0, None, None, Some("{\"city\":\"Tok".into()));
        acc.apply(1, None, None, Some("{\"tz\":\"U".into()));
        acc.apply(0, None, None, Some("yo\"}".into()));
        acc.apply(1, None, None, Some("TC\"}".into()));
        let calls = acc.finish();
        assert_eq!(calls[0].id, "call_weather");
        assert_eq!(calls[0].arguments, "{\"city\":\"Tokyo\"}");
        assert_eq!(calls[1].id, "call_time");
        assert_eq!(calls[1].arguments, "{\"tz\":\"UTC\"}");
    }

    #[test]
    fn concatenates_fragmented_function_name() {
        let mut acc = ToolCallAccumulator::new();
        acc.apply(0, Some("c1".into()), Some("get_".into()), None);
        acc.apply(0, None, Some("weather".into()), Some("{}".into()));
        assert_eq!(acc.finish()[0].name, "get_weather");
    }
}
