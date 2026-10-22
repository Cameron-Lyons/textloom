//! Textloom's QA tooling: the native report validator, the hosted macOS
//! interaction probe, and the local Windows VM with its checks. None of it is
//! part of the published crate.

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{env, fs, io, process};

pub mod cli;
pub mod native_report;

#[cfg(unix)]
pub mod macos_interaction;
#[cfg(unix)]
pub mod winvm;
#[cfg(unix)]
pub mod winvm_checks;

/// A failure, described for the person reading the tool's output.
#[derive(Debug)]
pub struct Error(String);

/// A result whose error defaults to the tools' [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self(message.to_owned())
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

/// Adds [`context`](ResultExt::context) to every `Result` with a displayable
/// error.
pub trait ResultExt<T> {
    /// Turns the error into an [`Error`] that starts with `what`, naming what was
    /// being done when it occurred, followed by `: ` and the original error.
    fn context(self, what: impl Display) -> Result<T>;
}

impl<T, E: Display> ResultExt<T> for std::result::Result<T, E> {
    fn context(self, what: impl Display) -> Result<T> {
        self.map_err(|error| Error(format!("{what}: {error}")))
    }
}

/// The repository checkout these tools were built from.
///
/// # Panics
///
/// Panics if the crate's manifest directory, fixed at build time, is less than
/// two directories deep, which cannot happen while it is `scripts/qa-tools` in
/// a checkout.
#[must_use]
pub fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the QA tools live in scripts/qa-tools")
        .to_path_buf()
}

/// The last `count` characters of `text`.
#[must_use]
pub fn tail(text: &str, count: usize) -> &str {
    let skip = text.chars().count().saturating_sub(count);
    text.char_indices()
        .nth(skip)
        .map_or("", |(index, _)| &text[index..])
}

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
    fn tail_counts_characters() {
        assert_eq!(tail("café", 2), "fé");
        assert_eq!(tail("café", 10), "café");
        assert_eq!(tail("café", 0), "");
    }

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
