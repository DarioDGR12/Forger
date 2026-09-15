//! Named, swappable capabilities. Forger does not port Cordis; the Rust
//! analogue is a trait object registered at composition time.

/// Stable name of a plugin implementation (e.g. `"openai-compat"`, `"landlock"`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginId(pub &'static str);

pub trait Plugin: Send + Sync {
    fn id(&self) -> PluginId;
}

use crate::error::ProviderError;
use crate::message::{Message, StreamEvent};
use crate::tool::ToolSpec;
use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio_util::sync::CancellationToken;

/// BYOK model backend. Implement this to add a provider — do not special-case
/// backends inside the agent loop.
#[async_trait]
pub trait Provider: Plugin {
    fn model(&self) -> &str;

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError>;
}
