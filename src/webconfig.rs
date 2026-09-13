//! Reading and writing `config.json` for the web editor.
//!
//! Everything here operates on `serde_json::Value`, never on the typed structs
//! in `crate::config`. Three reasons, all load-bearing:
//!
//! 1. Those structs have no `Serialize` derive.
//! 2. `load_config()` mutates `epub_gen` from the mail destinations before
//!    returning, so serialising its output would bake derived values into the file.
//! 3. The live config contains `Sources[].Selectors.IgnoredVolumes`, which the
//!    `Selectors` struct does not model. Serde drops unknown fields silently, so
//!    a typed round-trip would delete it from disk.
//!
//! Validation still goes through the typed structs — we parse the candidate to
//! prove the scraper could load it, then write the `Value`. Validation without
//! normalisation.

use std::io::{self, Write};
use std::path::Path;

use serde_json::Value;

use crate::config::Config;

/// Read `config.json` with the password removed.
///
/// The existing password is never sent to a client. The client learns only
/// whether one is set, so the editor can render "leave blank to keep".
pub fn read_redacted(path: &Path) -> io::Result<Value> {
    let raw = std::fs::read_to_string(path)?;
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    if let Some(mail) = value.get_mut("Mail").and_then(|m| m.as_object_mut()) {
        let was_set = mail
            .remove("Password")
            .and_then(|p| p.as_str().map(|s| !s.is_empty()))
            .unwrap_or(false);
        mail.insert("PasswordSet".to_string(), Value::Bool(was_set));
    }

    Ok(value)
}

/// Splice the stored password back into a candidate config.
///
/// A blank or absent `Mail.Password` means "leave unchanged". The editor's
/// password field is write-only, so this is the normal path, not the exception.
pub fn merge_password(candidate: &mut Value, current: &Value) {
    let supplied = candidate
        .get("Mail")
        .and_then(|m| m.get("Password"))
        .and_then(|p| p.as_str())
        .unwrap_or("");

    if !supplied.is_empty() {
        // Caller supplied a new password; keep it.
        if let Some(mail) = candidate.get_mut("Mail").and_then(|m| m.as_object_mut()) {
            mail.remove("PasswordSet");
        }
        return;
    }

    let existing = current
        .get("Mail")
        .and_then(|m| m.get("Password"))
        .cloned()
        .unwrap_or_else(|| Value::String(String::new()));

    if let Some(mail) = candidate.get_mut("Mail").and_then(|m| m.as_object_mut()) {
        mail.remove("PasswordSet");
        mail.insert("Password".to_string(), existing);
    }
}

/// Prove the scraper could load this config.
///
/// Parses into the typed `Config`; the `Value` is what actually gets written.
pub fn validate(candidate: &Value) -> Result<(), String> {
    serde_json::from_value::<Config>(candidate.clone())
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Top-level keys whose contents differ. Names only — never values.
///
/// Feeds the audit log, which must never become a second copy of the password.
pub fn changed_keys(before: &Value, after: &Value) -> Vec<String> {
    let empty = serde_json::Map::new();
    let b = before.as_object().unwrap_or(&empty);
    let a = after.as_object().unwrap_or(&empty);

    let mut keys: Vec<String> = a
        .iter()
        .filter(|(k, v)| b.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    keys.extend(b.keys().filter(|k| !a.contains_key(*k)).cloned());
    keys.sort();
    keys.dedup();
    keys
}

/// Write `config.json` atomically, preserving its mode.
///
/// Temp file in the same directory, then rename. Rename is atomic, so the
/// hourly scraper sees either the old file or the new one — never a torn read,
/// and never an absent one. The absent case matters most: `load_config()`
/// panics on a malformed file but silently falls back to defaults on a missing
/// one, which would scrape with the wrong settings and mail the results.
///
/// The temp file name includes the process id, which is enough to avoid
/// collisions between separate runs of this process but not between two
/// concurrent writers racing each other, and a crash between creating the
/// temp file and the rename leaves it behind uncollected. Both are accepted
/// for this single-writer deployment rather than left unstated.
pub fn write_atomic(path: &Path, value: &Value) -> io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));

    // Preserve the previous file before replacing it.
    if path.exists() {
        let backup = path.with_extension("json.bak");
        std::fs::copy(path, &backup)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600))?;
        }
    }

    let existing_mode = current_mode(path);

    let tmp = dir.join(format!("config.json.tmp.{}", std::process::id()));
    {
        let mut file = open_private(&tmp)?;
        let body = serde_json::to_string_pretty(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        file.write_all(body.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }

    apply_mode(&tmp, existing_mode)?;
    std::fs::rename(&tmp, path)?;

    // Durability of the rename itself.
    if let Ok(dir_handle) = std::fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

#[cfg(unix)]
fn current_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .ok()
        .map(|m| m.permissions().mode() & 0o777)
}

#[cfg(not(unix))]
fn current_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    std::fs::File::create(path)
}

