//! Maps validated source IDs to open databases.
//!
//! The only way an HTTP handler obtains a `SourceDatabase`. `SourceDatabase::open`
//! builds `db/{source_id}.db` by string interpolation with no validation, so a
//! source ID of `../../etc/passwd` would escape `db/`. Resolving through this
//! registry means a user-supplied string is only ever compared against the
//! configured IDs, never interpolated into a path.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::config::{Config, SourceConfig};
use crate::db::SourceDatabase;

pub struct SourceEntry {
    pub config: SourceConfig,
    // `rusqlite::Connection` is `Send` but not `Sync` (it wraps a `RefCell`),
    // so `SourceDatabase` cannot be shared across threads directly. A later
    // task puts this registry behind `Arc<AppState>` as axum state, which
    // requires `Send + Sync`. `Mutex<T>` is `Sync` whenever `T: Send`, so the
    // Mutex is load-bearing here, not incidental — do not remove it.
    //
    // Private: `db()` is the only route in, so the mutex-poisoning policy is
    // decided once, here, rather than re-decided at every call site.
    db: Mutex<SourceDatabase>,
}

impl SourceEntry {
    /// Lock this source's database.
    ///
    /// Poisoning is recovered rather than propagated. A query-only SQLite read
    /// is not left structurally broken by a Rust-side panic, so one panicking
    /// request must not permanently fail every later request for this source.
    pub fn db(&self) -> std::sync::MutexGuard<'_, SourceDatabase> {
        self.db
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub struct SourceRegistry {
    entries: HashMap<String, SourceEntry>,
    /// Config order, for stable display.
    order: Vec<String>,
}

impl SourceRegistry {
    /// Open a query-only database for every enabled source in `config`.
    ///
    /// A source whose database cannot be opened is skipped with a warning
    /// rather than aborting startup: one unreadable database should not
    /// take the whole frontend down.
    pub fn from_config(config: &Config) -> Self {
        let mut entries = HashMap::new();
        let mut order = Vec::new();

        for source in config.enabled_sources() {
            match SourceDatabase::open_query_only(&source.id) {
                Ok(db) => {
                    order.push(source.id.clone());
                    entries.insert(
                        source.id.clone(),
                        SourceEntry {
                            config: source.clone(),
                            db: Mutex::new(db),
                        },
                    );
                }
                Err(e) => {
                    eprintln!("warning: skipping source {}: {}", source.id, e);
                }
            }
        }

        SourceRegistry { entries, order }
    }

    /// Test seam: build a registry without opening real database files.
    #[cfg(test)]
    pub fn from_config_for_test(config: &Config) -> Self {
        let mut entries = HashMap::new();
        let mut order = Vec::new();
        for source in config.enabled_sources() {
            let db = SourceDatabase::open_in_memory(&source.id).unwrap();
            order.push(source.id.clone());
            entries.insert(
                source.id.clone(),
                SourceEntry {
                    config: source.clone(),
                    db: Mutex::new(db),
                },
            );
        }
        SourceRegistry { entries, order }
    }

    /// Resolve a user-supplied source ID. `None` for anything not configured.
    pub fn get(&self, source_id: &str) -> Option<&SourceEntry> {
        self.entries.get(source_id)
    }

    /// Entries in config order.
    pub fn entries(&self) -> impl Iterator<Item = &SourceEntry> {
        self.order.iter().filter_map(move |id| self.entries.get(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SourceConfig};

    fn config_with(ids: &[&str]) -> Config {
        let mut config = Config::default();
        config.sources = ids
            .iter()
            .map(|id| SourceConfig {
                id: id.to_string(),
                enabled: true,
                ..SourceConfig::default()
            })
            .collect();
        config
    }

    #[test]
    fn rejects_path_traversal_in_source_id() {
        let registry = SourceRegistry::from_config_for_test(&config_with(&["wandering-inn"]));

        // These are the shapes an attacker sends. None may resolve.
        for hostile in [
            "../../etc/passwd",
            "../wandering-inn",
            "/etc/passwd",
            "wandering-inn/../../../etc/passwd",
            "..",
            ".",
            "",
        ] {
            assert!(
                registry.get(hostile).is_none(),
                "source id {:?} must not resolve",
                hostile
            );
        }
    }

    #[test]
    fn rejects_unknown_but_harmless_source_id() {
        let registry = SourceRegistry::from_config_for_test(&config_with(&["wandering-inn"]));
        assert!(registry.get("royal-road-nope").is_none());
    }

    #[test]
    fn resolves_configured_source_id() {
        let registry = SourceRegistry::from_config_for_test(&config_with(&["wandering-inn"]));
        let entry = registry.get("wandering-inn").expect("configured id resolves");
        assert_eq!(entry.config.id, "wandering-inn");
    }

    /// `SourceRegistry` goes into `Arc<AppState>` as axum state, which requires
    /// `Send + Sync`. `rusqlite::Connection` wraps a `RefCell` and is not
    /// `Sync`, so `SourceEntry.db` must stay wrapped in a `Mutex`. This fails
    /// to compile if that wrapper is ever removed.
    #[test]
    fn registry_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<SourceRegistry>();
        assert_send_sync::<SourceEntry>();
    }
}
