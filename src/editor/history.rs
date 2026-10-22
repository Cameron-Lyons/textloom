use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use crate::document::{Delta, Replay};
use crate::{Document, InlineStyle, Selection};

/// Bounds on retained undo and redo history, set with
/// [`Editor::set_history_limits`](crate::Editor::set_history_limits).
///
/// When history exceeds a bound, the oldest undo steps are discarded first,
/// then the redo steps farthest from the current state, so a single step
/// larger than `max_bytes` is not kept. A zero bound disables history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    clippy::exhaustive_structs,
    reason = "built with struct update syntax from `HistoryLimits::default()`; a new field is a major release"
)]
pub struct HistoryLimits {
    /// The maximum number of undo and redo steps together; 256 by default.
    pub max_entries: usize,
    /// The maximum estimated memory of undo and redo steps together, in
    /// bytes, as [`Editor::history_bytes`](crate::Editor::history_bytes)
    /// reports it; 8 MiB by default.
    pub max_bytes: usize,
}

impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            max_entries: 256,
            max_bytes: 8 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct EditState {
    pub(super) selection: Selection,
    pub(super) style: InlineStyle,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Group {
    #[default]
    None,
    Typing,
    DeleteBackward,
    DeleteForward,
}

#[derive(Clone)]
struct HistoryEntry {
    deltas: Vec<Delta>,
    before: EditState,
    after: EditState,
    group: Group,
    bytes: usize,
}

impl HistoryEntry {
    fn new(deltas: Vec<Delta>, before: EditState, after: EditState, group: Group) -> Self {
        Self {
            bytes: retained_bytes(&deltas),
            deltas,
            before,
            after,
            group,
        }
    }

    fn can_extend(&self, delta: &Delta) -> bool {
        matches!(self.deltas.as_slice(), [previous] if extends(previous, delta))
    }
}

fn extends(previous: &Delta, delta: &Delta) -> bool {
    previous.rest.is_empty()
        && delta.rest.is_empty()
        && previous.start == delta.start
        && previous.after.len() == 1
        && delta.before.len() == 1
        && delta.after.len() == 1
}

#[derive(Clone)]
struct Transaction {
    token: u64,
    depth: usize,
    scope: Weak<()>,
    deltas: Vec<Delta>,
    before: EditState,
    marks: Vec<Mark>,
}

// Where a nested transaction began: the number of deltas recorded before it, and a weak
// handle to the scope token that its guard holds.
#[derive(Clone)]
struct Mark {
    deltas: usize,
    scope: Weak<()>,
}

#[derive(Clone, Copy)]
pub(super) struct TransactionLevel {
    token: u64,
    depth: usize,
}

// A newly opened transaction level and the scope token that its guard keeps alive.
pub(super) struct OpenTransaction {
    pub(super) level: TransactionLevel,
    pub(super) scope: Arc<()>,
}

#[derive(Default)]
pub(super) struct History {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    bytes: usize,
    limits: HistoryLimits,
    group: Group,
    transaction: Option<Transaction>,
    // Immutable queries can seal an editor moved out of its transaction closure.
    // Its next mutation adopts this snapshot before changing the editor state.
    detached: OnceLock<Box<History>>,
}

impl Clone for History {
    fn clone(&self) -> Self {
        if let Some(history) = self.detached.get() {
            return history.as_ref().clone();
        }
        Self {
            undo: self.undo.clone(),
            redo: self.redo.clone(),
            bytes: self.bytes,
            limits: self.limits,
            group: self.group,
            transaction: self.transaction.clone(),
            detached: OnceLock::new(),
        }
    }
}

impl History {
    fn is_detached(&self) -> bool {
        self.transaction.as_ref().is_some_and(|transaction| {
            // A moved editor may have a live nested guard on another thread
            // after its original outer guard has ended.
            transaction.scope.strong_count() == 0
                && transaction
                    .marks
                    .iter()
                    .all(|mark| mark.scope.strong_count() == 0)
        })
    }

    #[inline]
    pub(super) fn resolved(&self, after: EditState) -> &Self {
        if self.transaction.is_none() {
            self
        } else {
            self.resolve_transaction(after)
        }
    }

    fn resolve_transaction(&self, after: EditState) -> &Self {
        if !self.is_detached() {
            return self;
        }
        self.detached.get_or_init(|| {
            let mut history = self.clone();
            history.close_transactions(after);
            Box::new(history)
        })
    }

    #[inline]
    pub(super) fn normalize(&mut self, after: EditState) {
        if self.transaction.is_some() {
            self.normalize_transaction(after);
        }
    }

    fn normalize_transaction(&mut self, after: EditState) {
        if self.is_detached() {
            if let Some(history) = self.detached.take() {
                *self = *history;
            } else {
                self.close_transactions(after);
            }
        } else if let Some(transaction) = &mut self.transaction {
            // A nested guard may have finished after its editor was moved out.
            // Its edits join the still-active outer transaction.
            while transaction
                .marks
                .last()
                .is_some_and(|mark| mark.scope.strong_count() == 0)
            {
                transaction.marks.pop();
                transaction.depth -= 1;
            }
        }
    }

    pub(super) fn limits(&self) -> HistoryLimits {
        self.limits
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub(super) fn redo_len(&self) -> usize {
        self.redo.len()
    }

    pub(super) fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    pub(super) fn set_limits(&mut self, limits: HistoryLimits) {
        self.limits = limits;
        self.break_group();
        self.enforce_limits();
    }

    pub(super) fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.bytes = 0;
        self.break_group();
    }

    pub(super) fn break_group(&mut self) {
        self.group = Group::None;
    }

    pub(super) fn undo(&mut self, document: &mut Document) -> Option<EditState> {
        if self.in_transaction() {
            return None;
        }
        let entry = self.undo.pop_back()?;
        for delta in entry.deltas.iter().rev() {
            document.replay(delta, Replay::Undo);
        }
        let state = entry.before;
        self.redo.push_back(entry);
        Some(state)
    }

    pub(super) fn redo(&mut self, document: &mut Document) -> Option<EditState> {
        if self.in_transaction() {
            return None;
        }
        let entry = self.redo.pop_back()?;
        for delta in &entry.deltas {
            document.replay(delta, Replay::Redo);
        }
        let state = entry.after;
        self.undo.push_back(entry);
        Some(state)
    }

    pub(super) fn record(
        &mut self,
        delta: Delta,
        before: EditState,
        after: EditState,
        group: Group,
    ) {
        self.normalize(before);
        if let Some(transaction) = &mut self.transaction {
            let floor = transaction.marks.last().map_or(0, |mark| mark.deltas);
            let pending = transaction.deltas.len();
            match transaction.deltas.last_mut() {
                Some(previous) if pending > floor && extends(previous, &delta) => {
                    previous.after = delta.after;
                }
                _ => transaction.deltas.push(delta),
            }
            return;
        }
        self.discard_redo();
        if self.is_disabled() {
            self.break_group();
            return;
        }
        let merge = group != Group::None
            && self.group == group
            && self.undo.back().is_some_and(|previous| {
                previous.group == group
                    && previous.after.selection == before.selection
                    && previous.after.style == before.style
                    && previous.can_extend(&delta)
            });
        if merge {
            let previous = self
                .undo
                .back_mut()
                .expect("merging requires the previous undo entry checked above");
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            previous.deltas[0].after = delta.after;
            previous.after = after;
            previous.bytes = retained_bytes(&previous.deltas);
            self.bytes = self.bytes.saturating_add(previous.bytes);
        } else {
            self.push(HistoryEntry::new(vec![delta], before, after, group));
        }
        self.group = group;
        self.enforce_limits();
    }

    pub(super) fn begin_transaction(&mut self, before: EditState) -> OpenTransaction {
        self.normalize(before);
        static TOKENS: AtomicU64 = AtomicU64::new(0);
        self.break_group();
        let scope = Arc::new(());
        if let Some(transaction) = &mut self.transaction {
            transaction.marks.push(Mark {
                deltas: transaction.deltas.len(),
                scope: Arc::downgrade(&scope),
            });
            transaction.depth += 1;
            OpenTransaction {
                level: TransactionLevel {
                    token: transaction.token,
                    depth: transaction.depth,
                },
                scope,
            }
        } else {
            let token = TOKENS.fetch_add(1, Ordering::Relaxed);
            self.transaction = Some(Transaction {
                token,
                depth: 1,
                scope: Arc::downgrade(&scope),
                deltas: Vec::new(),
                before,
                marks: Vec::new(),
            });
            OpenTransaction {
                level: TransactionLevel { token, depth: 1 },
                scope,
            }
        }
    }

    fn transaction_at(&mut self, level: TransactionLevel) -> Option<&mut Transaction> {
        self.transaction.as_mut().filter(|transaction| {
            transaction.token == level.token && transaction.depth == level.depth
        })
    }

    pub(super) fn commit_transaction(&mut self, level: TransactionLevel, after: EditState) {
        let Some(transaction) = self.transaction_at(level) else {
            return;
        };
        transaction.depth -= 1;
        if transaction.depth != 0 {
            transaction.marks.pop();
            return;
        }
        let Some(transaction) = self.transaction.take() else {
            return;
        };
        self.break_group();
        if transaction.deltas.is_empty() {
            return;
        }
        self.discard_redo();
        if self.is_disabled() {
            return;
        }
        self.push(HistoryEntry::new(
            transaction.deltas,
            transaction.before,
            after,
            Group::None,
        ));
        self.enforce_limits();
    }

    pub(super) fn rollback_transaction(
        &mut self,
        level: TransactionLevel,
        document: &mut Document,
    ) -> bool {
        let Some(transaction) = self.transaction_at(level) else {
            return false;
        };
        let mark = transaction
            .marks
            .pop()
            .map_or(0, |mark| mark.deltas)
            .min(transaction.deltas.len());
        for delta in transaction.deltas.drain(mark..).rev() {
            document.replay(&delta, Replay::Undo);
        }
        transaction.depth -= 1;
        if transaction.depth == 0 {
            self.transaction = None;
        }
        self.break_group();
        true
    }

    pub(super) fn close_transactions(&mut self, after: EditState) {
        if let Some(transaction) = &mut self.transaction {
            transaction.depth = 1;
            transaction.marks.clear();
            let level = TransactionLevel {
                token: transaction.token,
                depth: 1,
            };
            self.commit_transaction(level, after);
        }
    }

    fn is_disabled(&self) -> bool {
        self.limits.max_entries == 0 || self.limits.max_bytes == 0
    }

    fn push(&mut self, entry: HistoryEntry) {
        self.bytes = self.bytes.saturating_add(entry.bytes);
        self.undo.push_back(entry);
    }

    fn discard_redo(&mut self) {
        for entry in self.redo.drain(..) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
    }

    fn enforce_limits(&mut self) {
        while self.undo.len() + self.redo.len() > self.limits.max_entries
            || self.bytes > self.limits.max_bytes
        {
            let entry = self.undo.pop_front().or_else(|| self.redo.pop_front());
            let Some(entry) = entry else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
    }
}

fn retained_bytes(deltas: &[Delta]) -> usize {
    deltas
        .iter()
        .map(Delta::retained_bytes)
        .fold(0, usize::saturating_add)
}
