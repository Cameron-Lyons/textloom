//! Private temporary directories that clean up after themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs, io, process};

/// A private directory that is removed with everything in it when dropped.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    /// Creates a directory whose name starts with `prefix` in the system's
    /// temporary directory.
    pub fn try_new(prefix: &str) -> io::Result<Self> {
        Self::try_new_in(&env::temp_dir(), prefix)
    }

    /// Creates a directory whose name starts with `prefix` in `parent`.
    pub fn try_new_in(parent: &Path, prefix: &str) -> io::Result<Self> {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        loop {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.subsec_nanos());
            let count = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!("{prefix}{}-{nanos:08x}-{count}", process::id()));
            match create_private_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::fs::DirBuilder;
    use std::os::unix::fs::DirBuilderExt;
    DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_directories_are_unique_and_removed() {
        let first = TempDir::try_new("textloom-qa-test-").unwrap();
        let second = TempDir::try_new("textloom-qa-test-").unwrap();
        assert_ne!(first.path(), second.path());
        let path = first.path().to_path_buf();
        fs::write(path.join("file"), "x").unwrap();
        drop(first);
        assert!(!path.exists());
    }
}
