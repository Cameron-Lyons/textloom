//! Ordered event handling and host clipboard coordination.
use super::{
    RichClipboard,
    layout::ParagraphLayouts,
    navigation::{VerticalNavigation, preedit_byte_range, vertical_position},
    record,
};
use crate::{Document, Editor, Error, Movement, ParagraphKind, Position, Selection, StylePatch};
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
        // The transport publishes synchronously; earlier deferred clipboard writes
        // would otherwise run afterward and replace this newer clipboard item.
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

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_event(
    editor: &mut Editor,
    ui: &Ui,
    event: &Event,
    layouts: &ParagraphLayouts,
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    interrupted: &mut bool,
    read_only: bool,
    clipboard: &mut Option<&mut dyn RichClipboard>,
) -> bool {
    if read_only {
        match event {
            Event::Cut => {
                copy_selection(editor, ui, clipboard);
                return true;
            }
            Event::Text(_) | Event::Paste(_) | Event::Ime(_) => return true,
            _ => {}
        }
    }
    if matches!(event, Event::Paste(text) if text.is_empty()) {
        return true;
    }
    if editor
        .composition()
        .is_some_and(|composition| composition.text.is_empty())
        && (matches!(event, Event::Text(_) | Event::Paste(_) | Event::Cut)
            || matches!(event, Event::Key { key, pressed: true, .. } if *key != Key::Escape))
    {
        // Empty preedit can be followed by either Commit or normal typing.
        // Keep the replacement for Commit, then cancel it when typing resumes.
        *interrupted |= editor.cancel_composition();
    }
    match event {
        Event::Copy => {
            copy_selection(editor, ui, clipboard);
            true
        }
        Event::Cut if editor.composition().is_none() => {
            if !editor.selection().is_caret() {
                copy_selection(editor, ui, clipboard);
                editor.break_history_group();
                record(editor.insert_text(""), errors);
                editor.break_history_group();
            }
            true
        }
        Event::Paste(text) if editor.composition().is_none() => {
            let fragment = clipboard
                .as_deref_mut()
                .and_then(|clipboard| clipboard.paste(text));
            editor.break_history_group();
            record(
                match fragment {
                    Some(fragment) => editor.insert_fragment(&fragment),
                    None => editor.insert_text(text),
                },
                errors,
            );
            editor.break_history_group();
            true
        }
        Event::Text(text) if editor.composition().is_none() => {
            if !text.is_empty() && text != "\n" && text != "\r" {
                record(editor.insert_text(text), errors);
            }
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
                record(editor.update_composition(text, range), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Commit(text)) => {
            if !text.is_empty() || editor.composition().is_some() {
                record(editor.commit_composition(text), errors);
            }
            true
        }
        Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars,
            after_chars,
        }) => {
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
            pressed: true,
            modifiers,
            ..
        } => {
            if *key == Key::Escape {
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
            if editor.composition().is_some() {
                return false;
            }
            handle_key(
                editor, *key, *modifiers, layouts, navigation, errors, read_only,
            )
        }
        _ => false,
    }
}

