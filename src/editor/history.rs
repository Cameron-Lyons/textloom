use std::collections::VecDeque;

use crate::document::Delta;
use crate::{Document, InlineStyle, Selection};

/// Limits apply to retained undo and redo entries together.
/// The default retains at most 256 entries and an estimated 8 MiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    /// Maximum total entries across both history directions; zero disables history.
    pub max_entries: usize,
    /// Maximum estimated retained bytes; zero disables history.
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

struct HistoryEntry {
    delta: Delta,
    before: EditState,
    after: EditState,
    typing: bool,
    bytes: usize,
}

/// Retained edit chains and their accounting, independent of transient editor state.
#[derive(Default)]
pub(super) struct History {
    undo: VecDeque<HistoryEntry>,
    redo: VecDeque<HistoryEntry>,
    bytes: usize,
    limits: HistoryLimits,
    coalesce_typing: bool,
}

impl History {
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
        self.coalesce_typing = false;
    }

    pub(super) fn undo(&mut self, document: &mut Document) -> Option<EditState> {
        let entry = self.undo.pop_back()?;
        document.replay(&entry.delta, false);
        let state = entry.before;
        self.redo.push_back(entry);
        Some(state)
    }

    pub(super) fn redo(&mut self, document: &mut Document) -> Option<EditState> {
        let entry = self.redo.pop_back()?;
        document.replay(&entry.delta, true);
        let state = entry.after;
        self.undo.push_back(entry);
        Some(state)
    }

    pub(super) fn record(
        &mut self,
        delta: Delta,
        before: EditState,
        after: EditState,
        typing: bool,
    ) {
        for entry in self.redo.drain(..) {
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
        if self.limits.max_entries == 0 || self.limits.max_bytes == 0 {
            self.coalesce_typing = false;
            return;
        }
        let entry = HistoryEntry {
            bytes: delta.retained_bytes(),
            delta,
            before,
            after,
            typing,
        };
        let merge = self.coalesce_typing
            && typing
            && self.undo.back().is_some_and(|previous| {
                previous.typing
                    && previous.after.selection == before.selection
                    && previous.after.style == before.style
                    && previous.delta.start == entry.delta.start
                    && previous.delta.before.len() == 1
                    && previous.delta.after.len() == 1
                    && entry.delta.before.len() == 1
                    && entry.delta.after.len() == 1
            });
        if merge {
            let previous = self.undo.back_mut().expect("checked entry");
            self.bytes = self.bytes.saturating_sub(previous.bytes);
            previous.delta.after = entry.delta.after;
            previous.after = after;
            previous.bytes = previous.delta.retained_bytes();
            self.bytes = self.bytes.saturating_add(previous.bytes);
        } else {
            self.bytes = self.bytes.saturating_add(entry.bytes);
            self.undo.push_back(entry);
        }
        self.coalesce_typing = typing;
        self.enforce_limits();
    }

    fn enforce_limits(&mut self) {
        while self.undo.len() + self.redo.len() > self.limits.max_entries
            || self.bytes > self.limits.max_bytes
        {
            // Dropping oldest undo or farthest redo preserves the applicable chain.
            let entry = self.undo.pop_front().or_else(|| self.redo.pop_front());
            let Some(entry) = entry else {
                break;
            };
            self.bytes = self.bytes.saturating_sub(entry.bytes);
        }
    }
}
