//! Integrations that connect the editing core to GUI toolkits.
//!
//! Each adapter is behind the Cargo feature of the same name and translates one
//! toolkit's input into commands for an [editor](crate::Editor):
//!
//! - `egui` provides a rich-text editor widget for egui that lays out, paints, and
//!   hit-tests the document and publishes it to egui's accessibility tree.
//! - `winit` routes winit keyboard, modifier, focus, and IME events to an editor
//!   whose host does its own layout, painting, pointer handling, and clipboard
//!   access.
//!
//! Both adapters share the rules that map key chords to editing and formatting
//! shortcuts and that filter control characters out of typed and pasted text.
//!
//! A host for another toolkit can build its own adapter on the public core API
//! instead: paint [`Document::paragraphs`](crate::Document::paragraphs), map
//! pointer positions to a [`Position`](crate::Position), route commands through
//! the editor, and describe it to assistive technology with
//! [`accessibility`](crate::accessibility).

#[cfg(feature = "egui")]
pub mod egui;
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(any(feature = "egui", feature = "winit"))]
mod control_characters;
#[cfg(any(feature = "egui", feature = "winit"))]
mod shortcut;

#[cfg(any(feature = "egui", feature = "winit"))]
use control_characters::{ControlCharacters, clean_text};
#[cfg(any(feature = "egui", feature = "winit"))]
use shortcut::{Shortcut, shortcut_key_name};
