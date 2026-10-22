//! The editing session: an [`Editor`] over a document, with its IME
//! composition and undo history.

mod composition;
mod editor;
mod history;

pub use composition::Composition;
pub use editor::Editor;
pub use history::HistoryLimits;
