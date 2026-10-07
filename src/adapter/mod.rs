//! GUI adapters keep platform events and rendering outside the editing core.
//!
//! Custom adapters can render [`crate::Document::paragraphs`], hit-test to a
//! [`crate::Position`], and route commands through [`crate::Editor`].
#[cfg(feature = "egui")]
pub mod egui;
#[cfg(feature = "winit")]
pub mod winit;