pub(super) fn handle_key(
    editor: &mut Editor,
    key: Key,
    modifiers: Modifiers,
    layouts: &ParagraphLayouts,
    navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    read_only: bool,
) -> bool {
    // Ctrl+Alt may be AltGr, and Command+Control is a host shortcut chord.
    let command = modifiers.command && !modifiers.alt && !(modifiers.mac_cmd && modifiers.ctrl);
    if command || !matches!(key, Key::ArrowUp | Key::ArrowDown) {
        *navigation = None;
    }
    if read_only
        && (matches!(key, Key::Backspace | Key::Delete | Key::Enter)
            || command
                && matches!(
                    key,
                    Key::Z
                        | Key::Y
                        | Key::B
                        | Key::I
                        | Key::U
                        | Key::Num0
                        | Key::Num1
                        | Key::Num2
                        | Key::Num3
                        | Key::Num4
                        | Key::Num5
                        | Key::Num6
                        | Key::Num7
                        | Key::Num8
                ))
    {
        return true;
    }
    if command {
        match key {
            Key::A => {
                editor.select_all();
                return true;
            }
            Key::Z => {
                if modifiers.shift {
                    editor.redo();
                } else {
                    editor.undo();
                }
                return true;
            }
            Key::Y => {
                editor.redo();
                return true;
            }
            Key::B => {
                record(
                    editor.apply_style(StylePatch {
                        bold: Some(editor.selection_style().bold != Some(true)),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::I => {
                record(
                    editor.apply_style(StylePatch {
                        italic: Some(editor.selection_style().italic != Some(true)),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::U => {
                record(
                    editor.apply_style(StylePatch {
                        underline: Some(editor.selection_style().underline != Some(true)),
                        ..Default::default()
                    }),
                    errors,
                );
                return true;
            }
            Key::Num0 => {
                record(editor.set_paragraph_kind(ParagraphKind::Body), errors);
                return true;
            }
            Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 => {
                let level = match key {
                    Key::Num1 => 1,
                    Key::Num2 => 2,
                    Key::Num3 => 3,
                    Key::Num4 => 4,
                    Key::Num5 => 5,
                    _ => 6,
                };
                record(
                    editor.set_paragraph_kind(ParagraphKind::Heading { level }),
                    errors,
                );
                return true;
            }
            Key::Num7 if modifiers.shift => {
                record(
                    editor.set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 0,
                        start: 1,
                    }),
                    errors,
                );
                return true;
            }
            Key::Num8 if modifiers.shift => {
                record(
                    editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }),
                    errors,
                );
                return true;
            }
            _ => {}
        }
    }
    let by_word = modifiers.ctrl || modifiers.alt;
    let movement = match key {
        Key::ArrowLeft if command && modifiers.mac_cmd => Some(Movement::ParagraphStart),
        Key::ArrowRight if command && modifiers.mac_cmd => Some(Movement::ParagraphEnd),
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
        Key::Home => Some(if modifiers.command {
            Movement::DocumentStart
        } else {
            Movement::ParagraphStart
        }),
        Key::End => Some(if modifiers.command {
            Movement::DocumentEnd
        } else {
            Movement::ParagraphEnd
        }),
        _ => None,
    };
    if let Some(movement) = movement {
        record(editor.move_cursor(movement, modifiers.shift), errors);
        return true;
    }
    match key {
        Key::ArrowUp | Key::ArrowDown => {
            let Some((position, preferred_x, prefer_next_row)) =
                vertical_position(editor, layouts, navigation.as_ref(), key == Key::ArrowDown)
            else {
                return true;
            };
            let anchor = if modifiers.shift {
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
            true
        }
        Key::Backspace => {
            record(
                if command && modifiers.mac_cmd {
                    crate::adapter::delete_to_paragraph_start(editor)
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
        Key::Enter if !modifiers.command => {
            record(editor.insert_paragraph(), errors);
            true
        }
        Key::Tab
            if !read_only
                && !modifiers.command
                && !modifiers.ctrl
                && !modifiers.alt
                && selection_contains_list(editor) =>
        {
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
        _ => false,
    }
}

pub(super) fn selection_contains_list(editor: &Editor) -> bool {
    editor.document().paragraphs()[selected_paragraphs(editor)]
        .iter()
        .any(|paragraph| {
            matches!(
                paragraph.kind(),
                ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. }
            )
        })
}

pub(super) fn selected_paragraphs(editor: &Editor) -> std::ops::Range<usize> {
    let range = editor.selection().range();
    let end = if range.end.paragraph > range.start.paragraph && range.end.byte == 0 {
        range.end.paragraph
    } else {
        range.end.paragraph + 1
    };
    range.start.paragraph..end
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
        // Only visit paragraphs crossed by the requested native scalar count.
        // Each paragraph separator contributes one scalar, including empty rows.
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
