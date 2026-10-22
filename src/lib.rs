//! A native rich-text editing core for Rust, with optional egui, winit, and
//! AccessKit adapters.
//!
//! The core needs no window, renderer, or async runtime. A [`Document`] is a
//! nonempty list of immutable, shared [`Paragraph`]s. Each paragraph has text,
//! formatting [`Span`]s that carry an [`InlineStyle`] and an optional
//! [`Link`], and a [`ParagraphKind`]: body text, a heading, a list item, or a
//! quote. A [`Position`] is a paragraph index and a UTF-8 byte offset on an
//! extended grapheme cluster boundary, and a [`Selection`] runs from an anchor
//! to a focus position. An [`Editor`] owns a document and applies editing
//! commands to its selection, with undo history, transactions, and IME
//! composition. A [`Fragment`] carries rich text through the clipboard and
//! converts to Textloom's lossless binary format (TLFR), HTML, and plain text.
//!
//! # Features
//!
//! - `egui`: `adapter::egui::RichTextEditor`, an egui widget that draws an
//!   editor and routes pointer, keyboard, IME, and accessibility input to it.
//! - `winit`: `adapter::winit::WinitAdapter`, which maps winit keyboard and
//!   IME events to editor commands for hosts that lay out and paint the text
//!   themselves.
//! - `accesskit`: `accessibility::AccessKitAdapter`, which publishes an
//!   AccessKit text tree and applies accessibility actions.
//!
//! The [`adapter`] and [`accessibility`] modules document each integration;
//! the renderer-independent parts of [`accessibility`] need no feature.
//!
//! # Example
//!
//! ```
//! use textloom::{Editor, ParagraphKind, Position, Selection, StylePatch};
//!
//! let mut editor = Editor::from_text("Hello, native Rust!");
//! editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5)))?;
//! editor.apply_style(StylePatch { bold: Some(true), ..StylePatch::default() })?;
//! assert_eq!(editor.selection_style().bold, Some(true));
//!
//! editor.set_paragraph_kind(ParagraphKind::Heading { level: 1 })?;
//! assert_eq!(
//!     editor.document().to_html(),
//!     r#"<div style="white-space:pre-wrap"><h1><strong>Hello</strong>, native Rust!</h1></div>"#,
//! );
//! assert!(editor.undo());
//! assert_eq!(editor.document().paragraphs()[0].kind(), ParagraphKind::Body);
//! # Ok::<(), textloom::Error>(())
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]

pub mod accessibility;
pub mod adapter;
mod document;
mod editor;
mod export;
mod fragment;
mod search;
mod types;

pub use document::{Document, Paragraph, Span};
pub use editor::{Composition, Editor, HistoryLimits};
pub use fragment::{DecodeLimits, Fragment, FragmentError};
pub use search::SearchOptions;
pub use types::{
    Color, Error, InlineAttribute, InlineStyle, Link, Movement, ParagraphKind, Position, Selection,
    SelectionStyle, StylePatch,
};

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Editor>();
    assert_send_sync::<Document>();
    assert_send_sync::<Paragraph>();
    assert_send_sync::<Fragment>();
    assert_send_sync::<Composition>();
    assert_send_sync::<Link>();
    assert_send_sync::<Error>();
    assert_send_sync::<FragmentError>();
};
