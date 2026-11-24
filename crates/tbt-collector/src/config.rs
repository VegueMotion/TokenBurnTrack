use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tbt_core::PrivacyLevel;

pub const CONFIG_FILE: &str = "config.toml";
pub const DEFAULT_SYNC_INTERVAL_MINUTES: u32 = 60;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollectorConfig {
    pub privacy: PrivacyLevel,
    pub sync_interval_minutes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_json_retention_days: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub generic_jsonl: Vec<toml::Value>,
}

impl Default for CollectorConfig {
    fn default() -> CollectorConfig {
        CollectorConfig {
            privacy: PrivacyLevel::default(),
            sync_interval_minutes: DEFAULT_SYNC_INTERVAL_MINUTES,
            data_dir: None,
            raw_json_retention_days: None,
            generic_jsonl: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot access config file {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid config file {}: {source}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("cannot serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

pub fn default_config_path() -> Option<PathBuf> {
    tbt_store::paths::app_dir(tbt_core::sandbox::XdgDir::Config).map(|d| d.join(CONFIG_FILE))
}

impl CollectorConfig {
    pub fn load(path: &Path) -> Result<CollectorConfig, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CollectorConfig::default());
            }
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                });
            }
        };
        CollectorConfig::parse(&text).map_err(|source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        })
    }

    pub fn load_default() -> Result<CollectorConfig, ConfigError> {
        match default_config_path() {
            Some(path) => CollectorConfig::load(&path),
            None => Ok(CollectorConfig::default()),
        }
    }

    pub fn parse(text: &str) -> Result<CollectorConfig, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn to_toml(&self) -> Result<String, ConfigError> {
        Ok(toml::to_string_pretty(self)?)
    }

    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = self.to_toml()?;
        let io = |source| ConfigError::Io {
            path: path.to_owned(),
            source,
        };
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }

    pub fn resolve_data_dir(&self) -> Option<PathBuf> {
        let env = std::env::var_os(tbt_store::paths::DATA_DIR_ENV).filter(|v| !v.is_empty());
        match (env, &self.data_dir) {
            (Some(dir), _) => Some(PathBuf::from(dir)),
            (None, Some(dir)) => Some(dir.clone()),
            (None, None) => tbt_store::paths::default_data_dir(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_missing_files_give_defaults() {
        assert_eq!(
            CollectorConfig::parse("").unwrap(),
            CollectorConfig::default()
        );
        let dir = tempfile::tempdir().unwrap();
        let cfg = CollectorConfig::load(&dir.path().join("nope.toml")).unwrap();
        assert_eq!(cfg.sync_interval_minutes, 60);
        assert_eq!(cfg.privacy, PrivacyLevel::MetadataOnly);
        assert!(cfg.data_dir.is_none() && cfg.raw_json_retention_days.is_none());
    }

    #[test]
    fn partial_file_keeps_other_defaults() {
        let cfg = CollectorConfig::parse(
            r#"
privacy = "truncated_preview"
raw_json_retention_days = 30

[[generic_jsonl]]
id = "mytool"
glob = "*.jsonl"
"#,
        )
        .unwrap();
        assert_eq!(cfg.privacy, PrivacyLevel::TruncatedPreview);
        assert_eq!(cfg.raw_json_retention_days, Some(30));
        assert_eq!(cfg.sync_interval_minutes, 60);
        assert_eq!(cfg.generic_jsonl.len(), 1);
        assert_eq!(cfg.generic_jsonl[0]["id"].as_str(), Some("mytool"));
    }

    #[test]
    fn rejects_bad_values() {
        assert!(CollectorConfig::parse("privacy = \"everything\"").is_err());
        assert!(CollectorConfig::parse("sync_interval_minutes = -5").is_err());
    }

    #[test]
    fn save_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        let mut generic = toml::Table::new();
        generic.insert("id".into(), "x".into());
        let cfg = CollectorConfig {
            privacy: PrivacyLevel::Off,
            sync_interval_minutes: 15,
            data_dir: Some(dir.path().join("data")),
            raw_json_retention_days: Some(7),
            generic_jsonl: vec![toml::Value::Table(generic)],
        };
        cfg.save(&path).unwrap();
        assert_eq!(CollectorConfig::load(&path).unwrap(), cfg);
        assert!(!dir.path().join("sub/config.toml.tmp").exists());

        CollectorConfig::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("generic_jsonl") && !text.contains("data_dir"));
        assert_eq!(
            CollectorConfig::load(&path).unwrap(),
            CollectorConfig::default()
        );
    }

    #[test]
    fn invalid_file_reports_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "sync_interval_minutes = \"soon\"").unwrap();
        let err = CollectorConfig::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
        assert!(err.to_string().contains("config.toml"));
    }
}
