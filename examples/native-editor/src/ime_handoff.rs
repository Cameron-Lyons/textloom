use eframe::egui::{self, Event, ImeEvent, Key};

fn newline(text: &str) -> bool {
    matches!(text, "\n" | "\r" | "\r\n")
}

fn valid_preedit(text: &str, range: &Option<std::ops::Range<usize>>) -> bool {
    range
        .as_ref()
        .is_none_or(|range| range.start <= range.end && range.end <= text.chars().count())
}

fn allowed(character: char) -> bool {
    !character.is_control() || matches!(character, '\n' | '\r' | '\t')
}

pub fn has_empty_preedit(events: &[Event]) -> bool {
    events.iter().any(|event| {
        matches!(event, Event::Ime(ImeEvent::Preedit { text, active_range_chars })
            if text.is_empty() && valid_preedit(text, active_range_chars))
    })
}

// The document consumes queued IME events before a pointer press when it loses focus.
// Retain only its final pending candidate; a normal commit or surrounding deletion
// ends that candidate, while an empty preedit or cancellation can still be echoed.
pub fn candidate(
    composition: Option<&textloom::Composition>,
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
            }) if !text.is_empty() && !newline(text) && valid_preedit(text, active_range_chars) => {
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
    candidate
        .map(|text| {
            text.chars()
                .filter(|character| allowed(*character))
                .collect::<String>()
        })
        .filter(|text| !text.is_empty())
}

pub struct Guard {
    target: egui::Id,
    text: String,
    time: f64,
}

impl Guard {
    pub fn new(target: egui::Id, text: String, time: f64) -> Self {
        Self { target, text, time }
    }

    // Match the document adapter's bounded, single-echo policy, scoped to the field
    // that received focus. Fresh input belongs to that field and ends the guard.
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
                if newline(text) || !valid_preedit(text, active_range_chars))
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
                }) => !text.is_empty() && !newline(text) && valid_preedit(text, active_range_chars),
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
