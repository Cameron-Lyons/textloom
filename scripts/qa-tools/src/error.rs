//! The tools' error type and a way to say what failed.

use std::fmt::{self, Display, Formatter};
use std::io;

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
