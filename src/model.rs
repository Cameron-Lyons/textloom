//! The rich-text document model: documents, paragraphs and their spans,
//! ordered list numbering, and the deltas that undo history records.

mod change;
mod document;
mod numbering;
mod paragraph;
mod words;

#[cfg(feature = "egui")]
pub(crate) use change::ParagraphChange;
pub(crate) use change::{Delta, Replay};
#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
pub(crate) use change::{SparseCoverage, take_sparse_coverage};
pub use document::Document;
pub(crate) use document::{Indent, LINE_SEPARATOR, Replacement, normalize_newlines};
#[cfg(feature = "egui")]
pub(crate) use numbering::NumberedItem;
pub(crate) use numbering::{Levels, same_effect as same_numbering_effect};
#[cfg(any(feature = "accesskit", feature = "egui"))]
pub(crate) use paragraph::SpanCursor;
pub use paragraph::{Paragraph, Span};
