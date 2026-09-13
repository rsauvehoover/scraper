//! Admin credential, sessions, CSRF tokens and login rate limiting.
//!
//! The credential lives in its own file, never in `config.json`: the editor
//! writes `config.json`, so a credential stored there would be rewritable by
//! anyone who got as far as the editor — the check would guard its own key.

use std::collections::HashMap;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::Argon2;
use rand::Rng;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct CredentialFile {
    argon2: String,
}

/// Hash a password with argon2id and a fresh salt, returning a PHC string.
///
/// `Argon2::hash_password` generates its own random salt internally (the
/// `getrandom` feature, on by default) — there is no `SaltString` to build by
/// hand in this version of the `password-hash` crate.
pub fn hash_password(plain: &str) -> Result<String, String> {
    Argon2::default()
        .hash_password(plain.as_bytes())
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Constant-time verification. Any malformed hash is a failed login, not a panic.
pub fn verify_password(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Read the stored PHC string. An absent file is an error: the server must
/// refuse to start rather than serve without authentication.
pub fn load_credential(path: &Path) -> Result<String, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "cannot read credential file {}: {e}. Run `wandering_inn_scraper web --set-password` first.",
            path.display()
        )
    })?;
    let parsed: CredentialFile =
        serde_json::from_str(&raw).map_err(|e| format!("malformed credential file: {e}"))?;
    if parsed.argon2.is_empty() {
        return Err("credential file contains an empty hash".to_string());
    }
    Ok(parsed.argon2)
}

/// Write the credential file at mode 600, atomically.
///
/// Temp file beside the target, then rename — matching
/// `webconfig::write_atomic`'s shape, and for the same reason: a crash
/// between truncating and writing must never leave a half-written or empty
/// credential file in place. An empty file is worse here than in
/// `webconfig.rs`, because `load_credential` treats an empty hash as an
/// error — that would lock the operator out of their own admin UI with no
/// way back in except editing the file by hand.
///
/// The mode is forced on the TEMP file after writing rather than relied on
/// from `OpenOptions::mode`, because POSIX `open()` applies the mode argument
/// only when it actually creates the inode. The temp path is normally new, so
/// `.mode(0o600)` normally does the job — but a leftover temp file from a
/// crashed earlier run with the same pid is opened, not created, and keeps
/// whatever mode it already had. The unconditional `set_permissions` closes
/// that case.
///
/// It says nothing about the target's previous mode, and does not need to:
/// `rename` replaces the target's directory entry with the temp file's inode,
/// so the mode that survives is the temp file's. A loose mode on a
/// pre-existing credential file is discarded by the rename, not inherited.
pub fn store_credential(path: &Path, phc: &str) -> io::Result<()> {
    let body = serde_json::to_string_pretty(&CredentialFile {
        argon2: phc.to_string(),
    })
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // `Path::parent()` returns `Some("")` for a bare relative filename, not
    // `None` — see the identical note in `webconfig::write_atomic`.
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("web-auth.json"));
    let tmp = dir.join(format!(
        "{}.tmp.{}",
        file_name.to_string_lossy(),
        std::process::id()
    ));

    write_private(&tmp, body.as_bytes())?;
    force_private_mode(&tmp)?;
    std::fs::rename(&tmp, path)?;

    // Durability of the rename itself, as in `webconfig::write_atomic`.
    if let Ok(dir_handle) = std::fs::File::open(dir) {
        let _ = dir_handle.sync_all();
    }

    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    std::fs::write(path, bytes)
}

