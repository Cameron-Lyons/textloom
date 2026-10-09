#![doc = include_str!("../README.md")]

/// Borrowed editor semantics and optional AccessKit integration.
pub mod accessibility;
/// Optional egui rendering and winit native input integrations.
pub mod adapter;
mod document;
mod editor;
mod export;
mod fragment;
mod search;
mod types;

pub use document::{Document, Paragraph, Span};
pub use editor::{Composition, Editor, HistoryLimits};
pub use fragment::{Fragment, FragmentError};
pub use search::SearchOptions;
pub use types::{
    Color, Error, InlineStyle, Movement, ParagraphKind, Position, Selection, SelectionStyle,
    StylePatch,
};