#[cfg(unix)]
fn apply_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = mode.unwrap_or(0o600);
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn apply_mode(_path: &Path, _mode: Option<u32>) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Obviously-synthetic fixture. Never use a real credential here.
    fn sample() -> serde_json::Value {
        json!({
            "Mail": {
                "Name": "Epub Mail Sender",
                "Address": "sender@example.com",
                "Password": "abcdefghijklmnop",
                "Destinations": [
                    {"Name": "Test Reader", "Email": "reader@example.com", "Sources": {"test-source": {}}}
                ]
            },
            "EpubGen": {"Volumes": true, "Chapters": true, "StripColour": false},
            "Sources": [{
                "Id": "test-source",
                "Name": "Test Serial",
                "Enabled": true,
                "TocUrl": "https://example.com/toc/",
                "Selectors": {
                    "VolumeWrapper": "volume-wrapper",
                    "SelectorType": "class",
                    "IgnoredVolumes": ["Volume 0"]
                },
                "Auth": {"Type": "None"},
                "Metadata": {"Author": "A. Writer", "Description": "Test"},
                "PostProcessors": ["strip-links"]
            }]
        })
    }

    fn write_sample(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("config.json");
        std::fs::write(&path, serde_json::to_string_pretty(&sample()).unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }

    #[test]
    fn read_redacted_never_exposes_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample(dir.path());

        let redacted = read_redacted(&path).unwrap();
        let serialised = serde_json::to_string(&redacted).unwrap();

        assert!(redacted["Mail"].get("Password").is_none(), "password key must be absent");
        assert!(
            !serialised.contains("abcdefghijklmnop"),
            "password value must not appear anywhere in the response"
        );
        assert_eq!(redacted["Mail"]["PasswordSet"], json!(true));
    }

    #[test]
    fn blank_password_preserves_the_existing_one() {
        let current = sample();
        let mut candidate = sample();
        candidate["Mail"]["Password"] = json!("");

        merge_password(&mut candidate, &current);

        assert_eq!(candidate["Mail"]["Password"], json!("abcdefghijklmnop"));
    }

    #[test]
    fn absent_password_preserves_the_existing_one() {
        let current = sample();
        let mut candidate = sample();
        candidate["Mail"].as_object_mut().unwrap().remove("Password");

        merge_password(&mut candidate, &current);

        assert_eq!(candidate["Mail"]["Password"], json!("abcdefghijklmnop"));
    }

    #[test]
    fn supplied_password_replaces_the_existing_one() {
        let current = sample();
        let mut candidate = sample();
        candidate["Mail"]["Password"] = json!("qrstuvwxyz012345");

        merge_password(&mut candidate, &current);

        assert_eq!(candidate["Mail"]["Password"], json!("qrstuvwxyz012345"));
    }

    #[cfg(unix)]
    #[test]
    fn write_preserves_file_mode_600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = write_sample(dir.path());

        let mut updated = sample();
        updated["EpubGen"]["StripColour"] = serde_json::json!(true);
        write_atomic(&path, &updated).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode must survive the write, got {mode:o}");
    }

    #[test]
    fn write_preserves_keys_the_typed_structs_do_not_model() {
        // Sources[].Selectors.IgnoredVolumes is nested a level too deep for the
        // Selectors struct, so serde silently drops it on load. A typed
        // round-trip would delete it from disk; a Value round-trip must not.
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample(dir.path());

        let current: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        write_atomic(&path, &current).unwrap();

        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            after["Sources"][0]["Selectors"]["IgnoredVolumes"],
            serde_json::json!(["Volume 0"])
        );
    }

    #[test]
    fn write_leaves_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_sample(dir.path());

        let mut updated = sample();
        updated["EpubGen"]["Volumes"] = serde_json::json!(false);
        write_atomic(&path, &updated).unwrap();

        let backup = dir.path().join("config.json.bak");
        assert!(backup.exists(), "previous config must be kept");
        let restored: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&backup).unwrap()).unwrap();
        assert_eq!(restored["EpubGen"]["Volumes"], serde_json::json!(true));
    }

    #[test]
    fn validate_rejects_config_the_scraper_could_not_load() {
        let mut broken = sample();
        broken["Sources"] = serde_json::json!("not an array");
        assert!(validate(&broken).is_err());
    }

    #[test]
    fn validate_accepts_the_real_shape() {
        assert!(validate(&sample()).is_ok());
    }

    #[test]
    fn changed_keys_reports_names_never_values() {
        let before = sample();
        let mut after = sample();
        after["Mail"]["Password"] = serde_json::json!("qrstuvwxyz012345");
        after["EpubGen"]["StripColour"] = serde_json::json!(true);

        let keys = changed_keys(&before, &after);

        assert!(keys.contains(&"Mail".to_string()));
        assert!(keys.contains(&"EpubGen".to_string()));
        for key in &keys {
            assert!(!key.contains("qrstuvwxyz"), "audit output must not carry values");
        }
    }
}
