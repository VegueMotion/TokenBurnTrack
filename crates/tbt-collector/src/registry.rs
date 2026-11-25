use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use tbt_adapters::generic_jsonl::{GenericJsonlAdapter, GenericJsonlConfig};
use tbt_core::{HarnessAdapter, HarnessId};
use tbt_store::{HarnessConfig, Store};

use crate::config::CollectorConfig;

pub fn builtin_adapters(config: &CollectorConfig) -> Vec<Arc<dyn HarnessAdapter>> {
    let mut adapters: Vec<Arc<dyn HarnessAdapter>> = vec![
        Arc::new(tbt_adapters::ClaudeCodeAdapter::new()),
        Arc::new(tbt_adapters::CodexAdapter::new()),
        Arc::new(tbt_adapters::OpenCodeAdapter::new()),
        Arc::new(tbt_adapters::GeminiCliAdapter::new()),
        Arc::new(tbt_adapters::CursorAdapter::new()),
        Arc::new(tbt_adapters::CopilotCliAdapter::new()),
        Arc::new(tbt_adapters::GrokBuildAdapter::new()),
        Arc::new(tbt_adapters::AntigravityAdapter::new()),
        Arc::new(tbt_adapters::LmStudioAdapter::new()),
    ];
    adapters.extend(generic_adapters(config));
    adapters
}

pub fn generic_adapters(config: &CollectorConfig) -> Vec<Arc<dyn HarnessAdapter>> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<Arc<dyn HarnessAdapter>> = Vec::new();
    for value in &config.generic_jsonl {
        let parsed: Result<GenericJsonlConfig, _> = value.clone().try_into();
        let adapter = parsed
            .map_err(|e| e.to_string())
            .and_then(|c| GenericJsonlAdapter::new(c).map_err(|e| e.to_string()));
        match adapter {
            Ok(a) if seen.insert(a.id()) => out.push(Arc::new(a)),
            Ok(a) => tracing::warn!(harness = %a.id(), "duplicate generic_jsonl id; skipped"),
            Err(e) => tracing::warn!(error = %e, "invalid generic_jsonl entry; skipped"),
        }
    }
    out
}

#[derive(Clone, Default)]
pub struct Registry {
    adapters: Vec<Arc<dyn HarnessAdapter>>,
}

#[derive(Clone)]
pub struct ResolvedAdapter {
    pub adapter: Arc<dyn HarnessAdapter>,
    pub id: HarnessId,
    pub enabled: bool,
    pub roots: Vec<PathBuf>,
    pub custom_roots: bool,
}

impl Registry {
    pub fn new(adapters: Vec<Arc<dyn HarnessAdapter>>) -> Registry {
        let mut unique: Vec<Arc<dyn HarnessAdapter>> = Vec::with_capacity(adapters.len());
        for a in adapters {
            let id = a.id();
            if unique.iter().any(|u| u.id() == id) {
                tracing::warn!(harness = %id, "duplicate adapter id; keeping the first");
                continue;
            }
            unique.push(a);
        }
        Registry { adapters: unique }
    }

    pub fn builtin(config: &CollectorConfig) -> Registry {
        Registry::new(builtin_adapters(config))
    }

    pub fn adapters(&self) -> &[Arc<dyn HarnessAdapter>] {
        &self.adapters
    }

    pub fn get(&self, id: &HarnessId) -> Option<&Arc<dyn HarnessAdapter>> {
        self.adapters.iter().find(|a| a.id() == *id)
    }

    pub fn ids(&self) -> Vec<HarnessId> {
        self.adapters.iter().map(|a| a.id()).collect()
    }

    pub fn resolve(&self, store: &mut Store) -> tbt_store::Result<Vec<ResolvedAdapter>> {
        for a in &self.adapters {
            store.register_harness(&a.id(), a.display_name())?;
        }
        let configs: HashMap<HarnessId, HarnessConfig> = store
            .harnesses()?
            .into_iter()
            .map(|c| (c.id.clone(), c))
            .collect();
        Ok(self
            .adapters
            .iter()
            .map(|a| {
                let id = a.id();
                let config = configs.get(&id);
                let custom = config.and_then(|c| c.roots.clone());
                ResolvedAdapter {
                    adapter: Arc::clone(a),
                    enabled: config.is_none_or(|c| c.enabled),
                    custom_roots: custom.is_some(),
                    roots: custom.unwrap_or_else(|| a.default_roots()),
                    id,
                }
            })
            .collect())
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.ids()).finish()
    }
}

impl fmt::Debug for ResolvedAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedAdapter")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .field("roots", &self.roots)
            .field("custom_roots", &self.custom_roots)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tbt_core::{Cursor, ReadBatch, SourceRef};

    struct Fake(&'static str, Vec<PathBuf>);

    impl HarnessAdapter for Fake {
        fn id(&self) -> HarnessId {
            HarnessId::from_static(self.0)
        }
        fn display_name(&self) -> &str {
            self.0
        }
        fn default_roots(&self) -> Vec<PathBuf> {
            self.1.clone()
        }
        fn discover(&self, _roots: &[PathBuf]) -> tbt_core::Result<Vec<SourceRef>> {
            Ok(Vec::new())
        }
        fn read(&self, _src: &SourceRef, cursor: &Cursor) -> tbt_core::Result<ReadBatch> {
            Ok(ReadBatch::unchanged(cursor.clone()))
        }
    }

    #[test]
    fn builtin_has_claude_code() {
        let r = Registry::builtin(&CollectorConfig::default());
        assert!(r.get(&HarnessId::CLAUDE_CODE).is_some());
    }

    #[test]
    fn resolve_merges_store_config() {
        let r = Registry::new(vec![
            Arc::new(Fake("a", vec![PathBuf::from("/default/a")])),
            Arc::new(Fake("b", vec![PathBuf::from("/default/b")])),
            Arc::new(Fake("a", vec![PathBuf::from("/dup")])),
        ]);
        assert_eq!(r.adapters().len(), 2);
        let mut store = Store::open_in_memory().unwrap();
        r.resolve(&mut store).unwrap();
        let b = HarnessId::from_static("b");
        store.set_harness_enabled(&b, false).unwrap();
        let a = HarnessId::from_static("a");
        store
            .set_harness_roots(&a, Some(&[PathBuf::from("/custom")]))
            .unwrap();

        let resolved = r.resolve(&mut store).unwrap();
        assert_eq!(resolved[0].id, a);
        assert!(resolved[0].enabled && resolved[0].custom_roots);
        assert_eq!(resolved[0].roots, [PathBuf::from("/custom")]);
        assert!(!resolved[1].enabled && !resolved[1].custom_roots);
        assert_eq!(resolved[1].roots, [PathBuf::from("/default/b")]);
        assert_eq!(store.harnesses().unwrap().len(), 2);
    }
}
