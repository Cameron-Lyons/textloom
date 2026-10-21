use std::{
    cell::OnceCell,
    collections::HashSet,
    ops::Range,
    sync::{Arc, OnceLock},
};

use super::{Document, Paragraph};

#[cfg(any(feature = "egui", feature = "accesskit"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ParagraphChange {
    pub range: Range<usize>,
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
        let (old_len, new_len) = delta.extent();
        if forward {
            for splice in delta.splices() {
                self.replay_splice(splice, true);
            }
            self.record_change(delta.start, old_len, new_len);
        } else {
            for splice in delta.splices().rev() {
                self.replay_splice(splice, false);
            }
            self.record_change(delta.start, new_len, old_len);
        }
    }

    fn replay_splice(&mut self, splice: &Delta, forward: bool) {
        let (remove, insert) = if forward {
            (&splice.before, &splice.after)
        } else {
            (&splice.after, &splice.before)
        };
        debug_assert_eq!(
            self.paragraphs
                .get(splice.start..splice.start + remove.len()),
            Some(remove.as_slice()),
            "history must be replayed against its corresponding document state"
        );
        self.paragraphs.splice(
            splice.start..splice.start + remove.len(),
            insert.iter().cloned(),
        );
        self.retain_numbers(remove, insert);
    }

    pub(super) fn change(&mut self, range: Range<usize>, mut after: Vec<Arc<Paragraph>>) -> Delta {
        let mut start = range.start;
        let mut end = range.end;
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
        let new_len = after.len();
        let runs = if end - start == new_len {
            self.changed_runs(start, &after)
        } else {
            Vec::new()
        };
        let delta = if runs.len() > 1 {
            let mut splices = runs.into_iter().map(|run| Delta {
                start: start + run.start,
                before: self.paragraphs[start + run.start..start + run.end].to_vec(),
                after: after[run].to_vec(),
                rest: Vec::new(),
            });
            let mut delta = splices.next().expect("several runs");
            delta.rest = splices.collect();
            delta
        } else {
            Delta {
                start,
                before: self.paragraphs[start..end].to_vec(),
                after,
                rest: Vec::new(),
            }
        };
        if !delta.is_empty() {
            for splice in delta.splices() {
                self.replay_splice(splice, true);
            }
            self.record_change(start, end - start, new_len);
        }
        delta
    }

    fn changed_runs(&self, start: usize, after: &[Arc<Paragraph>]) -> Vec<Range<usize>> {
        let mut runs: Vec<Range<usize>> = Vec::new();
        if after.len() <= SHARED_GAP + 2 {
            return runs;
        }
        for (index, (old, new)) in self.paragraphs[start..].iter().zip(after).enumerate() {
            if Arc::ptr_eq(old, new) {
                continue;
            }
            match runs.last_mut() {
                Some(run) if index - run.end <= SHARED_GAP => run.end = index + 1,
                _ => runs.push(index..index + 1),
            }
        }
        runs
    }

    fn retain_numbers(&mut self, before: &[Arc<Paragraph>], after: &[Arc<Paragraph>]) {
        if before.len() != after.len()
            || before
                .iter()
                .zip(after)
                .any(|(before, after)| !super::same_numbering_effect(before.kind, after.kind))
        {
            self.numbers = OnceLock::new();
        }
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

const SHARED_GAP: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct Delta {
    pub start: usize,
    pub before: Vec<Arc<Paragraph>>,
    pub after: Vec<Arc<Paragraph>>,
    // Later disjoint splices in document order, each positioned after its predecessors apply.
    pub rest: Vec<Delta>,
}

impl Delta {
    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty() && self.rest.is_empty()
    }

    pub fn retained_bytes(&self) -> usize {
        self.splices()
            .map(|splice| {
                side_bytes(&splice.before, &splice.after)
                    .max(side_bytes(&splice.after, &splice.before))
            })
            .fold(0, usize::saturating_add)
    }

    fn splices(&self) -> impl DoubleEndedIterator<Item = &Delta> {
        std::iter::once(self).chain(&self.rest)
    }

    fn extent(&self) -> (usize, usize) {
        let last = self.rest.last().unwrap_or(self);
        let end = last.start + last.after.len();
        let (removed, inserted) = self.splices().fold((0, 0), |(removed, inserted), splice| {
            (removed + splice.before.len(), inserted + splice.after.len())
        });
        (end + removed - inserted - self.start, end - self.start)
    }
}

