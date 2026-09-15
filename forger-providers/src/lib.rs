//! Model providers. Add a backend by implementing [`forger_core::Provider`] —
//! do not special-case vendors inside the agent loop.
//!
//! Goal: one OpenAI-compatible adapter covers DeepSeek, llama.cpp/Ollama in
//! compat mode, Mistral, and the rest of the BYOK surface (75+).

mod mock;
mod openai_compat;

pub use mock::{MockProvider, MockScript};
pub use openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider, ToolChoice};
