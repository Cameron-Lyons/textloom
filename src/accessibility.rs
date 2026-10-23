//! Accessibility semantics for an [`Editor`](crate::Editor) that do not depend on a GUI toolkit,
//! and an optional AccessKit tree.
//!
//! A host that exposes the editor to assistive technology, such as a screen
//! reader, uses this module in one of two ways:
//!
//! - [`AccessibilitySnapshot`] borrows an editor and describes it without any
//!   accessibility dependency: the accessible name and focus that the host
//!   supplies, each paragraph's text, kind, and inline styles, the directional
//!   selection in grapheme positions, and any IME composition. The host maps this
//!   onto its platform's accessibility API and passes requests from assistive
//!   technology back to [`apply_action`] as [`AccessibleAction`]s, which go
//!   through the editor's validation and undo history.
//! - With the `accesskit` feature, `AccessKitAdapter` publishes the editor as an
//!   AccessKit tree, updates that tree incrementally after edits, and carries out
//!   AccessKit action requests. Its failures are `AccessKitError`s.
//!
//! Neither one owns the window, the event loop, or text layout. The host adds
//! screen geometry to what it publishes, focuses the editor when an
//! [`AccessibilityOutcome`] requests it, and redraws when the outcome reports a
//! change. The egui widget publishes its own accessibility tree through egui.
//!
//! # Example
//!
//! ```
//! use textloom::accessibility::{AccessibilitySnapshot, AccessibleAction, Focus, apply_action};
//! use textloom::{Editor, ParagraphKind};
//!
//! let mut editor = Editor::from_text("Title\nBody");
//! editor.set_paragraph_kind(ParagraphKind::Heading { level: 1 })?;
//!
//! let snapshot = AccessibilitySnapshot::new(&editor, "Notes", Focus::Focused);
//! let kinds: Vec<ParagraphKind> = snapshot.paragraphs().map(|paragraph| paragraph.kind).collect();
//! assert_eq!(kinds, [ParagraphKind::Heading { level: 1 }, ParagraphKind::Body]);
//!
//! // A screen reader types at the caret, which is at the start of the document.
//! let outcome = apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("My "))?;
//! assert!(outcome.handled && outcome.changed);
//! assert_eq!(editor.document().plain_text(), "My Title\nBody");
//! # Ok::<(), textloom::Error>(())
//! ```

mod accessibility_snapshot;
mod accessible_action;
#[cfg(feature = "accesskit")]
mod accesskit_adapter;
#[cfg(any(feature = "accesskit", feature = "egui"))]
pub(crate) mod node_semantics;
#[cfg(test)]
#[cfg(any(feature = "accesskit", feature = "egui"))]
pub(crate) mod random_edits;

pub use accessibility_snapshot::{
    AccessibilitySnapshot, AccessibleParagraph, AccessiblePosition, AccessibleSelection, Focus,
};
pub use accessible_action::{AccessibilityOutcome, AccessibleAction, apply_action};
#[cfg(feature = "accesskit")]
pub use accesskit_adapter::{AccessKitAdapter, AccessKitError};
