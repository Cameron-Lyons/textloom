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
pub use fragment::{Fragment, FragmentError};
pub use search::SearchOptions;
pub use types::{
    Color, Error, InlineStyle, Movement, ParagraphKind, Position, Selection, SelectionStyle,
    StylePatch,
};
