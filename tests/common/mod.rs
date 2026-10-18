//! Scratch directories for integration tests, implemented without a dependency.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

/// Temporary root; the project targets Linux runners and containers.
const TEMP_ROOT: &str = "/tmp";

/// Distinguishes directories created by the same test process.
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

/// Create a unique temporary directory that is removed when the value is dropped.
pub fn temp_dir() -> TempDir {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        Path::new(TEMP_ROOT).join(format!("backup-repos-checker-{}-{id}", std::process::id()));

    std::fs::create_dir_all(&path).expect("cannot create temporary directory");

    TempDir(path)
}

/// Owned scratch directory deleted on drop so runs leave nothing behind.
pub struct TempDir(PathBuf);

impl TempDir {
    /// Root of the files a single test may create.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    /// Remove the tree; ignore errors so cleanup cannot mask test results.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
