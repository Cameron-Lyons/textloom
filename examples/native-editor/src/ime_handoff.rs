//! Keeps a composition's late commit echo out of a newly focused text field.
//!
//! When focus moves from the document to the Link, Find, or Replace field
//! during an IME composition, the platform can deliver the old composition's
//! commit after the move, and the field would type it a second time. The host
//! computes the text that could be echoed with [`candidate`] and installs a
//! [`Guard`] that drops one identical commit from the field's input.

use std::ops::Range;

use eframe::egui::{self, Event, ImeEvent, Key};
use textloom::Composition;

/// Whether `text` is exactly one line break: LF, CR, or CRLF.
fn newline(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

/// Whether a preedit's active range, in characters, lies within `text`.
fn valid_preedit(text: &str, range: Option<&Range<usize>>) -> bool {
    range.is_none_or(|range| range.start <= range.end && range.end <= text.chars().count())
}

/// Whether `character` can be inserted from IME input: control characters
/// other than line breaks and tabs are dropped.
fn allowed(character: char) -> bool {
    !character.is_control() || matches!(character, '\n' | '\r' | '\t')
}

/// Whether `events` contain a valid empty preedit, after which the document's
/// composition is empty but its text can still be echoed.
pub fn has_empty_preedit(events: &[Event]) -> bool {
    events.iter().any(|event| {
        matches!(event, Event::Ime(ImeEvent::Preedit { text, active_range_chars })
            if text.is_empty() && valid_preedit(text, active_range_chars.as_ref()))
    })
}

/// Returns the document text that a commit could still echo after focus
/// leaves the document, or `None` if there is nothing to guard.
///
/// The candidate starts as the text of the visible `composition`, or as
/// `cleared` when an empty preedit already emptied the composition, and is
/// `None` without a composition. The document consumes the IME events in
/// `events` before it loses focus, so they update the candidate: a valid
/// nonempty preedit replaces it, while a commit that ends the composition
/// (an empty one or one with insertable text) or a surrounding-text deletion
/// ends it. An empty preedit, a line-break commit, or a commit of only control
/// characters keeps it, since the composition can still be echoed afterwards.
/// When `leaving` is true, focus moves at the first primary pointer press in
/// `events`, so the scan stops there. The result omits control characters
/// other than line breaks and tabs.
pub fn candidate(
    composition: Option<&Composition>,
    cleared: Option<&str>,
    events: &[Event],
    leaving: bool,
) -> Option<String> {
    let mut candidate = composition.and_then(|composition| {
        if composition.text.is_empty() {
            cleared
        } else {
            Some(composition.text.as_str())
        }
    });
    for event in events {
        if leaving
            && matches!(
                event,
                Event::PointerButton {
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                }
            )
        {
            break;
        }
        match event {
            Event::Ime(ImeEvent::Preedit {
                text,
                active_range_chars,
            }) if !text.is_empty()
                && !newline(text)
                && valid_preedit(text, active_range_chars.as_ref()) =>
            {
                candidate = Some(text);
            }
            Event::Ime(ImeEvent::Commit(text))
                if !newline(text) && (text.is_empty() || text.chars().any(allowed)) =>
            {
                candidate = None;
            }
            Event::Ime(ImeEvent::DeleteSurrounding { .. }) => candidate = None,
            _ => {}
        }
    }
    let text: String = candidate?
        .chars()
        .filter(|character| allowed(*character))
        .collect();
    (!text.is_empty()).then_some(text)
}

/// Drops one commit echo of a [`candidate`] from the input of the text field
/// that received focus.
pub struct Guard {
    /// The text field that received focus from the document.
    target: egui::Id,
    /// The echo to drop, as returned by [`candidate`].
    text: String,
    /// The egui input time, in seconds, when the guard was created; it expires
    /// half a second later.
    time: f64,
}

impl Guard {
    /// Guards `target` against an echo of `text`, starting at input time
    /// `time`.
    pub fn new(target: egui::Id, text: String, time: f64) -> Self {
        Self { target, text, time }
    }

    /// Filters `input` for the guarded field and reports whether the guard is
    /// still active.
    ///
    /// Until the guard ends, this removes the echo and the IME events that
    /// cannot start a new composition: line-break preedits and commits,
    /// preedits with an invalid range, and commits without insertable text.
    /// The guard ends when focus is not on the guarded field, after half a
    /// second, when the echo arrives, or at fresh input, which belongs to the
    /// field and is kept. Cut counts as fresh input only when the field has a
    /// nonempty selection (`selected`). This matches the document adapter's
    /// bounded, single-echo policy, scoped to the field that received focus.
    pub fn filter(
        &self,
        input: &mut egui::InputState,
        focused: Option<egui::Id>,
        selected: bool,
    ) -> bool {
        if focused != Some(self.target) || input.time - self.time > 0.5 {
            return false;
        }
        let mut active = true;
        input.events.retain(|event| {
            if !active {
                return true;
            }
            if matches!(event, Event::Ime(ImeEvent::Preedit { text, active_range_chars })
                if newline(text) || !valid_preedit(text, active_range_chars.as_ref()))
                || matches!(event, Event::Ime(ImeEvent::Commit(text))
                    if newline(text) || !text.chars().any(allowed))
            {
                // These cannot start a valid new document IME session. Do not pass
                // its ignored trailing events on to a newly focused text field.
                return false;
            }
            if matches!(event, Event::Ime(ImeEvent::Commit(text))
                if !newline(text) && text.chars().filter(|character| allowed(*character)).eq(self.text.chars()))
            {
                active = false;
                return false;
            }
            let fresh = match event {
                Event::Ime(ImeEvent::Preedit {
                    text,
                    active_range_chars,
                }) => {
                    !text.is_empty()
                        && !newline(text)
                        && valid_preedit(text, active_range_chars.as_ref())
                }
                Event::Ime(ImeEvent::Commit(text)) => {
                    !newline(text) && text.chars().any(allowed)
                }
                Event::Ime(ImeEvent::DeleteSurrounding { .. }) => true,
                Event::Text(text) => !text.is_empty() && !matches!(text.as_str(), "\n" | "\r"),
                Event::Paste(text) => !text.is_empty(),
                Event::Cut => selected,
                Event::PointerButton { pressed: true, .. } => true,
                Event::Key { key, pressed: true, modifiers, .. } => {
                    matches!(key, Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown
                        | Key::Home | Key::End | Key::PageUp | Key::PageDown
                        | Key::Backspace | Key::Delete | Key::Enter | Key::Escape | Key::Tab)
                        || modifiers.command && !modifiers.alt
                            && !(modifiers.mac_cmd && modifiers.ctrl)
                            && matches!(key, Key::A | Key::Z | Key::Y | Key::K)
                }
                _ => false,
            };
            active &= !fresh;
            true
        });
        active
    }
}
