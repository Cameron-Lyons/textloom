//! The example's native rich clipboard transport.
//!
//! A rich copy publishes three representations of the selection: plain text,
//! HTML, and a TLFR fragment framed in a 32-byte envelope. Besides the
//! fragment's length, the envelope carries a publication timestamp and
//! sequence number, so that read-back verification can tell repeated copies of
//! the same text apart.
//!
//! Platform clipboard access runs in helper processes: the host runs its own
//! executable again with `--native-clipboard-copy` or
//! `--native-clipboard-read` (see [`helper`]) and exchanges a [`NativeItem`]
//! with it over pipes under a hard deadline. The platform modules implement
//! the helpers' `publish` and `read`. On Linux, the copy helper keeps running
//! to serve the clipboard until another application takes it over or the host
//! replaces it.
//!
//! Pastes are captured as they arrive in raw input (see
//! [`NativeClipboard::capture_pastes`]), so a paste that focus routing delivers
//! later still inserts the formatting that was on the clipboard when it
//! arrived.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
mod native_clipboard;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
use unsupported as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(any(target_os = "linux", target_os = "windows"))]
use native_clipboard::MIME;
use native_clipboard::{MAX_NATIVE_BYTES, NativeItem};
pub use native_clipboard::{NativeClipboard, SUPPORTED, helper};
