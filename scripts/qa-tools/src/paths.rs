//! Locations the tools work relative to.

use std::path::{Path, PathBuf};

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
