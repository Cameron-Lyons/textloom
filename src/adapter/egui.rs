//! An egui widget for editing a rich-text [`Editor`](crate::Editor).
//!
//! [`RichTextEditor`] paints the document and applies egui's pointer, keyboard,
//! clipboard, IME, and AccessKit input to the editor. It reports rejected
//! commands, caret geometry, and activated links in a [`RichTextEditorOutput`]
//! and publishes an AccessKit tree of paragraphs, text runs, links, and block
//! quotes. Implement [`RichClipboard`] to exchange rich fragments with the
//! system clipboard, and call [`install_input_routing`] when creating the egui
//! context.
//!
//! The widget needs the `egui` feature, which does not enable egui's
//! `default_fonts`: hosts that rely on egui's bundled fonts and do not use
//! eframe, which enables them, must enable `egui/default_fonts` themselves.

mod accessibility;
mod composition;
mod input;
mod input_routing;
mod layout;
mod navigation;
mod paint;
mod rich_text_editor;

pub use accessibility::AccessibilityError;
pub use input_routing::install_input_routing;
#[cfg(test)]
use rich_text_editor::PADDING;
use rich_text_editor::{Cache, CommitEcho, PARAGRAPH_GAP, record};
pub use rich_text_editor::{RichClipboard, RichTextEditor, RichTextEditorOutput};
