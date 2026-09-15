use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// File-backed settings. Flags and env vars override these.
/// API keys are **not** read from this file — only `FORGER_API_KEY` /
/// `OPENAI_API_KEY` / `DEEPSEEK_API_KEY`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
    pub workspace: Option<PathBuf>,
}

impl FileConfig {
    pub fn load(workspace: &Path) -> Result<Self> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let extra = std::env::var("FORGER_CONFIG").ok();
        Self::load_from(workspace, home.as_deref(), extra.as_deref().map(Path::new))
    }

    fn load_from(workspace: &Path, home: Option<&Path>, extra: Option<&Path>) -> Result<Self> {
        let mut acc = FileConfig::default();
        if let Some(home) = home {
            acc.merge(load_one(&home.join(".config/forger/config.toml"))?);
        }
        if let Some(extra) = extra {
            acc.merge(load_one(extra)?);
        }
        acc.merge(load_one(&workspace.join("forger.toml"))?);
        Ok(acc)
    }

    fn merge(&mut self, other: FileConfig) {
        if other.provider.is_some() {
            self.provider = other.provider;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
        if other.base_url.is_some() {
            self.base_url = other.base_url;
        }
        if other.workspace.is_some() {
            self.workspace = other.workspace;
        }
    }
}

fn load_one(path: &Path) -> Result<FileConfig> {
    if !path.is_file() {
        return Ok(FileConfig::default());
    }
    let raw =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_workspace_toml() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("forger.toml"),
            "provider = \"mock\"\nmodel = \"local-model\"\n",
        )
        .unwrap();
        let cfg = FileConfig::load_from(dir.path(), None, None).unwrap();
        assert_eq!(cfg.provider.as_deref(), Some("mock"));
        assert_eq!(cfg.model.as_deref(), Some("local-model"));
    }
}
