#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
use std::cell::Cell;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::{Arc, OnceLock};
use std::{iter, mem};

use super::{Document, Paragraph};
use crate::Link;

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

/// Which way [`Document::replay`] applies a [`Delta`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Replay {
    /// Restores the paragraphs the delta replaced.
    Undo,
    /// Reapplies the delta's replacement paragraphs.
    Redo,
}

impl Document {
    pub(crate) fn replay(&mut self, delta: &Delta, step: Replay) {
        let forward = step == Replay::Redo;
        if delta.is_empty() {
            return;
        }
        if !delta.rest.is_empty() {
            count_sparse(SparsePath::Replayed);
        }
        let extent = delta.extent();
        self.apply(delta, forward);
        if forward {
            self.record_change(delta.start, extent.before, extent.after);
        } else {
            self.record_change(delta.start, extent.after, extent.before);
        }
    }

    fn apply(&mut self, delta: &Delta, forward: bool) {
        if delta.rest.is_empty()
            || delta
                .splices()
                .all(|splice| splice.before.len() == splice.after.len())
        {
            for splice in delta.splices() {
                let Sides { remove, insert } = splice.sides(forward);
                debug_assert_eq!(
                    self.paragraphs
                        .get(splice.start..splice.start + remove.len()),
                    Some(remove),
                    "history must be replayed against its corresponding document state"
                );
                self.paragraphs.splice(
                    splice.start..splice.start + remove.len(),
                    insert.iter().cloned(),
                );
                self.retain_numbers(remove, insert);
            }
            return;
        }
        count_sparse(SparsePath::ShiftedTail);
        // Splicing each run in place would shift the tail once per run.
        let mut tail = self.paragraphs.split_off(delta.start).into_iter();
        let mut end = delta.start;
        for splice in delta.splices() {
            let Sides { remove, insert } = splice.sides(forward);
            self.paragraphs
                .extend(tail.by_ref().take(splice.start - end));
            for expected in remove {
                let removed = tail.next();
                debug_assert_eq!(
                    removed.as_ref(),
                    Some(expected),
                    "history must be replayed against its corresponding document state"
                );
            }
            self.paragraphs.extend(insert.iter().cloned());
            self.retain_numbers(remove, insert);
            end = splice.start + splice.after.len();
        }
        self.paragraphs.extend(tail);
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
        let runs = changed_runs(&self.paragraphs[start..end], &after);
        let delta = if runs.len() > 1 {
            count_sparse(SparsePath::Recorded);
            let mut splices = runs.into_iter().map(|run| Delta {
                start: start + run.after.start,
                before: self.paragraphs[start + run.before.start..start + run.before.end].to_vec(),
                after: after[run.after].to_vec(),
                rest: Vec::new(),
            });
            let mut delta = splices.next().expect("this branch has several runs");
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
            self.apply(&delta, /* forward */ true);
            self.record_change(start, end - start, new_len);
        }
        delta
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
            let previous_identity = mem::replace(&mut self.identity, Arc::new(()));
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

// A code path for deltas that keep only the changed runs of paragraphs, counted in
// tests so that randomized tests can check that they exercised each one.
#[derive(Clone, Copy)]
enum SparsePath {
    Recorded,
    Replayed,
    ShiftedTail,
}

// How many times each sparse delta path ran on this thread.
#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SparseCoverage {
    pub(crate) recorded: usize,
    pub(crate) replayed: usize,
    // Sparse applies that shift the paragraphs after the change.
    pub(crate) shifted_tail: usize,
}

#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
thread_local! {
    static SPARSE_COVERAGE: Cell<SparseCoverage> = const {
        Cell::new(SparseCoverage {
            recorded: 0,
            replayed: 0,
            shifted_tail: 0,
        })
    };
}

// Returns the counts since the last call and resets them.
#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
pub(crate) fn take_sparse_coverage() -> SparseCoverage {
    SPARSE_COVERAGE.take()
}

#[cfg(all(test, any(feature = "egui", feature = "accesskit")))]
fn count_sparse(path: SparsePath) {
    SPARSE_COVERAGE.with(|coverage| {
        let mut counts = coverage.get();
        match path {
            SparsePath::Recorded => counts.recorded += 1,
            SparsePath::Replayed => counts.replayed += 1,
            SparsePath::ShiftedTail => counts.shifted_tail += 1,
        }
        coverage.set(counts);
    });
}

#[cfg(not(all(test, any(feature = "egui", feature = "accesskit"))))]
fn count_sparse(_: SparsePath) {}

// A changed run of paragraphs in the old list and the run that replaces it in the new list.
struct ChangedRun {
    before: Range<usize>,
    after: Range<usize>,
}

// Pairs each changed run of `before` with the run of `after` replacing it. Paragraphs kept by
// pointer separate the runs once more than SHARED_GAP of them sit together. A kept paragraph
// stays aligned unless an earlier run changed the paragraph count; then it is found by
// pointer. A kept paragraph is also owned by `before`, so one with a single owner is new.
fn changed_runs(before: &[Arc<Paragraph>], after: &[Arc<Paragraph>]) -> Vec<ChangedRun> {
    let mut runs: Vec<ChangedRun> = Vec::new();
    if before.len().min(after.len()) <= SHARED_GAP {
        return runs;
    }
    let mut positions: Option<HashMap<usize, usize>> = None;
    let (mut old, mut new) = (0, 0);
    while old < before.len() || new < after.len() {
        if before
            .get(old)
            .zip(after.get(new))
            .is_some_and(|(before, after)| Arc::ptr_eq(before, after))
        {
            old += 1;
            new += 1;
            continue;
        }
        // The next kept paragraph, by its indexes in `before` and `after`.
        let mut next = (before.len(), after.len());
        let candidates = after.iter().enumerate().skip(new);
        for (index, paragraph) in candidates {
            let aligned = old + index - new;
            if index > new
                && before
                    .get(aligned)
                    .is_some_and(|kept| Arc::ptr_eq(kept, paragraph))
            {
                next = (aligned, index);
                break;
            }
            if Arc::strong_count(paragraph) < 2 {
                continue;
            }
            if let Some(kept) = positions
                .get_or_insert_with(|| positions_by_pointer(before))
                .get(&Arc::as_ptr(paragraph).addr())
                .filter(|kept| **kept >= old)
            {
                next = (*kept, index);
                break;
            }
        }
        let (next_old, next_new) = next;
        match runs.last_mut() {
            Some(run) if old - run.before.end <= SHARED_GAP => {
                run.before.end = next_old;
                run.after.end = next_new;
            }
            _ => runs.push(ChangedRun {
                before: old..next_old,
                after: new..next_new,
            }),
        }
        (old, new) = (next_old, next_new);
    }
    runs
}

fn positions_by_pointer(paragraphs: &[Arc<Paragraph>]) -> HashMap<usize, usize> {
    let mut positions = HashMap::with_capacity(paragraphs.len());
    for (position, paragraph) in paragraphs.iter().enumerate() {
        positions
            .entry(Arc::as_ptr(paragraph).addr())
            .or_insert(position);
    }
    positions
}

#[derive(Clone, Debug)]
pub(crate) struct Delta {
    pub start: usize,
    pub before: Vec<Arc<Paragraph>>,
    pub after: Vec<Arc<Paragraph>>,
    // Later disjoint splices in document order, each positioned after its predecessors apply.
    pub rest: Vec<Delta>,
}

impl Delta {
    pub(super) fn empty(start: usize) -> Self {
        Self {
            start,
            before: Vec::new(),
            after: Vec::new(),
            rest: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.before.is_empty() && self.after.is_empty() && self.rest.is_empty()
    }

    pub fn retained_bytes(&self) -> usize {
        let runs = self.rest.len().saturating_mul(mem::size_of::<Delta>());
        self.splices()
            .map(|splice| {
                let pointers = (splice.before.len() + splice.after.len())
                    .saturating_mul(mem::size_of::<Arc<Paragraph>>());
                side_bytes(&splice.before, &splice.after)
                    .max(side_bytes(&splice.after, &splice.before))
                    .saturating_add(pointers)
            })
            .fold(runs, usize::saturating_add)
    }

    fn splices(&self) -> impl Iterator<Item = &Delta> {
        iter::once(self).chain(&self.rest)
    }

    fn sides(&self, forward: bool) -> Sides<'_> {
        if forward {
            Sides {
                remove: &self.before,
                insert: &self.after,
            }
        } else {
            Sides {
                remove: &self.after,
                insert: &self.before,
            }
        }
    }

