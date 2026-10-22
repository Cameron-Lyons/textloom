//! Textloom's QA tooling: the native report validator, the hosted macOS
//! interaction probe, and the local Windows VM with its checks. None of it is
//! part of the published crate.

pub mod cli;
mod error;
#[cfg(unix)]
pub mod macos_interaction;
pub mod native_report;
mod paths;
mod temp_dir;
mod text;
#[cfg(unix)]
pub mod winvm;
#[cfg(unix)]
pub mod winvm_checks;

pub use error::{Error, Result, ResultExt};
pub use paths::repo_root;
pub use temp_dir::TempDir;
pub use text::tail;
