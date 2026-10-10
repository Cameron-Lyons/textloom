//! Paragraph replacement commits, bounded change notifications, and undo deltas.

use std::{ops::Range, sync::Arc};

use super::{Document, Paragraph};

#[cfg(any(feature = "egui", feature = "accesskit"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParagraphChange {
    /// Replaced paragraph range in the preceding document state.
    pub range: Range<usize>,
    /// Number of paragraphs inserted at `range.start` in the current state.
    pub new_len: usize,
}

#[cfg(any(feature = "egui", feature = "accesskit"))]
#[derive(Clone, Debug)]
pub(super) struct LastChange {
    previous_identity: Arc<()>,
    paragraphs: ParagraphChange,
}

impl Document {
    pub(crate) fn replay(&mut self, delta: &Delta, forward: bool) {
        if delta.is_empty() {
            return;
        }
        let (remove, insert) = if forward {
            (&delta.before, &delta.after)
        } else {
            (&delta.after, &delta.before)
        };
        debug_assert_eq!(
            self.paragraphs.get(delta.start..delta.start + remove.len()),
            Some(remove.as_slice()),
            "history must be replayed against its corresponding document state"
        );
        self.paragraphs.splice(
            delta.start..delta.start + remove.len(),
            insert.iter().cloned(),
        );
        self.record_change(delta.start, remove.len(), insert.len());
    }

    pub(super) fn change(&mut self, range: Range<usize>, mut after: Vec<Arc<Paragraph>>) -> Delta {
        let mut start = range.start;
        let mut end = range.end;
        // Preserve existing allocations at unchanged edges, and keep them out of
        // the undo delta even when a caller supplied a wide range.
        let mut prefix = 0;
        while start < end
            && prefix < after.len()
            && self.paragraphs[start].as_ref() == after[prefix].as_ref()
        {
            start += 1;
            prefix += 1;
        }
        after.drain(..prefix);
        while start < end
            && after
                .last()
                .is_some_and(|paragraph| self.paragraphs[end - 1].as_ref() == paragraph.as_ref())
        {
            end -= 1;
            after.pop();
        }
        let before = self.paragraphs[start..end].to_vec();
        let delta = Delta {
            start,
            before,
            after,
        };
        if !delta.is_empty() {
            self.paragraphs
                .splice(start..end, delta.after.iter().cloned());
            self.record_change(start, end - start, delta.after.len());
        }
        delta
    }

    fn record_change(&mut self, start: usize, old_len: usize, new_len: usize) {
        self.revision = self.revision.saturating_add(1);
        #[cfg(any(feature = "egui", feature = "accesskit"))]
        {
            let previous_identity = std::mem::replace(&mut self.identity, Arc::new(()));
            self.last_change = Some(LastChange {
                previous_identity,
                paragraphs: ParagraphChange {
                    range: start..start + old_len,
                    new_len,
                },
            });
        }
        #[cfg(not(any(feature = "egui", feature = "accesskit")))]
        {
            let _ = (start, old_len, new_len);
            self.identity = Arc::new(());
        }
    }

    /// Return the latest paragraph replacement only when it follows `identity`.
    /// Matching the current identity yields an empty replacement; missing an
    /// intervening edit requires adapters to rebuild or compare their snapshots.
    #[cfg(any(feature = "egui", feature = "accesskit"))]
    pub(crate) fn change_since(&self, identity: &Arc<()>) -> Option<ParagraphChange> {
        if Arc::ptr_eq(identity, &self.identity) {
            return Some(ParagraphChange {
                range: 0..0,
                new_len: 0,
            });
        }
        self.last_change.as_ref().and_then(|change| {
            Arc::ptr_eq(identity, &change.previous_identity).then(|| change.paragraphs.clone())
        })
    }
}

/// History stores only the contiguous paragraphs changed by an operation.
#[derive(Clone, Debug)]
pub(crate) struct Delta {
    pub start: usize,
    pub before: Vec<Arc<Paragraph>>,
    pub after: Vec<Arc<Paragraph>>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty()
    }

    /// A conservative estimate; shared text may be counted more than once.
    pub fn retained_bytes(&self) -> usize {
        self.before
            .iter()
            .chain(&self.after)
            .map(|paragraph| {
                std::mem::size_of::<Paragraph>()
                    .saturating_add(paragraph.text.len())
                    .saturating_add(std::mem::size_of_val(paragraph.spans()))
            })
            .fold(0, usize::saturating_add)
    }
}

#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
mod tests {
    use super::*;
    use crate::{InlineStyle, ParagraphKind, Position, StylePatch};