    // Counts the paragraphs from `start` through the last splice, before and after applying.
    fn extent(&self) -> Extent {
        let last = self.rest.last().unwrap_or(self);
        let end = last.start + last.after.len();
        let (removed, inserted) = self.splices().fold((0, 0), |(removed, inserted), splice| {
            (removed + splice.before.len(), inserted + splice.after.len())
        });
        Extent {
            before: end + removed - inserted - self.start,
            after: end - self.start,
        }
    }
}

// The paragraphs one splice removes and inserts when replayed in a given direction.
struct Sides<'delta> {
    remove: &'delta [Arc<Paragraph>],
    insert: &'delta [Arc<Paragraph>],
}

// The number of paragraphs a delta's covered range spans before and after it applies.
struct Extent {
    before: usize,
    after: usize,
}

fn side_bytes(paragraphs: &[Arc<Paragraph>], other: &[Arc<Paragraph>]) -> usize {
    let texts = Allocations::new(other, |paragraph| paragraph.text.as_ptr().addr());
    let runs = Allocations::new(other, |paragraph| paragraph.spans.as_ptr().addr());
    let shift = other.len().wrapping_sub(paragraphs.len());
    let mut charged = HashSet::new();
    let mut total: usize = 0;
    for (index, paragraph) in paragraphs.iter().enumerate() {
        let near = [index, index.wrapping_add(shift)];
        let mut bytes = mem::size_of::<Paragraph>();
        if !paragraph.text.is_empty() && !texts.contains(paragraph, near) {
            bytes = bytes.saturating_add(paragraph.text.len());
        }
        if !paragraph.spans.is_empty() && !runs.contains(paragraph, near) {
            bytes = bytes.saturating_add(mem::size_of_val(paragraph.spans()));
            if paragraph.linked {
                let near = near.map(|index| other.get(index).map(Arc::as_ref));
                bytes = bytes.saturating_add(link_bytes(paragraph, near, &mut charged));
            }
        }
        total = total.saturating_add(bytes);
    }
    total
}