fn side_bytes(paragraphs: &[Arc<Paragraph>], other: &[Arc<Paragraph>]) -> usize {
    let texts = Allocations::new(other, |paragraph| paragraph.text.as_ptr().addr());
    let runs = Allocations::new(other, |paragraph| paragraph.spans.as_ptr().addr());
    let shift = other.len().wrapping_sub(paragraphs.len());
    let mut charged = HashSet::new();
    paragraphs
        .iter()
        .enumerate()
        .map(|(index, paragraph)| {
            let near = [index, index.wrapping_add(shift)];
            let mut bytes = std::mem::size_of::<Paragraph>();
            if !paragraph.text.is_empty() && !texts.contains(paragraph, near) {
                bytes = bytes.saturating_add(paragraph.text.len());
            }
            if !paragraph.spans.is_empty() && !runs.contains(paragraph, near) {
                bytes = bytes.saturating_add(std::mem::size_of_val(paragraph.spans()));
                if paragraph.linked {
                    let near = near.map(|index| other.get(index).map(Arc::as_ref));
                    bytes = bytes.saturating_add(link_bytes(paragraph, near, &mut charged));
                }
            }
            bytes
        })
        .fold(0, usize::saturating_add)
}

fn link_bytes(
    paragraph: &Paragraph,
    near: [Option<&Paragraph>; 2],
    charged: &mut HashSet<usize>,
) -> usize {
    let links = |paragraph: &Paragraph| {
        paragraph
            .spans
            .iter()
            .filter_map(|span| span.link.as_ref().map(crate::Link::address))
            .collect::<Vec<_>>()
    };
    let own = links(paragraph);
    let near: Vec<Vec<usize>> = near
        .into_iter()
        .flatten()
        .filter(|other| other.linked)
        .map(links)
        .collect();
    let (mut prefix, mut suffix) = (0, 0);
    for other in &near {
        prefix = prefix.max(own.iter().zip(other).take_while(|(a, b)| a == b).count());
        suffix = suffix.max(
            own.iter()
                .rev()
                .zip(other.iter().rev())
                .take_while(|(a, b)| a == b)
                .count(),
        );
    }
    let start = prefix.min(own.len());
    let middle = &own[start..own.len().saturating_sub(suffix).max(start)];
    if middle.is_empty() {
        return 0;
    }
    let shared: HashSet<usize> = near.iter().flatten().copied().collect();
    let targets = paragraph
        .spans
        .iter()
        .filter_map(|span| span.link.as_ref())
        .skip(start)
        .take(middle.len());
    targets
        .filter(|link| !shared.contains(&link.address()) && charged.insert(link.address()))
        .map(|link| link.as_str().len())
        .fold(0, usize::saturating_add)
}

struct Allocations<'a> {
    paragraphs: &'a [Arc<Paragraph>],
    address: fn(&Paragraph) -> usize,
    set: OnceCell<HashSet<usize>>,
}

impl<'a> Allocations<'a> {
    fn new(paragraphs: &'a [Arc<Paragraph>], address: fn(&Paragraph) -> usize) -> Self {
        Self {
            paragraphs,
            address,
            set: OnceCell::new(),
        }
    }

    fn contains(&self, paragraph: &Paragraph, near: [usize; 2]) -> bool {
        let target = (self.address)(paragraph);
        let matches = |candidate: &Arc<Paragraph>| (self.address)(candidate) == target;
        if near
            .iter()
            .any(|index| self.paragraphs.get(*index).is_some_and(matches))
        {
            return true;
        }
        if self.paragraphs.len() <= 8 {
            return self.paragraphs.iter().any(matches);
        }
        self.set
            .get_or_init(|| {
                self.paragraphs
                    .iter()
                    .map(|paragraph| (self.address)(paragraph))
                    .collect()
            })
            .contains(&target)
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
    fn sparse_changes_and_their_history_notify_one_covering_range() {
        let heading = ParagraphKind::Heading { level: 1 };
        let mut document = Document::from_text(&"plain\n".repeat(30));
        document.set_kind(2..3, heading).unwrap();
        document.set_kind(25..26, heading).unwrap();
        let original = document.content_identity();
        let delta = document
            .set_kind(0..31, ParagraphKind::Body)
            .unwrap()
            .unwrap();
        assert_eq!(delta.rest.len(), 1);
        assert_change(&document, &original, 2..26, 24);
        let changed = document.content_identity();
        document.replay(&delta, false);
        assert_change(&document, &changed, 2..26, 24);
        let undone = document.content_identity();
        document.replay(&delta, true);
        assert_change(&document, &undone, 2..26, 24);
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
