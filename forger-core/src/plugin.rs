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
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Shared BYOK backend. Supertraits on [`Plugin`] / [`Provider`] are `Send + Sync`,
/// so this object is safe to clone into `tokio::spawn` tasks (quality mode).
pub type SharedProvider = Arc<dyn Provider + Send + Sync>;

/// BYOK model backend. Implement this to add a provider — do not special-case
/// backends inside the agent loop.
///
/// `Send + Sync` (via [`Plugin`]) is required so quality-mode can wrap the
/// backend in [`SharedProvider`] and run N agent loops on real tokio tasks.
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

impl<T: Plugin + ?Sized> Plugin for Arc<T> {
    fn id(&self) -> PluginId {
        (**self).id()
    }
}

#[async_trait]
impl<T: Provider + ?Sized> Provider for Arc<T> {
    fn model(&self) -> &str {
        (**self).model()
    }

    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<BoxStream<'static, Result<StreamEvent, ProviderError>>, ProviderError> {
        (**self).stream(messages, tools, cancel).await
    }
}
