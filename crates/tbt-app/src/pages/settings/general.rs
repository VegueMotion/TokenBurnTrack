use std::path::PathBuf;

use tbt_collector::CollectorConfig;
use tbt_core::PrivacyLevel;

pub const SYNC_MIN_MINUTES: u32 = 5;
pub const SYNC_MAX_MINUTES: u32 = 1440;
pub const RETENTION_MAX_DAYS: u32 = 36_500;

pub const PRIVACY_OPTIONS: [(PrivacyLevel, &str, &str); 3] = [
    (
        PrivacyLevel::Off,
        "Off",
        "No messages are stored; only token counts and costs.",
    ),
    (
        PrivacyLevel::MetadataOnly,
        "Metadata only",
        "Role, tool name and time of each message, without any text.",
    ),
    (
        PrivacyLevel::TruncatedPreview,
        "Truncated preview",
        "Metadata plus the first 280 characters of each message.",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneralForm {
    pub privacy: PrivacyLevel,
    pub sync_interval: String,
    pub data_dir: String,
    pub retention_enabled: bool,
    pub retention_days: String,
}

impl GeneralForm {
    pub fn from_config(cfg: &CollectorConfig) -> GeneralForm {
        GeneralForm {
            privacy: cfg.privacy,
            sync_interval: cfg.sync_interval_minutes.to_string(),
            data_dir: cfg
                .data_dir
                .as_ref()
                .map(|d| d.display().to_string())
                .unwrap_or_default(),
            retention_enabled: cfg.raw_json_retention_days.is_some(),
            retention_days: cfg.raw_json_retention_days.unwrap_or(30).to_string(),
        }
    }

    pub fn apply(&self, cfg: &mut CollectorConfig) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        let interval = parse_whole(&self.sync_interval)
            .filter(|n| (SYNC_MIN_MINUTES..=SYNC_MAX_MINUTES).contains(n));
        if interval.is_none() {
            errors.push(format!(
                "Sync interval: enter whole minutes from {SYNC_MIN_MINUTES} to {SYNC_MAX_MINUTES}"
            ));
        }
        let retention = if self.retention_enabled {
            let days =
                parse_whole(&self.retention_days).filter(|n| (1..=RETENTION_MAX_DAYS).contains(n));
            if days.is_none() {
                errors.push(format!(
                    "raw_json retention: enter whole days from 1 to {RETENTION_MAX_DAYS}"
                ));
            }
            days
        } else {
            None
        };
        let dir = self.data_dir.trim();
        let data_dir = if dir.is_empty() {
            None
        } else {
            let path = crate::export::expand_home(dir, crate::export::home_dir().as_deref());
            if !path.is_absolute() {
                errors.push("Data directory: use an absolute path, or leave it empty".into());
            }
            Some(path)
        };
        if !errors.is_empty() {
            return Err(errors);
        }
        cfg.privacy = self.privacy;
        cfg.sync_interval_minutes = interval.unwrap_or(cfg.sync_interval_minutes);
        cfg.raw_json_retention_days = retention;
        cfg.data_dir = data_dir;
        Ok(())
    }
}

fn parse_whole(text: &str) -> Option<u32> {
    let s = text.trim();
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

pub fn data_dir_changed(saved: &CollectorConfig, form: &CollectorConfig) -> bool {
    saved.data_dir != form.data_dir
}

pub fn effective_data_dir(db: Option<&std::path::Path>) -> Option<PathBuf> {
    db.and_then(super::ops::data_dir_of)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_round_trips_config() {
        let cfg = CollectorConfig {
            privacy: PrivacyLevel::TruncatedPreview,
            sync_interval_minutes: 15,
            data_dir: Some(PathBuf::from("/data/tbt")),
            raw_json_retention_days: Some(7),
            generic_jsonl: Vec::new(),
        };
        let form = GeneralForm::from_config(&cfg);
        assert_eq!(form.sync_interval, "15");
        assert_eq!(form.data_dir, "/data/tbt");
        assert!(form.retention_enabled);
        let mut out = CollectorConfig::default();
        form.apply(&mut out).unwrap();
        assert_eq!(out, cfg);
    }

    #[test]
    fn defaults_leave_retention_off() {
        let form = GeneralForm::from_config(&CollectorConfig::default());
        assert!(!form.retention_enabled);
        assert_eq!(form.retention_days, "30");
        assert_eq!(form.data_dir, "");
        let mut out = CollectorConfig::default();
        form.apply(&mut out).unwrap();
        assert_eq!(out, CollectorConfig::default());
    }

    #[test]
    fn validates_ranges() {
        let base = GeneralForm::from_config(&CollectorConfig::default());
        for bad in ["4", "1441", "0", "-5", "1.5", "abc", ""] {
            let form = GeneralForm {
                sync_interval: bad.into(),
                ..base.clone()
            };
            let errors = form.apply(&mut CollectorConfig::default()).unwrap_err();
            assert!(errors[0].starts_with("Sync interval"), "{bad}");
        }
        for good in ["5", "1440", " 60 "] {
            let form = GeneralForm {
                sync_interval: good.into(),
                ..base.clone()
            };
            assert!(
                form.apply(&mut CollectorConfig::default()).is_ok(),
                "{good}"
            );
        }
        let form = GeneralForm {
            retention_enabled: true,
            retention_days: "0".into(),
            data_dir: "relative/dir".into(),
            ..base.clone()
        };
        let errors = form.apply(&mut CollectorConfig::default()).unwrap_err();
        assert_eq!(errors.len(), 2);
        let form = GeneralForm {
            retention_enabled: false,
            retention_days: "garbage".into(),
            ..base
        };
        assert!(form.apply(&mut CollectorConfig::default()).is_ok());
    }

    #[test]
    fn apply_keeps_generic_entries_and_round_trips_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg/config.toml");
        let mut cfg =
            CollectorConfig::parse("[[generic_jsonl]]\nid = \"mytool\"\ntimestamp = \"$.ts\"\n")
                .unwrap();
        let form = GeneralForm {
            privacy: PrivacyLevel::Off,
            sync_interval: "30".into(),
            data_dir: String::new(),
            retention_enabled: true,
            retention_days: "90".into(),
        };
        form.apply(&mut cfg).unwrap();
        cfg.save(&path).unwrap();
        let back = CollectorConfig::load(&path).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.generic_jsonl.len(), 1);
        assert_eq!(back.privacy, PrivacyLevel::Off);
        assert_eq!(back.sync_interval_minutes, 30);
        assert_eq!(back.raw_json_retention_days, Some(90));
        assert_eq!(GeneralForm::from_config(&back), form);
    }
}