fn link_bytes(
    paragraph: &Paragraph,
    near: [Option<&Paragraph>; 2],
    charged: &mut HashSet<usize>,
) -> usize {
    let links = |paragraph: &Paragraph| -> Vec<usize> {
        paragraph
            .spans
            .iter()
            .filter_map(|span| span.link.as_ref().map(Link::address))
            .collect()
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
        prefix = prefix.max(
            own.iter()
                .zip(other)
                .take_while(|(left, right)| left == right)
                .count(),
        );
        suffix = suffix.max(
            own.iter()
                .rev()
                .zip(other.iter().rev())
                .take_while(|(left, right)| left == right)
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
    let mut bytes: usize = 0;
    for link in targets {
        if !shared.contains(&link.address()) && charged.insert(link.address()) {
            bytes = bytes.saturating_add(link.as_str().len());
        }
    }
    bytes
}

struct Allocations<'paragraphs> {
    paragraphs: &'paragraphs [Arc<Paragraph>],
    address: fn(&Paragraph) -> usize,
    set: OnceCell<HashSet<usize>>,
}

impl<'paragraphs> Allocations<'paragraphs> {
    fn new(paragraphs: &'paragraphs [Arc<Paragraph>], address: fn(&Paragraph) -> usize) -> Self {
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
    use crate::model::{Indent, Replacement};
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
        let Replacement { delta, .. } = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "ONE",
                InlineStyle::default(),
            )
            .unwrap();
        assert_change(&document, &original, 1..2, 1);
        let changed = document.content_identity();
        let Replacement { delta: empty, .. } = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "ONE",
                InlineStyle::default(),
            )
            .unwrap();
        assert!(empty.is_empty());
        document.replay(&empty, Replay::Redo);
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
        document.replay(&delta, Replay::Undo);
        assert_change(&document, &changed, 1..2, 1);
    }

    #[test]
    fn structural_edits_and_history_notify_in_their_own_paragraph_coordinates() {
        let mut document = Document::from_text("zero\none\ntwo");
        let original = document.content_identity();
        let Replacement { delta, .. } = document
            .replace(
                Position::new(1, 0)..Position::new(1, 3),
                "first\nmiddle\nlast",
                InlineStyle::default(),
            )
            .unwrap();
        assert_change(&document, &original, 1..2, 3);
        let inserted = document.content_identity();
        document.replay(&delta, Replay::Undo);
        assert_change(&document, &inserted, 1..4, 1);
        let undone = document.content_identity();
        document.replay(&delta, Replay::Redo);
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
        let delta = document.set_kind(0..31, ParagraphKind::Body).unwrap();
        assert_eq!(delta.rest.len(), 1);
        assert_change(&document, &original, 2..26, 24);
        let changed = document.content_identity();
        document.replay(&delta, Replay::Undo);
        assert_change(&document, &changed, 2..26, 24);
        let undone = document.content_identity();
        document.replay(&delta, Replay::Redo);
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
        document.change_list_indent(0..3, Indent::Increase);
        assert_change(&document, &listed, 1..2, 1);
        let indented = document.content_identity();
        assert!(
            document
                .change_list_indent(0..1, Indent::Increase)
                .is_empty()
        );
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