#[cfg(unix)]
fn force_private_mode(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

// This crate also ships a Windows MSI (`cargo wix`), so this arm is live
// code, not dead code. Windows has no unix mode bits, so permissions are
// left to OS defaults on non-unix targets, matching `webconfig.rs`.
#[cfg(not(unix))]
fn force_private_mode(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn random_token() -> String {
    // rand 0.10 dropped `OsRng`/`RngCore` in favour of `Rng`/`TryRng` and a
    // `SysRng` that only implements the fallible `TryRng`. `rand::rng()` is
    // the thread-local CSPRNG, seeded from the OS, and implements the
    // infallible `Rng` trait, which is what supplies `fill_bytes` here.
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    // `hex` is already a direct dependency of this crate. Hand-rolling a
    // base64 encoder for a security token is the kind of thing that looks
    // harmless and occasionally is not; hex is unambiguous, constant-length,
    // and URL-safe by construction.
    hex::encode(bytes)
}

struct Session {
    expires: Instant,
    csrf: String,
}

/// Server-side session store. In memory deliberately: a restart logs everyone
/// out, which is the correct behaviour for a single-admin tool.
pub struct SessionStore {
    sessions: Mutex<HashMap<String, Session>>,
    ttl: Duration,
}

impl SessionStore {
    pub fn new(ttl: Duration) -> Self {
        SessionStore {
            sessions: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn create(&self) -> String {
        let token = random_token();
        let mut guard = self.sessions.lock().expect("session store poisoned");
        guard.retain(|_, s| s.expires > Instant::now());
        guard.insert(
            token.clone(),
            Session {
                expires: Instant::now() + self.ttl,
                csrf: random_token(),
            },
        );
        token
    }

    pub fn validate(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        let mut guard = self.sessions.lock().expect("session store poisoned");
        match guard.get_mut(token) {
            Some(session) if session.expires > Instant::now() => {
                // Idle timeout: activity extends the session.
                session.expires = Instant::now() + self.ttl;
                true
            }
            Some(_) => {
                guard.remove(token);
                false
            }
            None => false,
        }
    }

    pub fn revoke(&self, token: &str) {
        self.sessions
            .lock()
            .expect("session store poisoned")
            .remove(token);
    }

    pub fn csrf_for(&self, token: &str) -> Option<String> {
        self.sessions
            .lock()
            .expect("session store poisoned")
            .get(token)
            .map(|s| s.csrf.clone())
    }
}

/// Fixed-window per-key limiter for the login endpoint.
///
/// Not doing account lockout: there is one account, so a lockout is a
/// self-denial-of-service that an attacker can trigger for free.
pub struct RateLimiter {
    attempts: Mutex<HashMap<String, (u32, Instant)>>,
    max: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        RateLimiter {
            attempts: Mutex::new(HashMap::new()),
            max,
            window,
        }
    }

    /// `true` if the attempt is allowed. Records it either way.
    ///
    /// Expired windows are dropped before the insert, as `SessionStore::create`
    /// does. Without that the map only ever grows, and with
    /// `--trust-forwarded-for` the key comes from a client-supplied header: an
    /// attacker rotating `X-Forwarded-For` would add an entry per request and
    /// never free one. The retain bounds the map by the number of distinct
    /// clients seen within one window instead.
    pub fn check(&self, key: &str) -> bool {
        let mut guard = self.attempts.lock().expect("rate limiter poisoned");
        let now = Instant::now();

        guard.retain(|_, (_, started)| now.duration_since(*started) <= self.window);

        // Every surviving entry is inside its window, so a new entry here is
        // either genuinely new or one whose window has just been dropped —
        // both start a fresh count.
        let entry = guard.entry(key.to_string()).or_insert((0, now));

        if entry.0 >= self.max {
            return false;
        }
        entry.0 += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hashes_are_salted_and_verify() {
        let a = hash_password("correct horse battery staple").unwrap();
        let b = hash_password("correct horse battery staple").unwrap();

        assert_ne!(a, b, "each hash must use a fresh salt");
        assert!(a.starts_with("$argon2id$"), "must be argon2id PHC, got {}", a);
        assert!(verify_password("correct horse battery staple", &a));
        assert!(verify_password("correct horse battery staple", &b));
        assert!(!verify_password("wrong password", &a));
    }

    #[test]
    fn verify_rejects_a_malformed_hash_without_panicking() {
        assert!(!verify_password("anything", "not-a-phc-string"));
        assert!(!verify_password("anything", ""));
    }

    #[cfg(unix)]
    #[test]
    fn stored_credential_is_mode_600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-auth.json");

        store_credential(&path, &hash_password("hunter2").unwrap()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credential file must not be readable by others");
    }

    #[cfg(unix)]
    #[test]
    fn stored_credential_is_tightened_when_the_file_already_exists() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-auth.json");

        // A pre-existing, world-readable file: a backup restore, a hand copy,
        // or an earlier tool. Rotation must tighten it, not inherit it.
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        store_credential(&path, &hash_password("hunter2hunter2").unwrap()).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rotation must tighten an existing file, got {mode:o}");
    }

    #[test]
    fn credential_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-auth.json");
        let phc = hash_password("hunter2").unwrap();

        store_credential(&path, &phc).unwrap();
        let loaded = load_credential(&path).unwrap();

        assert_eq!(loaded, phc);
        assert!(verify_password("hunter2", &loaded));
    }

    #[test]
    fn missing_credential_file_is_an_error_not_a_default() {
        // The server must refuse to start rather than fall open.
        let dir = tempfile::tempdir().unwrap();
        let result = load_credential(&dir.path().join("absent.json"));
        assert!(result.is_err());
    }

    #[test]
    fn sessions_are_unguessable_and_revocable() {
        let store = SessionStore::new(Duration::from_secs(3600));

        let token = store.create();
        assert_eq!(token.len(), 64, "32 random bytes hex-encoded is 64 chars");
        assert!(store.validate(&token));

        let other = store.create();
        assert_ne!(token, other);

        store.revoke(&token);
        assert!(!store.validate(&token));
        assert!(store.validate(&other), "revoking one session must not affect another");
    }

    #[test]
    fn expired_sessions_are_rejected() {
        let store = SessionStore::new(Duration::from_millis(0));
        let token = store.create();
        std::thread::sleep(Duration::from_millis(5));
        assert!(!store.validate(&token));
    }

    #[test]
    fn unknown_token_is_rejected() {
        let store = SessionStore::new(Duration::from_secs(3600));
        assert!(!store.validate("not-a-real-token"));
        assert!(!store.validate(""));
    }

    #[test]
    fn csrf_token_is_stable_per_session_and_differs_across_sessions() {
        let store = SessionStore::new(Duration::from_secs(3600));
        let a = store.create();
        let b = store.create();

        let csrf_a = store.csrf_for(&a).unwrap();
        assert_eq!(store.csrf_for(&a).unwrap(), csrf_a);
        assert_ne!(store.csrf_for(&b).unwrap(), csrf_a);
        assert_ne!(csrf_a, a, "csrf token must not be the session token");
    }

    #[test]
    fn rate_limiter_blocks_after_the_configured_attempts() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));

        assert!(limiter.check("192.0.2.1"));
        assert!(limiter.check("192.0.2.1"));
        assert!(limiter.check("192.0.2.1"));
        assert!(!limiter.check("192.0.2.1"), "fourth attempt must be refused");

        assert!(limiter.check("192.0.2.2"), "other clients are unaffected");
    }

    #[test]
    fn rate_limiter_window_expires() {
        let limiter = RateLimiter::new(1, Duration::from_millis(10));
        assert!(limiter.check("192.0.2.3"));
        assert!(!limiter.check("192.0.2.3"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(limiter.check("192.0.2.3"), "window must reset");
    }
}
