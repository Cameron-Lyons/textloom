use super::layout::ParagraphLayouts;
use super::navigation::{self, NavigationTarget, VerticalNavigation};
use super::{CommitEcho, RichClipboard};
use crate::adapter::{self, Shortcut};
use crate::{Document, Editor, Error, Movement, ParagraphKind, Position, Selection, SelectionMode};
use egui::os::OperatingSystem;
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

pub(super) fn reads_layout(os: OperatingSystem, event: &Event, options: &InputOptions) -> bool {
    match event {
        Event::Key {
            key,
            physical_key,
            pressed: true,
            modifiers,
            ..
        } => matches!(
            classify_key(os, *key, *physical_key, *modifiers, options),
            Some(
                KeyCommand::RowBoundary { .. }
                    | KeyCommand::DeleteRowStart
                    | KeyCommand::Vertical { .. }
                    | KeyCommand::Page { .. }
                    | KeyCommand::ListEdit(_)
            )
        ),
        Event::AccessKitActionRequest(_) => true,
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_event(
    editor: &mut Editor,
    ui: &Ui,
    event: &Event,
    event_modifiers: Modifiers,
    layouts: &ParagraphLayouts,
    vertical_navigation: &mut Option<VerticalNavigation>,
    commit_echo: &mut Option<CommitEcho>,
    errors: &mut Vec<Error>,
    interrupted: &mut bool,
    options: &InputOptions,
    clipboard: &mut Option<&mut dyn RichClipboard>,
) -> bool {
    if is_commit_echo(commit_echo, event, editor) {
        return true;
    }
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
        Event::Text(text) => Some((text, adapter::clean_text(text, false))),
        Event::Paste(text) => Some((text, adapter::clean_text(text, true))),
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
                super::record(
                    editor.isolated_edit(|editor| editor.insert_text("")),
                    errors,
                );
            }
            true
        }
        Event::Paste(_) => {
            if let Some((text, clean)) = &input_text {
                *commit_echo = None;
                let plain = event_modifiers.command && event_modifiers.shift;
                let fragment = clipboard
                    .as_deref_mut()
                    .filter(|_| !plain)
                    .and_then(|clipboard| clipboard.paste(text));
                super::record(
                    editor.isolated_edit(|editor| match fragment {
                        Some(fragment) => editor.insert_fragment(&fragment),
                        None => editor.insert_text(clean),
                    }),
                    errors,
                );
            }
            true
        }
        Event::Text(_) => {
            if let Some((_, clean)) = &input_text {
                *commit_echo = None;
                super::record(editor.insert_text(clean), errors);
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
                .map(|range| navigation::preedit_byte_range(text, range))
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
                super::record(editor.update_composition(text, range), errors);
            }
            true
        }
        Event::Ime(ImeEvent::Commit(text)) => {
            let clean = adapter::clean_text(text, true);
            if !clean.is_empty() || editor.composition().is_some() {
                *commit_echo = None;
            }
            if editor.composition().is_none() {
                if !clean.is_empty() {
                    super::record(editor.insert_text(&clean), errors);
                }
            } else if clean.is_empty() && !text.is_empty() {
                editor.cancel_composition();
            } else {
                super::record(editor.commit_composition(&clean), errors);
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
                ui.os(),
                *key,
                *physical_key,
                *modifiers,
                layouts,
                vertical_navigation,
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

fn is_commit_echo(echo: &mut Option<CommitEcho>, event: &Event, editor: &Editor) -> bool {
    let Some(pending) = echo else {
        return false;
    };
    match event {
        Event::Ime(ImeEvent::Commit(text))
            if !matches!(text.as_str(), "\n" | "\r" | "\r\n")
                && editor.composition().is_none()
                && adapter::clean_text(text, true) == pending.text =>
        {
            *echo = None;
            true
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
    let name = adapter::shortcut_key_name(key_character(key), physical)?;
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

#[derive(Clone, Copy)]
enum KeyCommand {
    Shortcut(Shortcut),
    ReadOnlyNoop,
    Move(Movement),
    ParagraphBoundary { end: bool },
    RowBoundary { end: bool },
    DeleteRowStart,
    Vertical { down: bool },
    Page { down: bool },
    Edit(fn(&mut Editor) -> Result<(), Error>),
    ListEdit(fn(&mut Editor) -> Result<(), Error>),
}

fn classify_key(
    os: OperatingSystem,
    mut key: Key,
    physical_key: Option<Key>,
    mut modifiers: Modifiers,
    options: &InputOptions,
) -> Option<KeyCommand> {
    if os == OperatingSystem::Mac
        && modifiers.ctrl
        && !modifiers.command
        && !modifiers.mac_cmd
        && !modifiers.alt
    {
        let alias = match key {
            Key::A => return Some(KeyCommand::Move(Movement::ParagraphStart)),
            Key::E => return Some(KeyCommand::Move(Movement::ParagraphEnd)),
            Key::B => Some(Key::ArrowLeft),
            Key::F => Some(Key::ArrowRight),
            Key::P => Some(Key::ArrowUp),
            Key::N => Some(Key::ArrowDown),
            Key::H => Some(Key::Backspace),
            Key::D => Some(Key::Delete),
            _ => None,
        };
        if let Some(alias) = alias {
            key = alias;
            // A Control character alias moves/deletes one grapheme, not a word.
            modifiers.ctrl = false;
        }
    }
    let command = modifiers.command && !modifiers.alt && !(modifiers.mac_cmd && modifiers.ctrl);
    if command
        && let Some(shortcut) = resolve_shortcut(
            key,
            physical_key,
            modifiers.shift,
            options.heading_shortcuts,
        )
    {
        return Some(KeyCommand::Shortcut(shortcut));
    }
    let by_word = modifiers.alt || modifiers.ctrl && os != OperatingSystem::Mac;
    let row_command = command && modifiers.mac_cmd;
    let paging = !modifiers.command && !modifiers.alt;
    let paragraph_command = os == OperatingSystem::Mac
        && modifiers.alt
        && !modifiers.ctrl
        && !modifiers.command
        && !modifiers.mac_cmd;
    Some(match key {
        Key::ArrowLeft | Key::ArrowRight
            if os == OperatingSystem::Mac
                && modifiers.ctrl
                && !modifiers.alt
                && !modifiers.command
                && !modifiers.mac_cmd =>
        {
            KeyCommand::RowBoundary {
                end: key == Key::ArrowRight,
            }
        }
        Key::ArrowLeft | Key::ArrowRight if row_command => KeyCommand::RowBoundary {
            end: key == Key::ArrowRight,
        },
        Key::Home | Key::End if !modifiers.command => KeyCommand::RowBoundary {
            end: key == Key::End,
        },
        Key::ArrowUp if command => KeyCommand::Move(Movement::DocumentStart),
        Key::ArrowDown if command => KeyCommand::Move(Movement::DocumentEnd),
        Key::ArrowUp | Key::ArrowDown if paragraph_command => KeyCommand::ParagraphBoundary {
            end: key == Key::ArrowDown,
        },
        Key::Home => KeyCommand::Move(Movement::DocumentStart),
        Key::End => KeyCommand::Move(Movement::DocumentEnd),
        Key::ArrowLeft if by_word => KeyCommand::Move(Movement::WordBackward),
        Key::ArrowLeft => KeyCommand::Move(Movement::GraphemeBackward),
        Key::ArrowRight if by_word => KeyCommand::Move(Movement::WordForward),
        Key::ArrowRight => KeyCommand::Move(Movement::GraphemeForward),
        Key::ArrowUp | Key::ArrowDown => KeyCommand::Vertical {
            down: key == Key::ArrowDown,
        },
        Key::PageUp | Key::PageDown if paging => KeyCommand::Page {
            down: key == Key::PageDown,
        },
        Key::Backspace | Key::Delete if options.read_only => KeyCommand::ReadOnlyNoop,
        Key::Enter if options.read_only && !modifiers.command => KeyCommand::ReadOnlyNoop,
        Key::Backspace if row_command => KeyCommand::DeleteRowStart,
        Key::Backspace if by_word => KeyCommand::Edit(Editor::delete_word_backward),
        Key::Backspace => KeyCommand::Edit(Editor::delete_backward),
        Key::Delete if by_word => KeyCommand::Edit(Editor::delete_word_forward),
        Key::Delete => KeyCommand::Edit(Editor::delete_forward),
        Key::Enter if modifiers.shift && !modifiers.command => {
            KeyCommand::Edit(Editor::insert_line_break)
        }
        Key::Enter if !modifiers.command => KeyCommand::Edit(Editor::insert_paragraph),
        Key::Tab
            if !options.read_only && !modifiers.command && !modifiers.ctrl && !modifiers.alt =>
        {
            KeyCommand::ListEdit(if modifiers.shift {
                Editor::outdent_list
            } else {
                Editor::indent_list
            })
        }
        _ => return None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn handle_key(
    editor: &mut Editor,
    os: OperatingSystem,
    key: Key,
    physical_key: Option<Key>,
    modifiers: Modifiers,
    layouts: &ParagraphLayouts,
    vertical_navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    options: &InputOptions,
) -> bool {
    // Keys delegated to the host and blocked edits keep the painted caret and
    // the column used by a later vertical movement.
    let Some(key_command) =
        classify_key(os, key, physical_key, modifiers, options).filter(|command| {
            !matches!(command, KeyCommand::ListEdit(_))
                || layouts.contains_list(editor.selected_paragraphs())
        })
    else {
        return false;
    };
    match key_command {
        KeyCommand::ReadOnlyNoop => return true,
        KeyCommand::Shortcut(shortcut) => {
            if !options.read_only || !shortcut.edits() {
                let revision = editor.document().revision();
                let selection = editor.selection();
                cancel_empty_composition(editor);
                super::record(shortcut.apply(editor), errors);
                // Changing only the pending typing style must preserve the visual caret.
                if editor.document().revision() != revision || editor.selection() != selection {
                    *vertical_navigation = None;
                }
            }
            return true;
        }
        _ => {}
    }
    let prefer_next_row = vertical_navigation
        .as_ref()
        .is_none_or(|navigation| navigation.prefer_next_row);
    if !matches!(
        key_command,
        KeyCommand::Vertical { .. } | KeyCommand::Page { .. }
    ) {
        *vertical_navigation = None;
    }
    cancel_empty_composition(editor);
    let selection_mode = if modifiers.shift {
        SelectionMode::Extend
    } else {
        SelectionMode::Move
    };
    let target = match key_command {
        KeyCommand::ParagraphBoundary { end } => {
            let position = navigation::paragraph_boundary_position(editor, end);
            let anchor = if modifiers.shift {
                editor.selection().anchor
            } else {
                position
            };
            super::record(
                editor.set_selection(Selection::new(anchor, position)),
                errors,
            );
            None
        }
        KeyCommand::RowBoundary { end } => {
            let target = navigation::row_boundary_position(editor, layouts, prefer_next_row, end);
            if target.is_none() {
                let movement = if end {
                    Movement::ParagraphEnd
                } else {
                    Movement::ParagraphStart
                };
                super::record(editor.move_cursor(movement, selection_mode), errors);
            }
            target
        }
        KeyCommand::Vertical { down } => {
            navigation::vertical_position(editor, layouts, vertical_navigation.as_ref(), down)
        }
        KeyCommand::Page { down } => navigation::page_position(
            editor,
            layouts,
            vertical_navigation.as_ref(),
            down,
            options.page_height,
        ),
        KeyCommand::Move(movement) => {
            super::record(editor.move_cursor(movement, selection_mode), errors);
            None
        }
        KeyCommand::Edit(edit) | KeyCommand::ListEdit(edit) => {
            super::record(edit(editor), errors);
            None
        }
        KeyCommand::DeleteRowStart => {
            if editor.selection().is_caret() {
                let focus = editor.selection().focus;
                let start =
                    navigation::row_boundary_position(editor, layouts, prefer_next_row, false)
                        .map_or(Position::new(focus.paragraph, 0), |target| target.position);
                if start < focus {
                    super::record(editor.replace_range(start..focus, ""), errors);
                }
            } else {
                super::record(editor.delete_backward(), errors);
            }
            None
        }
        KeyCommand::Shortcut(_) | KeyCommand::ReadOnlyNoop => None,
    };
    if let Some(target) = target {
        set_visual_selection(editor, vertical_navigation, errors, target, modifiers.shift);
    }
    true
}

fn set_visual_selection(
    editor: &mut Editor,
    vertical_navigation: &mut Option<VerticalNavigation>,
    errors: &mut Vec<Error>,
    target: NavigationTarget,
    extend: bool,
) {
    let NavigationTarget {
        position,
        preferred_x,
        prefer_next_row,
    } = target;
    let anchor = if extend {
        editor.selection().anchor
    } else {
        position
    };
    super::record(
        editor.set_selection(Selection::new(anchor, position)),
        errors,
    );
    *vertical_navigation = Some(VerticalNavigation {
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
        super::record(editor.replace_range(start..end, ""), errors);
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
        let source = document
            .paragraph(paragraph)
            .expect("the walk stays between the first and last paragraphs");
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
                .expect("a paragraph precedes every paragraph but the first")
                .scalar_count();
        }
        remaining -= 1;
    }
}