    fn assert_change(document: &Document, identity: &Arc<()>, range: Range<usize>, new_len: usize) {
        assert_eq!(
            document.change_since(identity),
            Some(ParagraphChange { range, new_len })
        );
    }

    #[test]
    fn notifications_keep_the_latest_change_through_noops_and_rejected_edits() {
        let mut document = Document::from_text("zero\none\ntwo");
        let original = document.content_identity();
        assert_change(&document, &original, 0..0, 0);
        let (delta, _) = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "ONE",
                InlineStyle::default(),
            )
            .unwrap();
        assert_change(&document, &original, 1..2, 1);
        let changed = document.content_identity();
        let (empty, _) = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "ONE",
                InlineStyle::default(),
            )
            .unwrap();
        assert!(empty.is_empty());
        document.replay(&empty, true);
        assert!(
            document
                .replace(
                    Position::new(8, 0)..Position::new(8, 0),
                    "invalid",
                    InlineStyle::default(),
                )
                .is_err()
        );
        assert_change(&document, &original, 1..2, 1);
        assert_change(&document, &changed, 0..0, 0);
        document.replay(&delta, false);
        assert_change(&document, &changed, 1..2, 1);
    }

    #[test]
    fn structural_edits_and_history_notify_in_their_own_paragraph_coordinates() {
        let mut document = Document::from_text("zero\none\ntwo");
        let original = document.content_identity();
        let (delta, _) = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "first\nmiddle\nlast",
                InlineStyle::default(),
            )
            .unwrap();
        assert_change(&document, &original, 1..2, 3);
        let inserted = document.content_identity();
        document.replay(&delta, false);
        assert_change(&document, &inserted, 1..4, 1);
        let undone = document.content_identity();
        document.replay(&delta, true);
        assert_change(&document, &undone, 1..2, 3);
        assert!(document.change_since(&original).is_none());
        assert_eq!(document.plain_text(), "zero\nfirst\nmiddle\nlast\ntwo");
    }

    #[test]
    fn formatting_and_list_changes_trim_unchanged_paragraph_edges() {
        let mut document = Document::from_text("\none\n");
        let original = document.content_identity();
        document
            .apply_style(
                Position::default()..document.end(),
                StylePatch {
                    bold: Some(true),
                    ..StylePatch::default()
                },
            )
            .unwrap();
        assert_change(&document, &original, 1..2, 1);
        let formatted = document.content_identity();
        document
            .set_kind(1..2, ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        assert_change(&document, &formatted, 1..2, 1);
        let listed = document.content_identity();
        document.change_list_indent(0..3, true);
        assert_change(&document, &listed, 1..2, 1);
        let indented = document.content_identity();
        assert!(document.change_list_indent(0..1, true).is_empty());
        assert_change(&document, &listed, 1..2, 1);
        assert_change(&document, &indented, 0..0, 0);
    }

    #[test]
    fn clone_branches_and_saturated_revisions_use_content_identity() {
        let mut document = Document::from_text("one\ntwo");
        document.revision = u64::MAX;
        let original = document.content_identity();
        let mut branch = document.clone();
        document
            .replace(
                Position::new(0, 0)..Position::new(0, 3),
                "ONE",
                InlineStyle::default(),
            )
            .unwrap();
        branch
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "TWO",
                InlineStyle::default(),
            )
            .unwrap();
        assert_eq!(document.revision(), u64::MAX);
        assert_eq!(branch.revision(), u64::MAX);
        assert_change(&document, &original, 0..1, 1);
        assert_change(&branch, &original, 1..2, 1);
        assert!(document.change_since(&branch.content_identity()).is_none());
        assert!(branch.change_since(&document.content_identity()).is_none());
        let mut equal = Document::from_text(&document.plain_text());
        equal.revision = u64::MAX;
        assert_eq!(document, equal);
        assert_eq!(document, document.clone());
    }

    #[test]
    fn notifications_retain_only_one_predecessor_identity_and_no_paragraphs() {
        let mut document = Document::from_text("one");
        let original = Arc::downgrade(&document.content_identity());
        let paragraph = Arc::clone(&document.paragraphs()[0]);
        document
            .replace(document.end()..document.end(), "!", InlineStyle::default())
            .unwrap();
        assert!(original.upgrade().is_some());
        assert_eq!(Arc::strong_count(&paragraph), 1);
        document
            .replace(document.end()..document.end(), "!", InlineStyle::default())
            .unwrap();
        assert!(original.upgrade().is_none());
    }
}
