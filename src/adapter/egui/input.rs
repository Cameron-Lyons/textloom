use super::{
    CommitEcho, RichClipboard,
    layout::ParagraphLayouts,
    navigation::{
        VerticalNavigation, page_position, preedit_byte_range, row_boundary_position,
        vertical_position,
    },
    record,
};
use crate::{
    Document, Editor, Error, Movement, ParagraphKind, Position, Selection,
    adapter::{Shortcut, clean_text, shortcut_key_name},
};
use egui::{Event, ImeEvent, Key, Modifiers, Ui};

pub(super) fn copy_selection(
    editor: &Editor,
    ui: &Ui,
    clipboard: &mut Option<&mut dyn RichClipboard>,
) {
    if editor.selection().is_caret() {
        return;
    }
    let copied = clipboard
        .as_deref_mut()
        .is_some_and(|clipboard| clipboard.copy(&editor.selected_fragment()));
    if copied {
        ui.ctx().output_mut(|output| {
            output.commands.retain(|command| {
                !matches!(
                    command,
                    egui::OutputCommand::CopyText(_) | egui::OutputCommand::CopyImage(_)
                )
            });
        });
    } else {
        ui.ctx().copy_text(editor.selected_text());
    }
}

#[derive(Clone, Copy)]
pub(super) struct InputOptions {
    pub(super) read_only: bool,
    pub(super) page_height: f32,
    pub(super) heading_shortcuts: bool,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_event(
    editor: &mut Editor,
    ui: &Ui,
    event: &Event,
    layouts: &ParagraphLayouts,
    navigation: &mut Option<VerticalNavigation>,
    commit_echo: &mut Option<CommitEcho>,
    errors: &mut Vec<Error>,
    interrupted: &mut bool,
    options: &InputOptions,
    clipboard: &mut Option<&mut dyn RichClipboard>,
) -> bool {
    if options.read_only {
        match event {
            Event::Cut => {
                copy_selection(editor, ui, clipboard);
                return true;
            }
            Event::Text(_) | Event::Paste(_) | Event::Ime(_) => return true,
            _ => {}
        }
    }
    if matches!(event, Event::Cut | Event::Paste(_) | Event::Text(_))
        && editor
            .composition()
            .is_some_and(|composition| !composition.text.is_empty())
    {
        return true;
    }
    let input_text = match event {
        Event::Text(text) => Some((text, clean_text(text, false))),
        Event::Paste(text) => Some((text, clean_text(text, true))),
        _ => None,
    };
    if input_text
        .as_ref()
        .is_some_and(|(_, clean)| clean.is_empty())
    {
        return true;
    }
    if matches!(event, Event::Text(_) | Event::Paste(_)) {
        *interrupted |= cancel_empty_composition(editor);
    }
    match event {
        Event::Copy => {
            copy_selection(editor, ui, clipboard);
            true
        }
        Event::Cut => {
            if !editor.selection().is_caret() {
                *interrupted |= cancel_empty_composition(editor);
                *commit_echo = None;
                copy_selection(editor, ui, clipboard);
                editor.break_history_group();
                record(editor.insert_text(""), errors);
                editor.break_history_group();
            }
            true
        }
        Event::Paste(_) => {
            if let Some((text, clean)) = &input_text {
                *commit_echo = None;
                let plain = ui.input(|input| input.modifiers.command && input.modifiers.shift);
                let fragment = clipboard
                    .as_deref_mut()
                    .filter(|_| !plain)
                    .and_then(|clipboard| clipboard.paste(text));
                editor.break_history_group();
                record(
                    match fragment {
                        Some(fragment) => editor.insert_fragment(&fragment),
                        None => editor.insert_text(clean),
                    },
                    errors,
                );
                editor.break_history_group();
            }
            true
        }
        Event::Text(_) => {
            if let Some((_, clean)) = &input_text {
                *commit_echo = None;
                record(editor.insert_text(clean), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Preedit { text, .. } | ImeEvent::Commit(text))
            if matches!(text.as_str(), "\n" | "\r" | "\r\n") =>
        {
            true
        }
        Event::Ime(ImeEvent::Preedit {
            text,
            active_range_chars,
        }) => {
            let range = match active_range_chars
                .as_ref()
                .map(|range| preedit_byte_range(text, range))
                .transpose()
            {
                Ok(range) => range,
                Err(error) => {
                    errors.push(error);
                    return true;
                }
            };
            if !text.is_empty() || editor.composition().is_some() {
                if !text.is_empty() {
                    // Only an accepted candidate starts a new IME input session.
                    *commit_echo = None;
                }
                record(editor.update_composition(text, range), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Commit(text)) => {
            let clean = clean_text(text, true);
            if !clean.is_empty() || editor.composition().is_some() {
                *commit_echo = None;
            }
            if editor.composition().is_none() {
                if !clean.is_empty() {
                    record(editor.insert_text(&clean), errors);
                }
            } else if clean.is_empty() && !text.is_empty() {
                editor.cancel_composition();
            } else {
                record(editor.commit_composition(&clean), errors);
            }
            true
        }
        Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars,
            after_chars,
        }) => {
            *commit_echo = None;
            *interrupted |= editor.cancel_composition();
            delete_surrounding(editor, *before_chars, *after_chars, errors);
            true
        }
        #[allow(deprecated)]
        Event::Ime(ImeEvent::Disabled) => {
            editor.cancel_composition();
            true
        }
        #[allow(deprecated)]
        Event::Ime(ImeEvent::Enabled) => true,
        Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => {
            if *key == Key::Escape {
                if !options.read_only {
                    *commit_echo = None;
                }
                let canceled = editor.cancel_composition();
                *interrupted |= canceled;
                if !canceled {
                    ui.memory_mut(|memory| {
                        if let Some(id) = memory.focused() {
                            memory.surrender_focus(id);
                        }
                    });
                }
                return true;
            }
            if editor
                .composition()
                .is_some_and(|composition| !composition.text.is_empty())
            {
                return false;
            }
            // Printable and clipboard keys may deliver their actual input as a later event.
            let composing = editor.composition().is_some();
            let handled = handle_key(
                editor,
                *key,
                *physical_key,
                *modifiers,
                layouts,
                navigation,
                errors,
                options,
            );
            *interrupted |= composing && editor.composition().is_none();
            if handled && !options.read_only {
                *commit_echo = None;
            }
            handled
        }
        _ => false,
    }
}

fn cancel_empty_composition(editor: &mut Editor) -> bool {
    editor
        .composition()
        .is_some_and(|composition| composition.text.is_empty())
        && editor.cancel_composition()
}

fn resolve_shortcut(
    key: Key,
    physical_key: Option<Key>,
    shift: bool,
    headings: bool,
) -> Option<Shortcut> {
    let physical = physical_key
        .and_then(key_character)
        .filter(|key| key.is_ascii_alphanumeric() || matches!(key, '[' | ']'));
    let name = shortcut_key_name(key_character(key), physical)?;
    match Shortcut::new(name, shift)? {
        Shortcut::Copy | Shortcut::Cut | Shortcut::Paste => None,
        Shortcut::Paragraph(ParagraphKind::Body | ParagraphKind::Heading { .. }) if !headings => {
            None
        }
        shortcut => Some(shortcut),
    }
}

fn key_character(key: Key) -> Option<char> {
    match key {
        Key::Minus => Some('-'),
        Key::Quote => Some('\''),
        Key::Space => Some(' '),
        _ => {
            let mut characters = key.symbol_or_name().chars();
            characters
                .next()
                .filter(|character| character.is_ascii() && characters.next().is_none())
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_key(
    editor: &mut Editor,
    key: Key,
    physical_key: Option<Key>,
    modifiers: Modifiers,
    layouts: &ParagraphLayouts,
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    options: &InputOptions,
) -> bool {
    let command = modifiers.command && !modifiers.alt && !(modifiers.mac_cmd && modifiers.ctrl);
    let paging = !modifiers.command && !modifiers.alt;
    let prefer_next_row = navigation
        .as_ref()
        .is_none_or(|navigation| navigation.prefer_next_row);
    let reset_navigation = command
        || !(matches!(key, Key::ArrowUp | Key::ArrowDown)
            || paging && matches!(key, Key::PageUp | Key::PageDown));
    let previous_navigation = if reset_navigation {
        navigation.take()
    } else {
        None
    };
    if command
        && let Some(shortcut) = resolve_shortcut(
            key,
            physical_key,
            modifiers.shift,
            options.heading_shortcuts,
        )
    {
        let revision = editor.document().revision();
        let selection = editor.selection();
        if !options.read_only || !shortcut.edits() {
            cancel_empty_composition(editor);
            record(shortcut.apply(editor), errors);
        }
        if editor.document().revision() == revision && editor.selection() == selection {
            // Changing the pending typing style must preserve the visual caret.
            *navigation = previous_navigation;
        }
        return true;
    }
    let by_word = modifiers.ctrl || modifiers.alt;
    let row_end = match key {
        Key::ArrowLeft if command && modifiers.mac_cmd => Some(false),
        Key::ArrowRight if command && modifiers.mac_cmd => Some(true),
        Key::Home if !modifiers.command => Some(false),
        Key::End if !modifiers.command => Some(true),
        _ => None,
    };
    if let Some(end) = row_end {
        cancel_empty_composition(editor);
        if let Some((position, preferred_x, prefer_next_row)) =
            row_boundary_position(editor, layouts, prefer_next_row, end)
        {
            set_visual_selection(
                editor,
                navigation,
                errors,
                position,
                preferred_x,
                prefer_next_row,
                modifiers.shift,
            );
        } else {
            let movement = if end {
                Movement::ParagraphEnd
            } else {
                Movement::ParagraphStart
            };
            record(editor.move_cursor(movement, modifiers.shift), errors);
        }
        return true;
    }
    let movement = match key {
        Key::ArrowUp if command => Some(Movement::DocumentStart),
        Key::ArrowDown if command => Some(Movement::DocumentEnd),
        Key::ArrowLeft => Some(if by_word {
            Movement::WordBackward
        } else {
            Movement::GraphemeBackward
        }),
        Key::ArrowRight => Some(if by_word {
            Movement::WordForward
        } else {
            Movement::GraphemeForward
        }),
        Key::Home => Some(Movement::DocumentStart),
        Key::End => Some(Movement::DocumentEnd),
        _ => None,
    };
    if let Some(movement) = movement {
        cancel_empty_composition(editor);
        record(editor.move_cursor(movement, modifiers.shift), errors);
        return true;
    }
    match key {
        Key::ArrowUp | Key::ArrowDown => {
            cancel_empty_composition(editor);
            if let Some((position, preferred_x, prefer_next_row)) =
                vertical_position(editor, layouts, navigation.as_ref(), key == Key::ArrowDown)
            {
                set_visual_selection(
                    editor,
                    navigation,
                    errors,
                    position,
                    preferred_x,
                    prefer_next_row,
                    modifiers.shift,
                );
            }
            true
        }
        Key::PageUp | Key::PageDown if paging => {
            cancel_empty_composition(editor);
            if let Some((position, preferred_x, prefer_next_row)) = page_position(
                editor,
                layouts,
                navigation.as_ref(),
                key == Key::PageDown,
                options.page_height,
            ) {
                set_visual_selection(
                    editor,
                    navigation,
                    errors,
                    position,
                    preferred_x,
                    prefer_next_row,
                    modifiers.shift,
                );
            }
            true
        }
        Key::Backspace | Key::Delete if options.read_only => {
            *navigation = previous_navigation;
            true
        }
        Key::Enter if options.read_only && !modifiers.command => {
            *navigation = previous_navigation;
            true
        }
        Key::Backspace => {
            cancel_empty_composition(editor);
            record(
                if command && modifiers.mac_cmd {
                    editor.delete_to_paragraph_start()
                } else if by_word {
                    editor.delete_word_backward()
                } else {
                    editor.delete_backward()
                },
                errors,
            );
            true
        }
        Key::Delete => {
            cancel_empty_composition(editor);
            record(
                if by_word {
                    editor.delete_word_forward()
                } else {
                    editor.delete_forward()
                },
                errors,
            );
            true
        }
        Key::Enter if modifiers.shift && !modifiers.command => {
            cancel_empty_composition(editor);
            record(editor.insert_line_break(), errors);
            true
        }
        Key::Enter if !modifiers.command => {
            cancel_empty_composition(editor);
            record(editor.insert_paragraph(), errors);
            true
        }
        Key::Tab
            if !options.read_only
                && !modifiers.command
                && !modifiers.ctrl
                && !modifiers.alt
                && layouts.contains_list(editor.selected_paragraphs()) =>
        {
            cancel_empty_composition(editor);
            record(
                if modifiers.shift {
                    editor.outdent_list()
                } else {
                    editor.indent_list()
                },
                errors,
            );
            true
        }
        _ => {
            // Delegating a key to the host must not move the painted caret or
            // discard the column used by a later vertical movement.
            if reset_navigation {
                *navigation = previous_navigation;
            }
            false
        }
    }
}

fn set_visual_selection(
    editor: &mut Editor,
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    position: Position,
    preferred_x: f32,
    prefer_next_row: bool,
    extend: bool,
) {
    let anchor = if extend {
        editor.selection().anchor
    } else {
        position
    };
    record(
        editor.set_selection(Selection::new(anchor, position)),
        errors,
    );
    *navigation = Some(VerticalNavigation {
        selection: editor.selection(),
        preferred_x,
        prefer_next_row,
    });
}

pub(super) fn delete_surrounding(
    editor: &mut Editor,
    before: usize,
    after: usize,
    errors: &mut Vec<Error>,
) {
    let focus = editor.selection().focus;
    let start = surrounding_position(editor.document(), focus, before, false);
    let end = surrounding_position(editor.document(), focus, after, true);
    if start < end {
        record(editor.replace_range(start..end, ""), errors);
    }
}

pub(super) fn surrounding_position(
    document: &Document,
    focus: Position,
    mut remaining: usize,
    forward: bool,
) -> Position {
    let mut paragraph = focus.paragraph;
    let mut index = document
        .paragraph(paragraph)
        .expect("editor maintains a valid selection")
        .scalar_index(focus.byte)
        .expect("editor maintains a valid selection");
    loop {
        let source = document.paragraph(paragraph).expect("valid paragraph");
        let chars = source.scalar_count();
        let available = if forward { chars - index } else { index };
        if remaining <= available {
            let target = if forward {
                index + remaining
            } else {
                index - remaining
            };
            let byte = source
                .byte_from_scalar(target)
                .unwrap_or(source.text().len());
            let boundary = if forward {
                source.boundary_at_or_after(byte)
            } else {
                source.boundary_at_or_before(byte)
            };
            return Position::new(paragraph, boundary);
        }
        remaining -= available;
        if forward {
            if paragraph + 1 == document.paragraphs().len() {
                return document.end();
            }
            paragraph += 1;
            index = 0;
        } else {
            if paragraph == 0 {
                return Position::default();
            }
            paragraph -= 1;
            index = document
                .paragraph(paragraph)
                .expect("valid paragraph")
                .scalar_count();
        }
        remaining -= 1;
    }
}
