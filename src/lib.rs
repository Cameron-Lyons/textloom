//! Native rich-text editing, independent of any window system or renderer.
//!
//! Positions use paragraph indices and UTF-8 byte offsets at grapheme boundaries.
//! GUI integrations are opt-in; the default build depends only on Unicode segmentation.
//!
//! ```
//! use textloom::{Editor, Position, Selection, StylePatch};
//! let mut editor = Editor::from_text("Hello Rust");
//! editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5)))?;
//! editor.apply_style(StylePatch { bold: Some(true), ..Default::default() })?;
//! assert!(editor.document().paragraph(0).unwrap().spans()[0].style.bold);
//! assert!(editor.undo());
//! # Ok::<(), textloom::Error>(())
//! ```
pub mod accessibility;
pub mod adapter;
mod document;
mod editor;
mod types;

pub use document::{Document, Paragraph, Span};
pub use editor::{Composition, Editor, HistoryLimits};
pub use types::{
    Color, Error, InlineStyle, Movement, ParagraphKind, Position, Selection, StylePatch,
};
