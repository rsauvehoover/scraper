//! Maps validated source IDs to open databases.
//!
//! The only way an HTTP handler obtains a `SourceDatabase`. `SourceDatabase::open`
//! builds `db/{source_id}.db` by string interpolation with no validation, so a
//! source ID of `../../etc/passwd` would escape `db/`.
//!
//! This closes that by FILTERING, not by construction: `get` compares the
//! user-supplied string against the configured IDs and returns `None` for
//! anything else, so a hostile value is rejected before any path is built. The
//! interpolation in `SourceDatabase::open_query_only` is still there and would
//! still escape `db/` if it were ever reached with an unchecked string — which
//! is why that constructor is `pub(crate)` and why every handler resolves
//! through `get` first. Nothing in the type system enforces it.

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

    /// Test seam: wrap an already-open `SourceDatabase`.
    ///
    /// Unlike `from_config_for_test`, this lets a test open a real,
    /// file-backed database directly (e.g. via `SourceDatabase::open` /
    /// `open_query_only`) so a second connection to the same file can
    /// commit concurrently — the shape needed to exercise WAL visibility
    /// between a long-lived reader and a writer, which an in-memory
    /// database cannot do.
    #[cfg(test)]
    pub fn for_test(config: SourceConfig, db: SourceDatabase) -> Self {
        SourceEntry {
            config,
            db: Mutex::new(db),
        }
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
    ///
    /// A database that opens but has no scraper schema is skipped on exactly
    /// the same terms. Adding a source in the config editor and restarting
    /// before the scraper has ever run for it produces precisely that: the
    /// file may not exist at all, or exist and be empty, and every query
    /// against it fails with "no such table". Admitting it to the registry
    /// would push that failure into request handlers and the startup cache
    /// warm-up, which is how one newly configured source used to abort the
    /// process before it reached `TcpListener::bind`.
    pub fn from_config(config: &Config) -> Self {
        let mut entries = HashMap::new();
        let mut order = Vec::new();

        for source in config.enabled_sources() {
            let db = match SourceDatabase::open_query_only(&source.id) {
                Ok(db) => db,
                Err(e) => {
                    eprintln!("warning: skipping source {}: {}", source.id, e);
                    continue;
                }
            };

            match db.has_scraper_schema() {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!(
                        "warning: skipping source {}: database has no scraper schema yet; \
                         run the scraper for this source first",
                        source.id
                    );
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "warning: skipping source {}: schema check failed: {}",
                        source.id, e
                    );
                    continue;
                }
            }

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

    /// Restores the process cwd on drop, including on unwind from a panic.
    /// `open_query_only` resolves `db/` relative to the cwd, so a test that
    /// exercises the real constructor must point the cwd at a scratch
    /// directory or it would touch the repository's own `db/`.
    struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        fn change_to(dir: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(dir).unwrap();
            CwdGuard { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }

    /// The config-editor path: a source is added to `config.json` and the
    /// service restarts before the scraper has ever run for it. Startup must
    /// skip it, not abort — and must not leave a zero-byte database behind,
    /// because this process is documented as never writing to `db/`.
    #[test]
    #[serial_test::serial]
    fn skips_a_source_whose_database_file_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();

        let registry = SourceRegistry::from_config(&config_with(&["never-scraped"]));

        assert!(
            registry.get("never-scraped").is_none(),
            "a source with no database file must not resolve"
        );
        assert_eq!(registry.entries().count(), 0);
        assert!(
            !std::path::Path::new("db/never-scraped.db").exists(),
            "opening for read must not create the database file"
        );
    }

    /// The same source one step later: the file exists (someone touched it, or
    /// an older build created it) but nothing ever created the tables. Every
    /// query against it fails with "no such table", so it must be skipped on
    /// the same terms as a file that cannot be opened at all.
    #[test]
    #[serial_test::serial]
    fn skips_a_source_whose_database_has_no_schema() {
        let dir = tempfile::tempdir().unwrap();
        let _cwd = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        std::fs::write("db/empty-source.db", b"").unwrap();

        let registry = SourceRegistry::from_config(&config_with(&["empty-source"]));

        assert!(
            registry.get("empty-source").is_none(),
            "a schema-less database must not resolve"
        );
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
