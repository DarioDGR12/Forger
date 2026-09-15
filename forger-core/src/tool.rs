use crate::error::ToolError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// JSON Schema fragment describing tool parameters for the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Per-invocation context. The loop does not know about sandboxes; tools that
/// need one capture it at construction time.
pub struct ToolContext<'a> {
    pub workspace: &'a Path,
    pub cancel: CancellationToken,
    /// Set when the user explicitly confirmed a denylist override for this call.
    pub denylist_override: bool,
}

impl<'a> ToolContext<'a> {
    pub fn new(workspace: &'a Path, cancel: CancellationToken) -> Self {
        Self {
            workspace,
            cancel,
            denylist_override: false,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    /// Generic "this tool is dangerous" bit. Independent of the sandbox
    /// denylist — both layers can require their own confirmation.
    fn is_sensitive(&self) -> bool {
        false
    }

    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<String, ToolError>;
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.spec().name;
        self.tools.insert(name, tool);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Helper for tools that take a `path` argument relative to the workspace.
pub fn required_path(args: &Value) -> Result<PathBuf, ToolError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::MalformedArgs("missing string field `path`".into()))?;
    Ok(PathBuf::from(path))
}
