//! Helpers shared by unit tests.

use std::path::{Path, PathBuf};

/// Points the process cwd at a scratch directory, restoring it on drop,
/// including on unwind from a panic.
///
/// `SourceDatabase::open` and `open_query_only` resolve `db/` relative to the
/// cwd, so a test that exercises either must redirect it or it would touch
/// the repository's own `db/`. The cwd is process-global, so every test that
/// uses this must also be `#[serial]`.
pub struct CwdGuard {
    original: PathBuf,
}

impl CwdGuard {
    pub fn change_to(dir: &Path) -> Self {
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
