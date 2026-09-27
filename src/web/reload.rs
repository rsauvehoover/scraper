//! Keeps the source registry in step with `config.json` and with the
//! databases on disk, so neither a config edit nor a source's first scrape
//! needs a restart to show up.
//!
//! Handlers take a `snapshot()` and use it for the whole request. A rebuild
//! swaps a new registry in for later requests; a request already holding the
//! old one finishes on it, and the old database connections close when the
//! last snapshot is dropped.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, RwLock, TryLockError};
use std::time::{Duration, Instant};

use crate::config::{parse_config, Config, ConfigLoadError, MailConfig};
use crate::db::SourceRegistry;

/// See `LiveRegistry::snapshot_and_mail`.
pub struct SendSnapshot {
    pub registry: Arc<SourceRegistry>,
    pub mail: MailConfig,
    pub load_error: Option<ConfigLoadError>,
}

pub struct LiveRegistry {
    current: RwLock<Arc<SourceRegistry>>,
    // Held for the length of a check, rebuild included, so only one request
    // ever rebuilds at a time.
    control: Mutex<Control>,
    // `None` for a registry that never reloads.
    config_path: Option<PathBuf>,
    min_interval: Duration,
}

struct Control {
    last_check: Option<Instant>,
    // What the current registry was built from. `None` bytes means there
    // was no file and it was built from the defaults.
    applied_bytes: Option<Vec<u8>>,
    applied: Config,
    // Bytes on disk that failed to load. Remembered so one bad file is
    // parsed and logged once, not on every check.
    rejected_bytes: Option<Vec<u8>>,
    error: Option<ConfigLoadError>,
}

impl LiveRegistry {
    /// Build from the file at `config_path`.
    ///
    /// A missing file gives the defaults, as the scraper does. A file that
    /// exists but will not load is an error here, unlike on a reload: at
    /// startup there is no earlier configuration to fall back on.
    pub fn load(config_path: PathBuf, min_interval: Duration) -> Result<Self, ConfigLoadError> {
        let (applied_bytes, applied) = match std::fs::read(&config_path) {
            Ok(bytes) => {
                let config = parse_config(&bytes)?;
                (Some(bytes), config)
            }
            Err(e) => match ConfigLoadError::from_io(&e) {
                ConfigLoadError::Missing => {
                    println!("No {} found, using default values", config_path.display());
                    (None, Config::default())
                }
                other => return Err(other),
            },
        };

        Ok(LiveRegistry {
            current: RwLock::new(Arc::new(SourceRegistry::from_config(&applied))),
            control: Mutex::new(Control {
                last_check: Some(Instant::now()),
                applied_bytes,
                applied,
                rejected_bytes: None,
                error: None,
            }),
            config_path: Some(config_path),
            min_interval,
        })
    }

    /// A registry that never reloads.
    #[cfg(test)]
    pub fn fixed(registry: SourceRegistry) -> Self {
        LiveRegistry {
            current: RwLock::new(Arc::new(registry)),
            control: Mutex::new(Control {
                last_check: None,
                applied_bytes: None,
                applied: Config::default(),
                rejected_bytes: None,
                error: None,
            }),
            config_path: None,
            min_interval: Duration::MAX,
        }
    }

    /// A registry that never reloads, built from `config`, destinations
    /// included.
    #[cfg(test)]
    pub fn fixed_with_config(config: Config) -> Self {
        let live = LiveRegistry::fixed(SourceRegistry::from_config_for_test(&config));
        live.control.lock().unwrap().applied = config;
        live
    }

    /// The registry, the mail settings of the configuration in use, and why
    /// the file on disk is not that configuration, all from the same reload.
    /// A send reads all three, and must not pair sources from one config with
    /// destinations from another, nor send to the last good destinations
    /// because the error was read after a reload cleared it.
    ///
    /// Unlike `snapshot`, this waits for a reload in progress: the three can
    /// only be read consistently under the lock that swaps them.
    pub fn snapshot_and_mail(&self) -> SendSnapshot {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        self.check_if_due(&mut control);
        SendSnapshot {
            registry: self.current(),
            mail: control.applied.mail.clone(),
            load_error: control.error.clone(),
        }
    }

    /// The registry to use for this request, reloading first if a check is
    /// due.
    ///
    /// Never waits on another request's rebuild: if a check is already in
    /// progress, this returns the registry as it stands.
    pub fn snapshot(&self) -> Arc<SourceRegistry> {
        match self.control.try_lock() {
            Ok(mut control) => self.check_if_due(&mut control),
            Err(TryLockError::Poisoned(p)) => self.check_if_due(&mut p.into_inner()),
            Err(TryLockError::WouldBlock) => {}
        }
        self.current()
    }

    /// Check now, ignoring the interval, and return why the file on disk is
    /// not in use, if it isn't. The config editor calls this after a save, so
    /// the page it redirects to already reflects the change.
    pub fn reload_now(&self) -> Option<ConfigLoadError> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        control.last_check = Some(Instant::now());
        self.refresh(&mut control);
        control.error.clone()
    }

    /// Why the file on disk is not the configuration in use, if it isn't.
    pub fn load_error(&self) -> Option<ConfigLoadError> {
        self.control
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .error
            .clone()
    }

    /// Names of destinations in the configuration in use that list no
    /// sources, and so are sent nothing. See `UserConfig::receives_source`.
    pub fn destinations_sent_nothing(&self) -> Vec<String> {
        self.control
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .applied
            .mail
            .destinations
            .iter()
            .filter(|d| d.sources.is_empty())
            .map(|d| d.name.clone())
            .collect()
    }

    fn current(&self) -> Arc<SourceRegistry> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    fn swap(&self, registry: SourceRegistry) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(registry);
    }

    fn check_if_due(&self, control: &mut Control) {
        let now = Instant::now();
        if let Some(last) = control.last_check {
            if now.duration_since(last) < self.min_interval {
                return;
            }
        }
        control.last_check = Some(now);
        self.refresh(control);
    }

    fn refresh(&self, control: &mut Control) {
        let Some(path) = &self.config_path else {
            return;
        };

        let on_disk = match std::fs::read(path) {
            Ok(bytes) => Some(bytes),
            Err(e) => match ConfigLoadError::from_io(&e) {
                ConfigLoadError::Missing => None,
                other => return self.reject(control, None, other),
            },
        };

        if on_disk == control.applied_bytes {
            // The configuration in use is the one on disk.
            if control.error.take().is_some() {
                println!(
                    "{} matches the configuration in use again",
                    path.display()
                );
            }
            control.rejected_bytes = None;
            if self.current().a_skipped_source_is_now_ready() {
                println!("a source waiting for its first scrape is ready; rebuilding the source list");
                self.swap(SourceRegistry::from_config(&control.applied));
            }
            return;
        }

        let Some(bytes) = on_disk else {
            // It was there and now isn't. Keep what is in use.
            return self.reject(control, None, ConfigLoadError::Missing);
        };
        if control.rejected_bytes.as_ref() == Some(&bytes) {
            return;
        }

        match parse_config(&bytes) {
            Ok(config) => {
                self.swap(SourceRegistry::from_config(&config));
                control.applied = config;
                control.applied_bytes = Some(bytes);
                control.rejected_bytes = None;
                control.error = None;
                println!("{} changed; source list rebuilt", path.display());
            }
            Err(e) => self.reject(control, Some(bytes), e),
        }
    }

    fn reject(&self, control: &mut Control, bytes: Option<Vec<u8>>, error: ConfigLoadError) {
        // `error` carries no text from the file. See `ConfigLoadError`.
        if control.error.as_ref() != Some(&error) {
            eprintln!(
                "warning: config not applied ({}); still using the last configuration that loaded",
                error
            );
        }
        control.rejected_bytes = bytes;
        control.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::CwdGuard;

    use super::*;
    use crate::db::SourceDatabase;
    use serial_test::serial;

    const CONFIG: &str = "config.json";

    fn write_config(ids: &[&str]) {
        let sources: Vec<String> = ids
            .iter()
            .map(|id| {
                format!(
                    r#"{{"Id": "{}", "Name": "Source {}", "Enabled": true}}"#,
                    id, id
                )
            })
            .collect();
        std::fs::write(CONFIG, format!(r#"{{"Sources": [{}]}}"#, sources.join(", "))).unwrap();
    }

    /// Give `id` a database with the scraper's schema, as its first scrape
    /// would.
    fn scrape(id: &str) {
        SourceDatabase::open(id).unwrap();
    }

    fn ids(registry: &SourceRegistry) -> Vec<String> {
        registry.entries().map(|e| e.config.id.clone()).collect()
    }

    fn scratch() -> (tempfile::TempDir, CwdGuard) {
        let dir = tempfile::tempdir().unwrap();
        let guard = CwdGuard::change_to(dir.path());
        std::fs::create_dir_all("db").unwrap();
        (dir, guard)
    }

    fn live(interval: Duration) -> LiveRegistry {
        LiveRegistry::load(PathBuf::from(CONFIG), interval).unwrap()
    }

    #[test]
    #[serial]
    fn an_edit_is_picked_up_without_a_restart() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        scrape("beta");
        write_config(&["alpha"]);
        let registry = live(Duration::ZERO);
        assert_eq!(ids(&registry.snapshot()), ["alpha"]);

        write_config(&["alpha", "beta"]);

        assert_eq!(ids(&registry.snapshot()), ["alpha", "beta"]);
        assert_eq!(registry.load_error(), None);
    }

    #[test]
    #[serial]
    fn an_unchanged_file_does_not_rebuild() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        // "waiting" has no database, so every check probes it and finds it
        // still not ready. That must not rebuild either.
        write_config(&["alpha", "waiting"]);
        let registry = live(Duration::ZERO);

        let first = registry.snapshot();
        let second = registry.snapshot();

        assert!(
            Arc::ptr_eq(&first, &second),
            "nothing changed, so the same registry must be served"
        );
    }

    #[test]
    #[serial]
    fn checks_are_throttled_but_reload_now_is_not() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        scrape("beta");
        write_config(&["alpha"]);
        let registry = live(Duration::from_secs(3600));

        write_config(&["alpha", "beta"]);
        assert_eq!(
            ids(&registry.snapshot()),
            ["alpha"],
            "within the interval, a request must not re-read the file"
        );

        assert_eq!(registry.reload_now(), None);
        assert_eq!(ids(&registry.snapshot()), ["alpha", "beta"]);
    }

    #[test]
    #[serial]
    fn a_bad_edit_keeps_the_last_good_configuration() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        scrape("beta");
        write_config(&["alpha"]);
        let registry = live(Duration::ZERO);

        std::fs::write(CONFIG, r#"{"Sources": ["#).unwrap();
        assert_eq!(ids(&registry.snapshot()), ["alpha"]);
        assert!(
            matches!(registry.load_error(), Some(ConfigLoadError::Invalid { .. })),
            "{:?}",
            registry.load_error()
        );
        // A send reads the error with the last good configuration.
        let send = registry.snapshot_and_mail();
        assert_eq!(ids(&send.registry), ["alpha"]);
        assert!(matches!(send.load_error, Some(ConfigLoadError::Invalid { .. })));

        // Fixing the file clears the error and applies the new content.
        write_config(&["alpha", "beta"]);
        assert_eq!(ids(&registry.snapshot()), ["alpha", "beta"]);
        assert_eq!(registry.load_error(), None);
        assert_eq!(registry.snapshot_and_mail().load_error, None);
    }

    #[test]
    #[serial]
    fn a_deleted_file_keeps_the_last_good_configuration() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        write_config(&["alpha"]);
        let registry = live(Duration::ZERO);

        std::fs::remove_file(CONFIG).unwrap();

        assert_eq!(
            ids(&registry.snapshot()),
            ["alpha"],
            "a missing file must not reset the page to the defaults"
        );
        assert_eq!(registry.load_error(), Some(ConfigLoadError::Missing));
    }

    #[test]
    #[serial]
    fn a_source_appears_after_its_first_scrape() {
        let (_dir, _cwd) = scratch();
        scrape("alpha");
        write_config(&["alpha", "gamma"]);
        let registry = live(Duration::ZERO);
        assert_eq!(ids(&registry.snapshot()), ["alpha"]);
        assert_eq!(registry.snapshot().skipped().len(), 1);

        // The config does not change. Only gamma's database appears.
        scrape("gamma");

        let after = registry.snapshot();
        assert_eq!(ids(&after), ["alpha", "gamma"]);
        assert!(after.skipped().is_empty());
    }

    #[test]
    #[serial]
    fn a_file_that_will_not_load_at_startup_is_an_error() {
        let (_dir, _cwd) = scratch();
        std::fs::write(CONFIG, r#"{"Sources": ["#).unwrap();
        assert!(matches!(
            LiveRegistry::load(PathBuf::from(CONFIG), Duration::ZERO),
            Err(ConfigLoadError::Invalid { .. })
        ));
    }

    #[test]
    #[serial]
    fn a_missing_file_at_startup_gives_the_defaults() {
        let (_dir, _cwd) = scratch();
        let registry = live(Duration::ZERO);
        assert_eq!(registry.snapshot().entries().count(), 0);
        assert_eq!(registry.load_error(), None);
    }

    #[test]
    #[serial]
    fn mail_settings_follow_an_edit_with_the_sources() {
        let (_dir, _cwd) = scratch();
        write_config(&["first-source"]);
        let live = live(Duration::ZERO);
        let mail = live.snapshot_and_mail().mail;
        assert!(mail.destinations.is_empty());

        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(CONFIG).unwrap()).unwrap();
        doc["Mail"] = serde_json::json!({
            "Name": "Example Sender", "Address": "sender@example.com", "Password": "",
            "SmtpHostname": "smtp.example.com", "SmtpPort": 587,
            "Destinations": [{ "Name": "Test Reader", "Email": "reader@example.com",
                               "Sources": { "first-source": {} } }]
        });
        std::fs::write(CONFIG, serde_json::to_vec(&doc).unwrap()).unwrap();

        let mail = live.snapshot_and_mail().mail;
        assert_eq!(mail.destinations.len(), 1);
        assert_eq!(mail.destinations[0].email, "reader@example.com");
    }

    #[test]
    fn a_fixed_registry_can_carry_destinations() {
        let mut config = Config::default();
        config.mail.destinations.push(crate::config::UserConfig {
            name: "Test Reader".into(),
            email: "reader@example.com".into(),
            ..Default::default()
        });
        let live = LiveRegistry::fixed_with_config(config);
        assert_eq!(live.snapshot_and_mail().mail.destinations[0].name, "Test Reader");
    }
}
