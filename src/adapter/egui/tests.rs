use super::*;

fn frame(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    focus: bool,
) -> egui::FullOutput {
    frame_options(context, editor, events, focus, false)
}

fn frame_options(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    focus: bool,
    read_only: bool,
) -> egui::FullOutput {
    clipboard_frame(context, editor, events, focus, read_only, true, None)
}

fn clipboard_frame(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    focus: bool,
    read_only: bool,
    enabled: bool,
    mut clipboard: Option<&mut dyn RichClipboard>,
) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
        events,
        ..Default::default()
    };
    let mut output = context.run_ui(input, |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            let id = ui.make_persistent_id("editor");
            if focus {
                ui.memory_mut(|memory| {
                    if !memory.has_focus(id) {
                        memory.request_focus(id);
                    }
                });
            }
            ui.add_enabled_ui(enabled, |ui| {
                let mut widget = RichTextEditor::new(editor).id(id).read_only(read_only);
                if let Some(clipboard) = clipboard.as_deref_mut() {
                    widget = widget.rich_clipboard(clipboard);
                }
                let output = widget.show(ui);
                assert!(output.errors.is_empty(), "{:?}", output.errors);
                assert!(
                    output.accessibility_errors.is_empty(),
                    "{:?}",
                    output.accessibility_errors
                );
            });
        });
    });
    output.textures_delta.clear();
    output
}

#[derive(Debug, PartialEq, Eq)]
enum ClipboardCall {
    Copy(Fragment),
    Paste(String),
}

#[derive(Default)]
struct TestClipboard {
    calls: Vec<ClipboardCall>,
    accept_copy: bool,
    fragment: Option<Fragment>,
}

impl RichClipboard for TestClipboard {
    fn copy(&mut self, fragment: &Fragment) -> bool {
        self.calls.push(ClipboardCall::Copy(fragment.clone()));
        self.accept_copy
    }

    fn paste(&mut self, plain_text: &str) -> Option<Fragment> {
        self.calls.push(ClipboardCall::Paste(plain_text.into()));
        self.fragment.clone()
    }
}

fn styled_clipboard_editor() -> Editor {
    let mut editor = Editor::from_text("café 日本語👩🏽‍💻\nsecond");
    editor.select_all();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            italic: Some(true),
            foreground: Some(Some(crate::Color([42, 100, 200, 255]))),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: Some(4),
        })
        .unwrap();
    editor
        .set_selection(Selection::new(editor.document().end(), Position::default()))
        .unwrap();
    editor.clear_history();
    editor
}

#[test]
fn rich_copy_and_cut_capture_styles_before_deletion_and_preserve_undo() {
    for accept_copy in [true, false] {
        let context = egui::Context::default();
        let mut editor = styled_clipboard_editor();
        let fragment = editor.selected_fragment();
        let selection = editor.selection();
        let mut clipboard = TestClipboard {
            accept_copy,
            ..Default::default()
        };
        let output = clipboard_frame(
            &context,
            &mut editor,
            vec![Event::Copy, Event::Cut],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        assert_eq!(
            clipboard.calls,
            vec![
                ClipboardCall::Copy(fragment.clone()),
                ClipboardCall::Copy(fragment.clone()),
            ]
        );
        let copied: Vec<_> = output
            .platform_output
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::OutputCommand::CopyText(text) => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if accept_copy {
            assert!(copied.is_empty());
        } else {
            let text = fragment.plain_text();
            assert_eq!(copied, vec![text.as_str(), text.as_str()]);
        }
        assert_eq!(editor.document().plain_text(), "");
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(Fragment::from_document(editor.document()), fragment);
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());

        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        clipboard.calls.clear();
        let output = clipboard_frame(
            &context,
            &mut editor,
            vec![Event::Copy, Event::Cut],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        assert!(clipboard.calls.is_empty());
        assert!(output.platform_output.commands.is_empty());
        assert!(!editor.can_undo());
    }
}

#[test]
fn rich_copy_supersedes_earlier_plain_and_image_commands_with_later_plain_fallback() {
    struct AlternatingClipboard {
        outcomes: Vec<bool>,
        copied: Vec<String>,
    }
    impl RichClipboard for AlternatingClipboard {
        fn copy(&mut self, fragment: &Fragment) -> bool {
            self.copied.push(fragment.plain_text());
            self.outcomes[self.copied.len() - 1]
        }
    }

    for later_plain_fallback in [false, true] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("first");
        editor.select_all();
        let mut clipboard = AlternatingClipboard {
            outcomes: if later_plain_fallback {
                vec![false, true, false]
            } else {
                vec![false, true]
            },
            copied: Vec::new(),
        };
        let mut events = vec![
            Event::Copy,
            Event::Text("second".into()),
            key(Key::A, Modifiers::COMMAND),
            Event::Copy,
        ];
        if later_plain_fallback {
            events.extend([
                Event::Text("third".into()),
                key(Key::A, Modifiers::COMMAND),
                Event::Copy,
            ]);
        }
        let open_url = egui::OpenUrl::new_tab("https://example.invalid/clipboard-regression");
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                events,
                ..Default::default()
            },
            |ui| {
                ui.ctx()
                    .copy_image(egui::ColorImage::filled([1, 1], Color32::WHITE));
                ui.ctx().open_url(open_url.clone());
                egui::CentralPanel::default().show(ui, |ui| {
                    let id = ui.make_persistent_id("editor");
                    ui.memory_mut(|memory| memory.request_focus(id));
                    let response = RichTextEditor::new(&mut editor)
                        .id(id)
                        .rich_clipboard(&mut clipboard)
                        .show(ui);
                    assert!(response.errors.is_empty());
                    assert!(response.accessibility_errors.is_empty());
                });
            },
        );
        output.textures_delta.clear();
        let mut expected_commands = vec![egui::OutputCommand::OpenUrl(open_url)];
        let mut expected_copies = vec!["first", "second"];
        if later_plain_fallback {
            expected_commands.push(egui::OutputCommand::CopyText("third".into()));
            expected_copies.push("third");
        }
        assert_eq!(clipboard.copied, expected_copies);
        assert_eq!(output.platform_output.commands, expected_commands);
    }
}

#[test]
fn rich_paste_replaces_selection_in_one_undo_step_with_plain_fallback() {
    let fragment = styled_clipboard_editor().selected_fragment();
    for payload in [Some(fragment.clone()), None] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        editor.select_all();
        let selection = editor.selection();
        let mut clipboard = TestClipboard {
            fragment: payload.clone(),
            ..Default::default()
        };
        clipboard_frame(
            &context,
            &mut editor,
            vec![Event::Paste(fragment.plain_text())],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        assert_eq!(
            clipboard.calls,
            vec![ClipboardCall::Paste(fragment.plain_text())]
        );
        assert_eq!(editor.document().plain_text(), fragment.plain_text());
        let expected = payload.unwrap_or_else(|| Fragment::from_text(&fragment.plain_text()));
        assert_eq!(Fragment::from_document(editor.document()), expected);
        assert_eq!(
            editor.selection(),
            Selection::caret(editor.document().end())
        );
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());
        assert!(editor.redo());
        assert_eq!(Fragment::from_document(editor.document()), expected);
    }
}

#[test]
fn shifted_command_paste_pastes_plain_text_without_the_rich_clipboard() {
    let fragment = styled_clipboard_editor().selected_fragment();
    let text = fragment.plain_text();
    for (modifiers, rich) in [
        (Modifiers::CTRL | Modifiers::COMMAND, true),
        (Modifiers::SHIFT, true),
        (
            Modifiers::CTRL | Modifiers::COMMAND | Modifiers::SHIFT,
            false,
        ),
        (
            Modifiers::MAC_CMD | Modifiers::COMMAND | Modifiers::SHIFT,
            false,
        ),
        (
            Modifiers::MAC_CMD | Modifiers::COMMAND | Modifiers::SHIFT | Modifiers::ALT,
            false,
        ),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("original");
        editor.select_all();
        let mut clipboard = TestClipboard {
            fragment: Some(fragment.clone()),
            ..Default::default()
        };
        clipboard_frame(
            &context,
            &mut editor,
            vec![
                Event::ModifiersChanged(modifiers),
                Event::Paste(text.clone()),
            ],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        let (calls, expected) = if rich {
            (vec![ClipboardCall::Paste(text.clone())], fragment.clone())
        } else {
            (Vec::new(), Fragment::from_text(&text))
        };
        assert_eq!(clipboard.calls, calls, "{modifiers:?}");
        assert_eq!(Fragment::from_document(editor.document()), expected);
        assert_eq!(
            context.input(|input| input.events.clone()),
            vec![Event::ModifiersChanged(modifiers)]
        );
        assert_eq!(editor.undo_len(), 1);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
    }
}

#[test]
fn rich_clipboard_callbacks_follow_input_order_and_separate_typing_history() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    editor.select_all();
    let fragment = styled_clipboard_editor().selected_fragment();
    let initial = editor.selected_fragment();
    let mut clipboard = TestClipboard {
        accept_copy: true,
        fragment: Some(fragment.clone()),
        ..Default::default()
    };
    let unrelated = key(Key::F1, Modifiers::NONE);
    clipboard_frame(
        &context,
        &mut editor,
        vec![
            Event::Copy,
            Event::Text("before".into()),
            Event::Paste(fragment.plain_text()),
            unrelated.clone(),
            Event::Text("after".into()),
            key(Key::A, Modifiers::COMMAND),
            Event::Copy,
        ],
        true,
        false,
        true,
        Some(&mut clipboard),
    );
    assert_eq!(
        editor.document().plain_text(),
        format!("before{}after", fragment.plain_text())
    );
    let final_fragment = editor.selected_fragment();
    assert_eq!(
        clipboard.calls,
        vec![
            ClipboardCall::Copy(initial),
            ClipboardCall::Paste(fragment.plain_text()),
            ClipboardCall::Copy(final_fragment),
        ]
    );
    assert_eq!(context.input(|input| input.events.clone()), vec![unrelated]);
    assert_eq!(editor.undo_len(), 3);
    assert!(editor.undo());
    assert_eq!(
        editor.document().plain_text(),
        format!("before{}", fragment.plain_text())
    );
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "before");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "original");
}

#[test]
fn rich_clipboard_read_only_copies_without_paste_or_cut_mutation() {
    let context = egui::Context::default();
    let mut editor = styled_clipboard_editor();
    let fragment = editor.selected_fragment();
    let selection = editor.selection();
    let revision = editor.document().revision();
    let mut clipboard = TestClipboard {
        accept_copy: true,
        fragment: Some(Fragment::from_text("replacement")),
        ..Default::default()
    };
    let output = clipboard_frame(
        &context,
        &mut editor,
        vec![Event::Cut, Event::Paste("replacement".into()), Event::Copy],
        true,
        true,
        true,
        Some(&mut clipboard),
    );
    assert_eq!(
        clipboard.calls,
        vec![
            ClipboardCall::Copy(fragment.clone()),
            ClipboardCall::Copy(fragment.clone()),
        ]
    );
    assert!(output.platform_output.commands.is_empty());
    assert_eq!(Fragment::from_document(editor.document()), fragment);
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.selection(), selection);
    assert!(!editor.can_undo());
}

#[test]
fn rich_clipboard_ignores_empty_paste_and_active_preedit() {
    for preedit in ["", "候"] {
        let context = egui::Context::default();
        let mut editor = styled_clipboard_editor();
        let fragment = editor.selected_fragment();
        editor.update_composition(preedit, None).unwrap();
        let composition = editor.composition().cloned();
        let mut clipboard = TestClipboard {
            accept_copy: true,
            fragment: Some(Fragment::from_text("replacement")),
            ..Default::default()
        };
        clipboard_frame(
            &context,
            &mut editor,
            vec![Event::Paste(String::new()), Event::Copy],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        assert_eq!(clipboard.calls, vec![ClipboardCall::Copy(fragment.clone())]);
        assert_eq!(editor.composition(), composition.as_ref());
        assert_eq!(Fragment::from_document(editor.document()), fragment);
        assert!(!editor.can_undo());

        clipboard.calls.clear();
        clipboard_frame(
            &context,
            &mut editor,
            vec![Event::Paste("replacement".into())],
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        if preedit.is_empty() {
            assert_eq!(
                clipboard.calls,
                vec![ClipboardCall::Paste("replacement".into())]
            );
            assert!(editor.composition().is_none());
            assert_eq!(editor.document().plain_text(), "replacement");
            assert_eq!(editor.undo_len(), 1);
            assert!(editor.undo());
            assert_eq!(Fragment::from_document(editor.document()), fragment);
        } else {
            assert!(clipboard.calls.is_empty());
            assert_eq!(editor.composition(), composition.as_ref());
            assert_eq!(Fragment::from_document(editor.document()), fragment);
            assert!(!editor.can_undo());
            clipboard_frame(
                &context,
                &mut editor,
                vec![Event::Cut],
                true,
                false,
                true,
                Some(&mut clipboard),
            );
            assert!(clipboard.calls.is_empty());
            assert_eq!(editor.composition(), composition.as_ref());
            assert!(!editor.can_undo());
        }
    }
}

#[test]
fn caret_cut_preserves_an_empty_preedit_for_its_normal_commit() {
    for separate_frame in [false, true] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        editor
            .set_selection(Selection::caret(editor.document().end()))
            .unwrap();
        clipboard_frame(
            &context,
            &mut editor,
            vec![
                Event::Ime(ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: None,
                }),
                Event::Ime(ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }),
            ],
            true,
            false,
            true,
            None,
        );
        let mut clipboard = TestClipboard {
            accept_copy: true,
            ..Default::default()
        };
        let mut events = vec![Event::Cut];
        if separate_frame {
            let output = clipboard_frame(
                &context,
                &mut editor,
                std::mem::take(&mut events),
                true,
                false,
                true,
                Some(&mut clipboard),
            );
            assert!(editor.composition().is_some());
            assert_eq!(editor.document().plain_text(), "abc");
            assert!(!editor.can_undo());
            assert!(output.platform_output.commands.is_empty());
            assert!(
                !output
                    .platform_output
                    .ime
                    .unwrap()
                    .should_interrupt_composition
            );
        }
        events.push(Event::Ime(ImeEvent::Commit("한".into())));
        let output = clipboard_frame(
            &context,
            &mut editor,
            events,
            true,
            false,
            true,
            Some(&mut clipboard),
        );
        assert_eq!(editor.document().plain_text(), "abc한");
        assert!(editor.composition().is_none());
        assert!(clipboard.calls.is_empty());
        assert!(output.platform_output.commands.is_empty());
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "abc");
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 3)));
        assert!(!editor.can_undo());
    }
}

#[test]
fn rich_clipboard_does_not_access_host_when_disabled_or_unfocused() {
    for (enabled, focused) in [(false, true), (true, false)] {
        let context = egui::Context::default();
        let mut editor = styled_clipboard_editor();
        let fragment = editor.selected_fragment();
        let mut clipboard = TestClipboard {
            accept_copy: true,
            fragment: Some(Fragment::from_text("replacement")),
            ..Default::default()
        };
        let events = vec![Event::Copy, Event::Cut, Event::Paste("replacement".into())];
        let output = clipboard_frame(
            &context,
            &mut editor,
            events.clone(),
            focused,
            false,
            enabled,
            Some(&mut clipboard),
        );
        assert!(clipboard.calls.is_empty());
        assert!(output.platform_output.commands.is_empty());
        assert_eq!(context.input(|input| input.events.clone()), events);
        assert_eq!(Fragment::from_document(editor.document()), fragment);
        assert!(!editor.can_undo());
    }
}

#[test]
fn default_rich_clipboard_methods_keep_plain_text_interchange() {
    struct DefaultClipboard;
    impl RichClipboard for DefaultClipboard {}
    let context = egui::Context::default();
    let mut editor = styled_clipboard_editor();
    let text = editor.selected_text();
    let output = clipboard_frame(
        &context,
        &mut editor,
        vec![Event::Copy, Event::Paste("plain".into())],
        true,
        false,
        true,
        Some(&mut DefaultClipboard),
    );
    assert!(output.platform_output.commands.iter().any(|command| {
        matches!(command, egui::OutputCommand::CopyText(copied) if copied == &text)
    }));
    assert_eq!(editor.document().plain_text(), "plain");
    assert_eq!(editor.undo_len(), 1);
}

#[test]
fn rich_clipboard_preserves_accessibility_selection_replacement_and_keyboard_undo() {
    use egui::accesskit::{Action, ActionData, ActionRequest, TextSelection, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = styled_clipboard_editor();
    let original = Fragment::from_document(editor.document());
    frame(&context, &mut editor, vec![], true);
    let id = context.memory(|memory| memory.focused()).unwrap();
    let snapshot = context
        .data(|data| data.get_temp::<Cache>(id))
        .unwrap()
        .accessibility
        .unwrap();
    let selected = Selection::new(Position::new(0, 6), Position::new(0, 12));
    let fragment = editor.document().fragment(selected.range()).unwrap();
    assert_eq!(fragment.plain_text(), "日本");
    let select = Event::AccessKitActionRequest(ActionRequest {
        action: Action::SetTextSelection,
        target_node: id.accesskit_id(),
        target_tree: TreeId::ROOT,
        data: Some(ActionData::SetTextSelection(TextSelection {
            anchor: snapshot.position(selected.anchor).unwrap(),
            focus: snapshot.position(selected.focus).unwrap(),
        })),
    });
    let replace = Event::AccessKitActionRequest(ActionRequest {
        action: Action::ReplaceSelectedText,
        target_node: id.accesskit_id(),
        target_tree: TreeId::ROOT,
        data: Some(ActionData::Value("replacement".into())),
    });
    let mut clipboard = TestClipboard {
        accept_copy: true,
        ..Default::default()
    };
    let output = clipboard_frame(
        &context,
        &mut editor,
        vec![
            select,
            Event::Copy,
            replace,
            key(Key::Z, Modifiers::COMMAND),
            Event::Copy,
        ],
        true,
        false,
        true,
        Some(&mut clipboard),
    );
    assert_eq!(
        clipboard.calls,
        vec![
            ClipboardCall::Copy(fragment.clone()),
            ClipboardCall::Copy(fragment)
        ]
    );
    assert_eq!(Fragment::from_document(editor.document()), original);
    assert_eq!(editor.selection(), selected);
    assert!(!editor.can_undo());
    assert!(editor.can_redo());
    assert!(output.platform_output.commands.is_empty());
    assert!(output.platform_output.accesskit_update.is_some());
    assert!(context.input(|input| input.events.is_empty()));
}

fn input_options(read_only: bool) -> InputOptions {
    InputOptions {
        read_only,
        page_height: 100.0,
        heading_shortcuts: true,
    }
}

fn key(key: Key, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn widget_window_and_escape_blur_separate_typing_undo_groups() {
    for blur in 0..3 {
        let context = egui::Context::default();
        let mut editor = Editor::default();
        frame(
            &context,
            &mut editor,
            vec![Event::Text("a".into()), Event::Text("b".into())],
            true,
        );
        assert_eq!(editor.undo_len(), 1);
        let id = context.memory(|memory| memory.focused()).unwrap();
        if blur == 0 {
            context.memory_mut(|memory| memory.surrender_focus(id));
        }
        let mut events = vec![Event::Text("ignored".into())];
        if blur == 2 {
            events.insert(0, key(Key::Escape, Modifiers::NONE));
        }
        context
            .run_ui(
                egui::RawInput {
                    focused: blur != 1,
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let output = RichTextEditor::new(&mut editor).id(id).show(ui);
                        assert!(!output.response.has_focus());
                        assert!(output.errors.is_empty());
                        assert!(output.accessibility_errors.is_empty());
                    });
                },
            )
            .drop_without_applying_deltas();
        assert_eq!(editor.document().plain_text(), "ab");
        frame(
            &context,
            &mut editor,
            vec![Event::Text("c".into()), Event::Text("d".into())],
            true,
        );
        assert_eq!(editor.document().plain_text(), "abcd");
        assert_eq!(editor.undo_len(), 2);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "ab");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "");
    }
}

fn scrolling_frame(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    focus: bool,
    preceding_space: f32,
) -> (egui::FullOutput, Vec2, Rect, bool) {
    let mut offset = Vec2::ZERO;
    let mut viewport = Rect::NOTHING;
    let mut visible = false;
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
            events,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let scroll = egui::ScrollArea::vertical()
                    .id_salt("caret scroll")
                    .animated(false)
                    .max_height(100.0)
                    .show(ui, |ui| {
                        ui.add_space(preceding_space);
                        let id = Id::new("scroll editor");
                        if focus {
                            ui.memory_mut(|memory| {
                                if !memory.has_focus(id) {
                                    memory.request_focus(id);
                                }
                            });
                        }
                        let output = RichTextEditor::new(editor).id(id).min_rows(1).show(ui);
                        assert!(output.errors.is_empty());
                        assert!(output.accessibility_errors.is_empty());
                        visible = ui.is_rect_visible(output.response.rect);
                    });
                offset = scroll.state.offset;
                viewport = scroll.inner_rect;
            });
        },
    );
    output.textures_delta.clear();
    (output, offset, viewport, visible)
}

fn appearance() -> Appearance {
    Appearance {
        font: FontId::proportional(16.0),
        bold_family: None,
        color: Color32::WHITE,
        strong_color: Color32::WHITE,
        code_background: Color32::BLACK,
        link_color: Color32::LIGHT_BLUE,
        width: 400.0,
    }
}

fn cached_layout_frame(
    context: &egui::Context,
    cache: &mut Cache,
    editor: &Editor,
    appearance: &Appearance,
) -> ParagraphLayouts {
    let mut layouts = ParagraphLayouts::default();
    context
        .run_ui(egui::RawInput::default(), |ui| {
            layouts = cache.layout(ui, editor, appearance);
        })
        .drop_without_applying_deltas();
    layouts
}

fn cached_preview_frame(
    context: &egui::Context,
    cache: &mut Cache,
    editor: &Editor,
    appearance: &Appearance,
) -> ParagraphLayouts {
    let mut layouts = ParagraphLayouts::default();
    context
        .run_ui(egui::RawInput::default(), |ui| {
            cache.layout(ui, editor, appearance);
            layouts = cache
                .layout_preview(ui, editor, appearance)
                .unwrap()
                .expect("active nonempty preedit");
        })
        .drop_without_applying_deltas();
    layouts
}

#[test]
fn successive_preedits_reuse_unchanged_galleys() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text("first\nOLD\nlast");
    let appearance = appearance();
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(1, 3)))
        .unwrap();
    editor.update_composition("あ", None).unwrap();
    let first = cached_preview_frame(&context, &mut cache, &editor, &appearance);

    editor.update_composition("あい", None).unwrap();
    let second = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(Arc::ptr_eq(
        &first.get(0).unwrap().galley,
        &second.get(0).unwrap().galley
    ));
    assert!(!Arc::ptr_eq(
        &first.get(1).unwrap().galley,
        &second.get(1).unwrap().galley
    ));
    assert!(Arc::ptr_eq(
        &first.get(2).unwrap().galley,
        &second.get(2).unwrap().galley
    ));
    assert_eq!(second.get(1).unwrap().galley.job.text, "あい");

    assert_eq!(
        editor.update_composition("invalid", Some(9..9)),
        Err(Error::InvalidCompositionSelection)
    );
    let unchanged = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(second.identity().ptr_eq(&unchanged.identity()));

    editor.update_composition("あい\nう", None).unwrap();
    let multiline = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert_eq!(multiline.len(), 4);
    assert!(Arc::ptr_eq(
        &first.get(0).unwrap().galley,
        &multiline.get(0).unwrap().galley
    ));
    assert!(Arc::ptr_eq(
        &first.get(2).unwrap().galley,
        &multiline.get(3).unwrap().galley
    ));
    assert_eq!(editor.document().plain_text(), "first\nOLD\nlast");
    assert!(!editor.can_undo());
}

#[test]
fn preedit_cursor_changes_reuse_document_and_layout_with_unicode_and_newlines() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text("first\nOLD\nlast");
    let appearance = appearance();
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(1, 3)))
        .unwrap();
    let text = "あ\r\n👩‍💻";
    editor.update_composition(text, Some(0..3)).unwrap();
    let first = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    let identity = cache.preview.as_ref().unwrap().document.content_identity();
    assert_eq!(cache.preview.as_ref().unwrap().caret, Position::new(1, 3));
    let snapshot = editor.composition_snapshot().unwrap();
    assert!(Arc::ptr_eq(
        &snapshot,
        &cache.preview.as_ref().unwrap().composition
    ));
    editor.update_composition(text, Some(0..3)).unwrap();
    let identical = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(first.identity().ptr_eq(&identical.identity()));
    assert!(Arc::ptr_eq(
        &snapshot,
        &editor.composition_snapshot().unwrap()
    ));

    editor.update_composition(text, Some(5..9)).unwrap();
    let moved = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    let preview = cache.preview.as_ref().unwrap();
    assert!(first.identity().ptr_eq(&moved.identity()));
    assert!(Arc::ptr_eq(&identity, &preview.document.content_identity()));
    assert_eq!(preview.caret, Position::new(2, 11));
    assert_eq!(
        preview.selection,
        Some(Selection::new(Position::new(2, 0), Position::new(2, 11)))
    );
    assert_eq!(preview.composition.selection, Some(5..9));
    assert!(Arc::ptr_eq(
        &preview.composition,
        &editor.composition_snapshot().unwrap()
    ));
    assert_eq!(snapshot.selection, Some(0..3));
    assert_eq!(snapshot.text, text);

    editor.update_composition(text, None).unwrap();
    let hidden = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    let preview = cache.preview.as_ref().unwrap();
    assert!(first.identity().ptr_eq(&hidden.identity()));
    assert!(Arc::ptr_eq(&identity, &preview.document.content_identity()));
    assert_eq!(preview.caret, Position::new(2, 11));
    assert_eq!(preview.selection, None);

    editor.update_composition(text, Some(0..0)).unwrap();
    let restored = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    let preview = cache.preview.as_ref().unwrap();
    assert!(first.identity().ptr_eq(&restored.identity()));
    assert_eq!(preview.caret, Position::new(1, 0));
    assert_eq!(preview.document.plain_text(), "first\nあ\n👩‍💻\nlast");
    assert_eq!(editor.document().plain_text(), "first\nOLD\nlast");
    assert!(!editor.can_undo());
}

#[test]
fn preedit_text_updates_edit_the_preview_in_place_like_fresh_previews() {
    let link = Link::new("https://example.com").unwrap();
    let mut linked = Editor::from_text("before  after");
    linked
        .set_selection(Selection::caret(Position::new(0, 7)))
        .unwrap();
    linked.insert_link("LINKED", link).unwrap();
    let mut styled = Editor::from_text("plain\nheading\nitem\nlast");
    styled
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5)))
        .unwrap();
    styled.toggle_style(crate::InlineAttribute::Bold).unwrap();
    styled
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    styled
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    styled
        .set_selection(Selection::caret(Position::new(2, 0)))
        .unwrap();
    styled
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 0,
            start: None,
        })
        .unwrap();
    type Step = (&'static str, Option<Range<usize>>);
    let cases: Vec<(Editor, Selection, Vec<Step>)> = vec![
        (
            Editor::from_text("first\nOLD\nlast"),
            Selection::new(Position::new(1, 0), Position::new(1, 3)),
            vec![
                ("k", None),
                ("か", Some(3..3)),
                ("かn", None),
                ("かん", Some(0..3)),
                ("漢字", None),
            ],
        ),
        (
            Editor::from_text("cafe\nx"),
            Selection::caret(Position::new(0, 4)),
            vec![
                ("\u{301}", None),
                ("\u{301}x", None),
                ("y", None),
                ("\u{301}", None),
                ("z", None),
            ],
        ),
        (
            Editor::from_text("ab\u{301}cd"),
            Selection::caret(Position::new(0, 4)),
            vec![
                ("x", None),
                ("xe", None),
                ("x", None),
                ("", None),
                ("q", None),
                ("q\u{301}", None),
            ],
        ),
        (
            Editor::from_text("🇺🇸🇨"),
            Selection::caret(Position::new(0, 12)),
            vec![("🇦", None), ("🇦🇺", None), ("x", None), ("🇦", None)],
        ),
        (
            Editor::from_text("👩\u{200d}x"),
            Selection::caret(Position::new(0, 7)),
            vec![("a", None), ("\u{200d}", None), ("💻", None), ("b", None)],
        ),
        (
            Editor::from_text("\u{301}abc"),
            Selection::caret(Position::new(0, 0)),
            vec![
                ("e", None),
                ("ex", None),
                ("e", None),
                ("f", None),
                ("e", None),
            ],
        ),
        (
            Editor::from_text("\u{1161}z"),
            Selection::caret(Position::new(0, 0)),
            vec![
                ("\u{1100}", None),
                ("a\u{1100}", None),
                ("\u{1100}", None),
                ("b", None),
            ],
        ),
        (
            Editor::from_text("🇸x"),
            Selection::caret(Position::new(0, 0)),
            vec![
                ("🇺", None),
                ("🇺🇦", None),
                ("🇺🇦🇺", None),
                ("q", None),
                ("🇺", None),
            ],
        ),
        (
            linked.clone(),
            Selection::caret(Position::new(0, 10)),
            vec![("a", None), ("ab", None), ("abc", Some(1..2)), ("z", None)],
        ),
        (
            linked,
            Selection::caret(Position::new(0, 7)),
            vec![("a", None), ("ab", None), ("abc", None)],
        ),
        (
            styled.clone(),
            Selection::new(Position::new(0, 0), Position::new(2, 3)),
            vec![
                ("a", None),
                ("ab", None),
                ("a\nb", None),
                ("abc", None),
                ("a\r\nb", None),
                ("x\u{2029}y", None),
                ("q", None),
            ],
        ),
        (
            styled.clone(),
            Selection::new(Position::new(1, 3), Position::new(3, 2)),
            vec![
                ("a", None),
                ("ab", None),
                ("abc", None),
                ("", None),
                ("d", None),
            ],
        ),
        (
            styled,
            Selection::new(Position::new(0, 5), Position::new(0, 2)),
            vec![("a\u{2028}b", None), ("ab", None), ("a\u{2028}", None)],
        ),
    ];
    let appearance = appearance();
    let mut in_place = 0;
    for (case, (mut editor, replacement, steps)) in cases.into_iter().enumerate() {
        let context = egui::Context::default();
        let mut retained = Cache::default();
        editor.set_selection(replacement).unwrap();
        for (step, (text, selection)) in steps.into_iter().enumerate() {
            let previous = retained
                .preview
                .as_ref()
                .map(|preview| preview.document.content_identity());
            editor.update_composition(text, selection).unwrap();
            if text.is_empty() {
                continue;
            }
            let actual = cached_preview_frame(&context, &mut retained, &editor, &appearance);
            let mut fresh = Cache::default();
            let expected = cached_preview_frame(&context, &mut fresh, &editor, &appearance);
            let (actual_preview, expected_preview) = (
                retained.preview.as_ref().unwrap(),
                fresh.preview.as_ref().unwrap(),
            );
            if previous
                .is_some_and(|identity| actual_preview.document.change_since(&identity).is_some())
            {
                in_place += 1;
            }
            let label = format!("case {case}, step {step}");
            assert_eq!(
                actual_preview.document.to_bytes(),
                expected_preview.document.to_bytes(),
                "{label}"
            );
            assert_eq!(
                actual_preview.document.paragraphs(),
                expected_preview.document.paragraphs(),
                "{label}"
            );
            assert_eq!(actual_preview.caret, expected_preview.caret, "{label}");
            assert_eq!(
                actual_preview.selection, expected_preview.selection,
                "{label}"
            );
            assert_eq!(actual.len(), expected.len(), "{label}");
            for index in 0..actual.len() {
                let (actual, expected) = (actual.get(index).unwrap(), expected.get(index).unwrap());
                assert_eq!(actual.galley.job.text, expected.galley.job.text, "{label}");
                assert_eq!(
                    actual.galley.job.sections, expected.galley.job.sections,
                    "{label}"
                );
                assert_eq!(actual.rect, expected.rect, "{label}");
                assert_eq!(
                    actual.marker.as_ref().map(|marker| &marker.job.text),
                    expected.marker.as_ref().map(|marker| &marker.job.text),
                    "{label}"
                );
            }
        }
    }
    assert!(in_place >= 20, "{in_place} in-place preview updates");

    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text("one");
    editor.update_composition("a", None).unwrap();
    cached_preview_frame(&context, &mut cache, &editor, &appearance);
    editor = Editor::from_text("two");
    editor.update_composition("ab", None).unwrap();
    context
        .run_ui(egui::RawInput::default(), |ui| {
            cache.layout_preview(ui, &editor, &appearance).unwrap();
        })
        .drop_without_applying_deltas();
    assert_eq!(
        cache.preview.as_ref().unwrap().document.plain_text(),
        "abtwo"
    );
}

#[test]
fn reused_preedit_cache_refreshes_for_width_fonts_and_color() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text(&"words ".repeat(30));
    let mut appearance = appearance();
    editor.update_composition("あ", None).unwrap();
    let wide = cached_preview_frame(&context, &mut cache, &editor, &appearance);

    editor.update_composition("あい", None).unwrap();
    appearance.width = 100.0;
    let narrow = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(narrow.get(0).unwrap().galley.size().y > wide.get(0).unwrap().galley.size().y);

    context.set_pixels_per_point(2.0);
    let scaled = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(!Arc::ptr_eq(
        &narrow.get(0).unwrap().galley,
        &scaled.get(0).unwrap().galley
    ));
    let mut definitions = egui::FontDefinitions::default();
    definitions.families.insert(
        FontFamily::Proportional,
        definitions.families[&FontFamily::Monospace].clone(),
    );
    context.set_fonts(definitions);
    let replaced_fonts = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(!Arc::ptr_eq(
        &scaled.get(0).unwrap().galley,
        &replaced_fonts.get(0).unwrap().galley
    ));

    editor.update_composition("あいう", None).unwrap();
    appearance.color = Color32::RED;
    let recolored = cached_preview_frame(&context, &mut cache, &editor, &appearance);
    assert!(
        recolored
            .get(0)
            .unwrap()
            .galley
            .job
            .sections
            .iter()
            .all(|section| section.format.color == Color32::RED)
    );
}

#[test]
fn idle_layouts_and_unchanged_paragraph_galleys_are_reused() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text("first\nsecond\nthird");
    let appearance = appearance();
    let first = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(first.identity().ptr_eq(&idle.identity()));
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    editor.insert_text("edited ").unwrap();
    let edited = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(Arc::ptr_eq(
        &first.get(0).unwrap().galley,
        &edited.get(0).unwrap().galley
    ));
    assert!(!Arc::ptr_eq(
        &first.get(1).unwrap().galley,
        &edited.get(1).unwrap().galley
    ));
    assert!(Arc::ptr_eq(
        &first.get(2).unwrap().galley,
        &edited.get(2).unwrap().galley
    ));
    assert_eq!(edited.get(1).unwrap().galley.job.text, "edited second");
    assert!(cache.plain_text.is_none());
    assert_eq!(
        cache.char_offset(editor.document(), Position::new(2, 2)),
        22
    );
}

#[test]
fn structural_layout_updates_retain_edges_and_match_a_fresh_cache() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let mut editor = Editor::from_text("first\nsecond\nthird\nfourth");
    let mut appearance = appearance();
    editor
        .set_selection(Selection::caret(Position::new(2, 0)))
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(1, 3)))
        .unwrap();
    let original = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    editor.insert_paragraph().unwrap();
    let split = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    for (before, after) in [(0, 0), (2, 3), (3, 4)] {
        assert!(Arc::ptr_eq(
            &original.get(before).unwrap().galley,
            &split.get(after).unwrap().galley
        ));
    }
    assert_eq!(original.get(1).unwrap().galley.job.text, "second");
    assert_eq!(split.get(1).unwrap().galley.job.text, "sec");
    assert_eq!(split.get(2).unwrap().galley.job.text, "ond");

    for step in 0..5 {
        match step {
            0 => {}
            1 => assert!(editor.undo()),
            2 => {
                let fragment = Fragment::from_document(editor.document());
                editor.insert_fragment(&fragment).unwrap();
            }
            3 => {
                editor.select_all();
                editor.insert_text("new\ncontent").unwrap();
                appearance.color = Color32::RED;
            }
            4 => {
                editor.select_all();
                editor.insert_text("").unwrap();
            }
            _ => unreachable!(),
        }
        let updated = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        let mut fresh = Cache::default();
        let expected = cached_layout_frame(&context, &mut fresh, &editor, &appearance);
        assert_eq!(updated.len(), expected.len());
        for index in 0..updated.len() {
            assert_eq!(
                cache.layout.layouts.char_offset(index),
                fresh.layout.layouts.char_offset(index)
            );
            assert_eq!(
                cache.layout.layouts.contains_list(index..index + 1),
                fresh.layout.layouts.contains_list(index..index + 1)
            );
        }
        for (actual, expected) in updated.iter().zip(expected.iter()) {
            assert_eq!(actual.rect, expected.rect);
            assert_eq!(actual.marker_position, expected.marker_position);
            assert_eq!(actual.galley.job, expected.galley.job);
            assert_eq!(
                actual.marker.as_ref().map(|marker| &marker.job),
                expected.marker.as_ref().map(|marker| &marker.job)
            );
        }
    }
}

#[test]
fn retained_layouts_match_fresh_frames_through_mixed_edits_and_environment_changes() {
    fn compare_layouts(
        editor: &Editor,
        actual: &ParagraphLayouts,
        expected: &ParagraphLayouts,
        step: usize,
    ) {
        let document = editor.document();
        let origin = Pos2::new(37.0, 53.0);
        assert_eq!(actual.len(), document.paragraphs().len(), "step={step}");
        assert_eq!(actual.len(), expected.len(), "step={step}");
        assert_eq!(
            actual.content_height(),
            expected.content_height(),
            "step={step}"
        );
        for index in 0..actual.len() {
            let actual_layout = actual.get(index).unwrap();
            let expected_layout = expected.get(index).unwrap();
            assert_eq!(
                actual_layout.galley.job, expected_layout.galley.job,
                "step={step}, paragraph={index}"
            );
            assert_eq!(
                actual_layout.galley.size(),
                expected_layout.galley.size(),
                "step={step}, paragraph={index}"
            );
            assert_eq!(
                actual_layout.galley.rows.len(),
                expected_layout.galley.rows.len()
            );
            assert_eq!(
                actual_layout.rect, expected_layout.rect,
                "step={step}, paragraph={index}"
            );
            assert_eq!(
                actual_layout.marker_position, expected_layout.marker_position,
                "step={step}, paragraph={index}"
            );
            assert_eq!(
                actual_layout.marker.as_ref().map(|m| (&m.job, m.size())),
                expected_layout.marker.as_ref().map(|m| (&m.job, m.size()))
            );
            assert_eq!(
                actual.char_offset(index),
                expected.char_offset(index),
                "step={step}, paragraph={index}"
            );
            for range in [index..index + 1, 0..index + 1, index..actual.len()] {
                assert_eq!(
                    actual.contains_list(range.clone()),
                    expected.contains_list(range),
                    "step={step}, paragraph={index}"
                );
            }
            let actual_view = actual.views_range(index..index + 1).next().unwrap();
            let expected_view = expected.views_range(index..index + 1).next().unwrap();
            assert_eq!(actual_view.rect, expected_view.rect);
            assert_eq!(actual_view.rect, actual_layout.rect);
            assert_eq!(actual_view.marker_position, expected_view.marker_position);
            assert_eq!(actual_view.galley.job, expected_view.galley.job);
            assert_eq!(
                actual_view.marker.map(|m| &m.job),
                expected_view.marker.map(|m| &m.job)
            );
            for y in [
                actual_layout.rect.top(),
                actual_layout.rect.bottom(),
                actual_layout.rect.bottom() + PARAGRAPH_GAP / 2.0,
            ] {
                assert_eq!(
                    actual.first_bottom_at_least(y),
                    expected.first_bottom_at_least(y)
                );
                assert_eq!(actual.first_top_after(y), expected.first_top_after(y));
                assert_eq!(actual.paragraph_at_y(y), expected.paragraph_at_y(y));
                for x in [
                    actual_layout.rect.left() - 1.0,
                    actual_layout.rect.center().x,
                    actual_layout.rect.right() + 1.0,
                ] {
                    assert_eq!(
                        hit_test(editor, actual, Vec2::new(x, y)),
                        hit_test(editor, expected, Vec2::new(x, y)),
                        "step={step}, paragraph={index}, point={x},{y}"
                    );
                }
            }
            let source = document.paragraph(index).unwrap();
            for grapheme in [0, source.grapheme_count() / 2, source.grapheme_count()] {
                let position = Position::new(index, source.byte_from_grapheme(grapheme).unwrap());
                for affinity in [false, true] {
                    let actual_caret =
                        document_caret_rect(document, actual, origin, position, affinity);
                    let expected_caret =
                        document_caret_rect(document, expected, origin, position, affinity);
                    assert_eq!(
                        actual_caret, expected_caret,
                        "step={step}, position={position:?}, affinity={affinity}"
                    );
                    let point = actual_caret.center() - origin;
                    assert_eq!(
                        hit_test(editor, actual, point),
                        hit_test(editor, expected, point)
                    );
                }
            }
        }
        assert_eq!(
            actual.char_offset(actual.len()),
            expected.char_offset(expected.len())
        );
    }

    fn set_middle(editor: &mut Editor, index: usize) {
        let source = editor.document().paragraph(index).unwrap();
        let byte = source
            .byte_from_grapheme(source.grapheme_count() / 2)
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::new(index, byte)))
            .unwrap();
    }

    let context = egui::Context::default();
    let source = (0..160)
        .map(|index| format!("{index}: café e\u{301} 👩‍💻 日本語 words\n"))
        .collect::<String>();
    let mut editor = Editor::from_text(&source);
    let mut retained = Cache::default();
    let mut appearance = appearance();
    cached_layout_frame(&context, &mut retained, &editor, &appearance);

    for step in 0..144 {
        let index = (step * 47 + 13) % editor.document().paragraphs().len();
        match step % 18 {
            0 => {
                set_middle(&mut editor, index);
                editor.insert_text("e\u{301} x ").unwrap();
            }
            1 => {
                let end = editor.document().paragraph(index).unwrap().text().len();
                editor
                    .set_selection(Selection::new(
                        Position::new(index, 0),
                        Position::new(index, end),
                    ))
                    .unwrap();
                editor
                    .apply_style(StylePatch {
                        bold: Some(step % 36 == 1),
                        italic: Some(true),
                        code: Some(step % 36 == 19),
                        ..StylePatch::default()
                    })
                    .unwrap();
            }
            2 => {
                set_middle(&mut editor, index);
                editor
                    .set_paragraph_kind(ParagraphKind::Bullet {
                        indent: (step % 3) as u8,
                    })
                    .unwrap();
            }
            3 => {
                set_middle(&mut editor, index);
                editor
                    .set_paragraph_kind(ParagraphKind::Ordered {
                        indent: 1,
                        start: Some(u32::MAX - 1),
                    })
                    .unwrap();
            }
            4 => {
                set_middle(&mut editor, index);
                editor.insert_paragraph().unwrap();
            }
            5 => {
                let index = index.max(1);
                editor
                    .set_selection(Selection::caret(Position::new(index, 0)))
                    .unwrap();
                editor.delete_backward().unwrap();
            }
            6 => {
                for index in [
                    1,
                    editor.document().paragraphs().len() / 2,
                    editor.document().paragraphs().len() - 2,
                ] {
                    set_middle(&mut editor, index);
                    editor.insert_text(" skipped ").unwrap();
                }
            }
            7 => assert!(editor.undo()),
            8 => assert!(editor.redo()),
            9 => {
                let end = editor.document().paragraph(3).unwrap().text().len();
                let fragment = editor
                    .document()
                    .fragment(Position::default()..Position::new(3, end))
                    .unwrap();
                editor
                    .set_selection(Selection::caret(Position::new(60 + step % 30, 0)))
                    .unwrap();
                editor.insert_fragment(&fragment).unwrap();
            }
            10 => editor = Editor::new(editor.document().clone()),
            11 => {
                editor = Editor::new(Document::from_bytes(&editor.document().to_bytes()).unwrap())
            }
            12 => {
                set_middle(&mut editor, index);
                editor.update_composition("あ\n👩‍💻", Some(3..3)).unwrap();
            }
            13 => editor.update_composition("あ\n👩‍💻", Some(4..4)).unwrap(),
            14 => editor
                .update_composition("日本e\u{301}", Some(6..9))
                .unwrap(),
            15 => assert!(editor.cancel_composition()),
            16 => {
                editor.update_composition("", None).unwrap();
                editor.commit_composition("x\n日本").unwrap();
            }
            17 => match (step / 18) % 4 {
                0 => {
                    appearance.width = if appearance.width > 100.0 {
                        80.0
                    } else {
                        400.0
                    }
                }
                1 => {
                    context.set_visuals(egui::Visuals::light());
                    appearance.color = Color32::from_rgb(25, 90, 135);
                    appearance.strong_color = Color32::from_rgb(150, 45, 75);
                    appearance.code_background = Color32::from_rgb(230, 220, 180);
                }
                2 => {
                    let mut definitions = egui::FontDefinitions::default();
                    definitions.families.insert(
                        FontFamily::Proportional,
                        definitions.families[&FontFamily::Monospace].clone(),
                    );
                    context.set_fonts(definitions);
                }
                _ => context.set_pixels_per_point(if step < 100 { 2.0 } else { 1.0 }),
            },
            _ => unreachable!(),
        }
        assert!(editor.document().paragraphs().len() >= 100);
        let mut fresh = Cache::default();
        context
            .run_ui(egui::RawInput::default(), |ui| {
                let actual = retained.layout(ui, &editor, &appearance);
                let expected = fresh.layout(ui, &editor, &appearance);
                compare_layouts(&editor, &actual, &expected, step);
                assert_eq!(
                    retained.plain_text(editor.document()),
                    fresh.plain_text(editor.document())
                );
                for index in [
                    0,
                    editor.document().paragraphs().len() / 2,
                    editor.document().paragraphs().len() - 1,
                ] {
                    let position = Position::new(
                        index,
                        editor.document().paragraph(index).unwrap().text().len(),
                    );
                    assert_eq!(
                        retained.char_offset(editor.document(), position),
                        fresh.char_offset(editor.document(), position)
                    );
                }
                assert_eq!(
                    retained.selection_contains_list(&editor),
                    fresh.selection_contains_list(&editor)
                );
                let actual_preview = retained.layout_preview(ui, &editor, &appearance).unwrap();
                let expected_preview = fresh.layout_preview(ui, &editor, &appearance).unwrap();
                assert_eq!(
                    actual_preview.is_some(),
                    expected_preview.is_some(),
                    "step={step}"
                );
                if let (Some(actual), Some(expected)) = (actual_preview, expected_preview) {
                    let actual_preview = retained.preview.as_ref().unwrap();
                    let expected_preview = fresh.preview.as_ref().unwrap();
                    assert_eq!(
                        actual_preview.document.to_bytes(),
                        expected_preview.document.to_bytes()
                    );
                    assert_eq!(actual_preview.caret, expected_preview.caret);
                    assert_eq!(actual_preview.selection, expected_preview.selection);
                    let preview_editor = Editor::new(actual_preview.document.clone());
                    compare_layouts(&preview_editor, &actual, &expected, step);
                }
            })
            .drop_without_applying_deltas();
    }
}

#[test]
fn layout_cache_refreshes_for_width_scale_fonts_and_colors() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let editor = Editor::from_text(&"words ".repeat(30));
    let mut appearance = appearance();
    let wide = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    appearance.width = 100.0;
    let narrow = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(narrow.get(0).unwrap().galley.size().y > wide.get(0).unwrap().galley.size().y);
    context.set_pixels_per_point(2.0);
    let scaled = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(!Arc::ptr_eq(
        &narrow.get(0).unwrap().galley,
        &scaled.get(0).unwrap().galley
    ));
    let mut definitions = egui::FontDefinitions::default();
    definitions.families.insert(
        FontFamily::Proportional,
        definitions.families[&FontFamily::Monospace].clone(),
    );
    context.set_fonts(definitions);
    let replaced_fonts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(!Arc::ptr_eq(
        &scaled.get(0).unwrap().galley,
        &replaced_fonts.get(0).unwrap().galley
    ));
    appearance.color = Color32::RED;
    let recolored = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert_eq!(
        recolored.get(0).unwrap().galley.job.sections[0]
            .format
            .color,
        Color32::RED
    );
    let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert!(recolored.identity().ptr_eq(&idle.identity()));
}

#[test]
fn malformed_shaping_retries_preserve_rendered_text_styles_and_widget_navigation() {
    let context = egui::Context::default();
    let cluster = format!("{}z", "\u{600}".repeat(16));
    let text = format!("a {cluster} end");
    let mut editor = Editor::from_text(&text);
    editor
        .set_selection(Selection::new(
            Position::new(0, 2),
            Position::new(0, 2 + cluster.len()),
        ))
        .unwrap();
    editor
        .apply_style(StylePatch {
            italic: Some(true),
            underline: Some(true),
            foreground: Some(Some(crate::Color([42, 100, 200, 255]))),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::default()))
        .unwrap();
    let revision = editor.document().revision();
    let draw = |editor: &mut Editor, events| {
        context
            .run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let id = ui.make_persistent_id("malformed-shaping");
                        ui.memory_mut(|memory| memory.request_focus(id));
                        let output = RichTextEditor::new(editor)
                            .id(id)
                            .font(FontId::proportional(14.0))
                            .desired_width(48.0)
                            .show(ui);
                        assert!(output.errors.is_empty(), "{:?}", output.errors);
                    });
                },
            )
            .drop_without_applying_deltas();
    };
    draw(&mut editor, vec![]);
    let id = context.memory(|memory| memory.focused()).unwrap();
    let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
    let galley = &cache.layout.layouts.get(0).unwrap().galley;
    assert_eq!(galley.job.text, text);
    assert_eq!(galley.end().index.0, text.chars().count());
    let rendered: String = galley
        .rows
        .iter()
        .flat_map(|row| row.glyphs.iter().map(|glyph| glyph.chr))
        .collect();
    assert_eq!(rendered, text);
    let source = editor.document().paragraph(0).unwrap();
    for section in &galley.job.sections {
        let style = source
            .spans()
            .iter()
            .find(|span| span.range.contains(&section.byte_range.start.0))
            .unwrap()
            .style;
        assert_eq!(section.format.italics, style.italic);
        assert_eq!(section.format.underline != Stroke::NONE, style.underline);
        if let Some(color) = style.foreground {
            assert_eq!(
                section.format.color,
                Color32::from_rgba_unmultiplied(color.0[0], color.0[1], color.0[2], color.0[3])
            );
        }
    }
    draw(&mut editor, vec![]);
    let idle = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
    assert!(
        cache
            .layout
            .layouts
            .identity()
            .ptr_eq(&idle.layout.layouts.identity())
    );
    assert!(Arc::ptr_eq(
        galley,
        &idle.layout.layouts.get(0).unwrap().galley
    ));
    for _ in 0..galley.rows.len() {
        draw(&mut editor, vec![key(Key::ArrowDown, Modifiers::NONE)]);
        editor
            .document()
            .validate_position(editor.selection().focus)
            .unwrap();
    }
    assert_eq!(editor.selection().focus, editor.document().end());
    for _ in 0..galley.rows.len() {
        draw(&mut editor, vec![key(Key::ArrowUp, Modifiers::NONE)]);
        editor
            .document()
            .validate_position(editor.selection().focus)
            .unwrap();
    }
    assert_eq!(editor.selection().focus, Position::default());
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.document().plain_text(), text);
}

#[test]
fn font_probe_refreshes_layouts_when_text_options_recreate_the_atlas() {
    let context = egui::Context::default();
    let mut cache = Cache::default();
    let editor = Editor::from_text("atlas glyphs café 👩‍💻\nunchanged paragraph");
    let appearance = appearance();
    let mut previous = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    for change_hinting in [true, false] {
        context.global_style_mut(|style| {
            let options = &mut style.visuals.text_options;
            if change_hinting {
                options.font_hinting = !options.font_hinting;
            } else {
                options.subpixel_binning = !options.subpixel_binning;
            }
        });
        let refreshed = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        for (old, new) in previous.iter().zip(refreshed.iter()) {
            assert!(!Arc::ptr_eq(&old.galley, &new.galley));
            assert_eq!(old.galley.job.text, new.galley.job.text);
        }
        let idle = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        assert!(refreshed.identity().ptr_eq(&idle.identity()));
        previous = refreshed;
    }
}

#[test]
fn inactive_accessibility_keeps_text_lazy_but_events_report_changes() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    frame(&context, &mut editor, vec![], true);
    let output = frame(&context, &mut editor, vec![Event::Text("x".into())], true);
    let id = context.memory(|memory| memory.focused()).unwrap();
    assert!(
        context
            .data(|data| data.get_temp::<Cache>(id))
            .unwrap()
            .plain_text
            .is_none()
    );
    assert!(
        output
            .platform_output
            .events
            .iter()
            .any(|event| matches!(event, egui::output::OutputEvent::ValueChanged(_)))
    );
    context.enable_accesskit();
    let output = frame(&context, &mut editor, vec![Event::Text("y".into())], true);
    assert!(output.platform_output.events.iter().any(|event| {
        matches!(event, egui::output::OutputEvent::ValueChanged(info)
                if info.current_text_value.as_deref() == Some("xyhello")
                && info.prev_text_value.as_deref() == Some("xhello"))
    }));
}

#[test]
fn read_only_consumes_mutations_and_preserves_copy_selection_and_history() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    editor.insert_text("x").unwrap();
    editor.select_all();
    let revision = editor.document().revision();
    let output = frame_options(
        &context,
        &mut editor,
        vec![
            Event::Cut,
            Event::Paste("pasted".into()),
            Event::Text("typed".into()),
            key(Key::Backspace, Modifiers::NONE),
            key(Key::Delete, Modifiers::NONE),
            key(Key::Enter, Modifiers::NONE),
            key(Key::B, Modifiers::COMMAND),
            key(Key::Z, Modifiers::COMMAND),
            key(Key::Y, Modifiers::COMMAND),
            key(Key::Num1, Modifiers::COMMAND),
            Event::Ime(ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: Some(1..1),
            }),
            Event::Ime(ImeEvent::Commit("日本".into())),
            Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 1,
                after_chars: 1,
            }),
        ],
        true,
        true,
    );
    assert_eq!(editor.document().plain_text(), "xhello");
    assert_eq!(editor.document().revision(), revision);
    assert!(!editor.typing_style().bold);
    assert!(editor.composition().is_none());
    assert!(output.platform_output.ime.is_none());
    assert!(output.platform_output.commands.iter().any(|command| {
        matches!(command, egui::OutputCommand::CopyText(text) if text == "xhello")
    }));
    assert!(context.memory(|memory| memory.focused()).is_some());
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::ArrowRight, Modifiers::NONE)],
        true,
        true,
    );
    assert!(editor.selection().is_caret());
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::A, Modifiers::COMMAND)],
        true,
        true,
    );
    assert_eq!(editor.selected_text(), "xhello");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "hello");
}

#[test]
fn read_only_cancels_existing_composition() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    editor.update_composition("candidate", None).unwrap();
    frame_options(&context, &mut editor, vec![], true, true);
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(!editor.can_undo());
}

#[test]
fn disabling_widget_cancels_composition_without_processing_input() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    editor.update_composition("candidate", None).unwrap();
    let output = context.run_ui(
        egui::RawInput {
            events: vec![Event::Text("typed".into())],
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.disable();
                let id = ui.make_persistent_id("editor");
                ui.add(RichTextEditor::new(&mut editor).id(id));
            });
        },
    );
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(output.platform_output.ime.is_none());
    assert!(!editor.can_undo());
    output.drop_without_applying_deltas();
}

#[test]
fn escape_releases_focus_before_later_text_events() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    frame(
        &context,
        &mut editor,
        vec![
            key(Key::Escape, Modifiers::NONE),
            Event::Text("typed".into()),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "original");
    assert!(context.memory(|memory| memory.focused()).is_none());
    assert_eq!(
        context.input(|input| input.events.clone()),
        vec![Event::Text("typed".into())]
    );
}

#[test]
fn second_escape_after_canceling_composition_releases_focus() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    editor.update_composition("candidate", None).unwrap();
    frame(
        &context,
        &mut editor,
        vec![
            key(Key::Escape, Modifiers::NONE),
            key(Key::Escape, Modifiers::NONE),
            Event::Text("typed".into()),
        ],
        true,
    );
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(context.memory(|memory| memory.focused()).is_none());
    assert_eq!(
        context.input(|input| input.events.clone()),
        vec![Event::Text("typed".into())]
    );
}

#[test]
fn first_escape_cancels_empty_composition_and_second_releases_focus() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    editor.update_composition("", None).unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Escape, Modifiers::NONE)],
        true,
    );
    assert!(editor.composition().is_none());
    assert!(context.memory(|memory| memory.focused()).is_some());
    frame(
        &context,
        &mut editor,
        vec![
            key(Key::Escape, Modifiers::NONE),
            Event::Text("typed".into()),
        ],
        false,
    );
    assert_eq!(editor.document().plain_text(), "original");
    assert!(context.memory(|memory| memory.focused()).is_none());
}

#[test]
fn escape_releases_focus_after_read_only_cancels_composition() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    editor.update_composition("candidate", None).unwrap();
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::Escape, Modifiers::NONE)],
        true,
        true,
    );
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(context.memory(|memory| memory.focused()).is_none());
}

#[test]
fn command_navigation_and_altgr_preserve_platform_shortcut_semantics() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("first\nsecond");
    let mac_command = Modifiers::MAC_CMD | Modifiers::COMMAND;
    editor
        .set_selection(Selection::caret(Position::new(1, 3)))
        .unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::ArrowLeft, mac_command)],
        true,
    );
    assert_eq!(editor.selection().focus, Position::new(1, 0));
    frame(
        &context,
        &mut editor,
        vec![key(Key::ArrowRight, mac_command)],
        true,
    );
    assert_eq!(editor.selection().focus, Position::new(1, 6));
    frame(
        &context,
        &mut editor,
        vec![key(Key::ArrowUp, mac_command)],
        true,
    );
    assert_eq!(editor.selection().focus, Position::default());
    frame(
        &context,
        &mut editor,
        vec![key(Key::ArrowDown, mac_command)],
        true,
    );
    assert_eq!(editor.selection().focus, editor.document().end());
    let altgr = Modifiers::ALT | Modifiers::CTRL | Modifiers::COMMAND;
    frame(
        &context,
        &mut editor,
        vec![key(Key::B, altgr), Event::Text("β".into())],
        true,
    );
    assert_eq!(editor.document().plain_text(), "first\nsecondβ");
    assert!(!editor.typing_style().bold);
}

#[test]
fn command_backspace_deletes_to_paragraph_start_and_undo_restores_selection() {
    let text = "prefix\ncafé 👩🏽‍💻 tail";
    let end = Position::new(1, "café 👩🏽‍💻".len());
    let caret = Selection::caret(end);
    for selection in [
        caret,
        Selection::new(Position::new(0, 1), end),
        Selection::new(end, Position::new(0, 1)),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text(text);
        editor.set_selection(selection).unwrap();
        let command = Modifiers::MAC_CMD | Modifiers::COMMAND;
        frame(
            &context,
            &mut editor,
            vec![key(Key::Backspace, command)],
            true,
        );
        assert_eq!(
            editor.document().plain_text(),
            if selection == caret {
                "prefix\n tail"
            } else {
                "p tail"
            }
        );
        assert_eq!(editor.undo_len(), 1);
        if selection == caret {
            let revision = editor.document().revision();
            frame(
                &context,
                &mut editor,
                vec![key(Key::Backspace, command)],
                true,
            );
            assert_eq!(editor.document().revision(), revision);
            assert_eq!(editor.undo_len(), 1);
        }
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), text);
        assert_eq!(editor.selection(), selection);
    }
}

#[test]
fn command_backspace_respects_read_only_ime_and_modifier_guards() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("one two");
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    let command = Modifiers::MAC_CMD | Modifiers::COMMAND;
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::Backspace, command)],
        true,
        true,
    );
    editor.update_composition("候", None).unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Backspace, command)],
        true,
    );
    assert!(editor.composition().is_some());
    editor.cancel_composition();
    assert_eq!(editor.document().plain_text(), "one two");
    assert!(!editor.can_undo());

    for modifiers in [
        Modifiers::CTRL | Modifiers::ALT | Modifiers::COMMAND,
        Modifiers::CTRL | Modifiers::MAC_CMD | Modifiers::COMMAND,
    ] {
        frame(
            &context,
            &mut editor,
            vec![key(Key::Backspace, modifiers)],
            true,
        );
        assert_eq!(editor.document().plain_text(), "one ");
        assert!(editor.undo());
    }
}

#[test]
fn modifier_delete_shortcuts_remove_whole_words_and_restore_selection_on_undo() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("café 東京 next");
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    let before = editor.selection();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Backspace, Modifiers::CTRL)],
        true,
    );
    assert_eq!(editor.document().plain_text(), "café 東京 ");
    assert!(editor.undo());
    assert_eq!(editor.selection(), before);
    editor
        .set_selection(Selection::caret(Position::default()))
        .unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Delete, Modifiers::ALT)],
        true,
    );
    assert_eq!(editor.document().plain_text(), " 東京 next");
}

#[test]
fn tab_and_shift_tab_indent_lists_while_body_tab_keeps_host_focus_navigation() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("first\nsecond");
    editor.select_all();
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    frame(&context, &mut editor, vec![], true);
    frame(&context, &mut editor, vec![], true);
    frame(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::NONE)],
        true,
    );
    assert!(
        editor
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 1 })
    );
    frame(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::SHIFT)],
        true,
    );
    assert!(
        editor
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 0 })
    );
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::NONE)],
        true,
        true,
    );
    assert!(
        editor
            .document()
            .paragraphs()
            .iter()
            .all(|paragraph| paragraph.kind() == ParagraphKind::Bullet { indent: 0 })
    );
    editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::NONE)],
        true,
    );
    assert_eq!(editor.document().plain_text(), "first\nsecond");
    assert!(context.input(|input| {
        input
            .events
            .iter()
            .any(|event| matches!(event, Event::Key { key: Key::Tab, .. }))
    }));
}

#[test]
fn read_only_native_node_advertises_selection_and_rejects_mutation_actions() {
    use egui::accesskit::{
        Action, ActionData, ActionRequest, Role, TextPosition, TextSelection, TreeId,
    };
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("original");
    let output = frame_options(&context, &mut editor, vec![], true, true);
    let update = output.platform_output.accesskit_update.unwrap();
    let (id, node) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .unwrap();
    assert!(node.is_read_only());
    assert!(node.supports_action(Action::SetTextSelection));
    assert!(!node.supports_action(Action::SetValue));
    assert!(!node.supports_action(Action::ReplaceSelectedText));
    let (run_id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun)
        .unwrap();
    frame_options(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetTextSelection,
            target_node: *id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::SetTextSelection(TextSelection {
                anchor: TextPosition {
                    node: *run_id,
                    character_index: 0,
                },
                focus: TextPosition {
                    node: *run_id,
                    character_index: 3,
                },
            })),
        })],
        true,
        true,
    );
    assert_eq!(editor.selected_text(), "ori");
    for action in [Action::SetValue, Action::ReplaceSelectedText] {
        frame_options(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(ActionRequest {
                action,
                target_node: *id,
                target_tree: TreeId::ROOT,
                data: Some(ActionData::Value("replacement".into())),
            })],
            true,
            true,
        );
    }
    assert_eq!(editor.document().plain_text(), "original");
    assert_eq!(editor.selected_text(), "ori");
    assert!(!editor.can_undo());
}

#[test]
fn focused_widget_edits_and_undoes() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    frame(
        &context,
        &mut editor,
        vec![Event::Text("世界".into())],
        true,
    );
    assert_eq!(editor.document().plain_text(), "世界hello");
    frame(
        &context,
        &mut editor,
        vec![key(Key::Z, Modifiers::COMMAND)],
        true,
    );
    assert_eq!(editor.document().plain_text(), "hello");
    frame(
        &context,
        &mut editor,
        vec![key(Key::Y, Modifiers::COMMAND)],
        true,
    );
    assert_eq!(editor.document().plain_text(), "世界hello");
}

#[test]
fn unfocused_widget_leaves_text_events_for_host() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    frame(
        &context,
        &mut editor,
        vec![Event::Text("world".into())],
        false,
    );
    assert_eq!(editor.document().plain_text(), "hello");
}

#[test]
fn ime_preedit_is_transient_then_commits_once() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    let preedit = "に\r\n👩‍💻e\u{301}";
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: preedit.into(),
            active_range_chars: Some(3..7),
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "hello");
    assert_eq!(
        editor
            .composition()
            .map(|composition| composition.text.as_str()),
        Some(preedit)
    );
    assert_eq!(editor.composition().unwrap().selection, Some(5..17));
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Commit("日本".into()))],
        true,
    );
    assert_eq!(editor.document().plain_text(), "日本hello");
    assert!(editor.composition().is_none());
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "hello");
}

#[test]
fn preedit_scalar_ranges_preserve_raw_unicode_offsets_and_reject_invalid_ranges() {
    let long_unicode = "に\r\n👩‍💻e\u{301}".repeat(9);
    let long_ascii = "a".repeat(65);
    for text in ["", "plain", "に\r\n👩‍💻e\u{301}", &long_unicode, &long_ascii] {
        let boundaries: Vec<_> = text
            .char_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        for start in 0..boundaries.len() {
            for end in start..boundaries.len() {
                assert_eq!(
                    preedit_byte_range(text, &(start..end)),
                    Ok(boundaries[start]..boundaries[end]),
                    "text={text:?}, scalars={start}..{end}"
                );
            }
        }
        for range in [
            Range { start: 1, end: 0 },
            0..boundaries.len(),
            boundaries.len()..boundaries.len(),
            0..usize::MAX,
            Range {
                start: usize::MAX,
                end: 0,
            },
            usize::MAX - 1..usize::MAX,
            usize::MAX..usize::MAX,
        ] {
            assert_eq!(
                preedit_byte_range(text, &range),
                Err(Error::InvalidCompositionSelection)
            );
        }
    }
}

#[test]
fn rejected_native_preedit_ranges_preserve_the_active_composition() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "に".into(),
            active_range_chars: Some(1..1),
        })],
        true,
    );
    let composition = editor.composition().cloned();
    for range in [Range { start: 3, end: 1 }, 0..99, usize::MAX..usize::MAX] {
        let mut errors = Vec::new();
        context
            .run_ui(
                egui::RawInput {
                    events: vec![Event::Ime(ImeEvent::Preedit {
                        text: "changed\r\n👩‍💻".into(),
                        active_range_chars: Some(range),
                    })],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let id = ui.make_persistent_id("editor");
                        ui.memory_mut(|memory| memory.request_focus(id));
                        errors = RichTextEditor::new(&mut editor).id(id).show(ui).errors;
                    });
                },
            )
            .drop_without_applying_deltas();
        assert_eq!(errors, vec![Error::InvalidCompositionSelection]);
        assert_eq!(editor.composition(), composition.as_ref());
        assert_eq!(editor.document().plain_text(), "hello");
        assert!(!editor.can_undo());
    }
}

#[test]
fn emphasis_shortcuts_normalize_mixed_selections_in_both_directions() {
    for (shortcut, patch) in [
        (
            Key::B,
            StylePatch {
                bold: Some(true),
                ..Default::default()
            },
        ),
        (
            Key::I,
            StylePatch {
                italic: Some(true),
                ..Default::default()
            },
        ),
        (
            Key::U,
            StylePatch {
                underline: Some(true),
                ..Default::default()
            },
        ),
    ] {
        for reverse in [false, true] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("aé");
            editor
                .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 1)))
                .unwrap();
            editor.apply_style(patch).unwrap();
            let selection = if reverse {
                Selection::new(editor.document().end(), Position::default())
            } else {
                Selection::new(Position::default(), editor.document().end())
            };
            editor.set_selection(selection).unwrap();
            editor.clear_history();
            let original = editor.document().to_bytes();

            for enabled in [true, false] {
                frame(
                    &context,
                    &mut editor,
                    vec![key(shortcut, Modifiers::CTRL | Modifiers::COMMAND)],
                    true,
                );
                let style = editor.selection_style();
                let actual = match shortcut {
                    Key::B => style.bold,
                    Key::I => style.italic,
                    _ => style.underline,
                };
                assert_eq!(actual, Some(enabled));
                assert_eq!(editor.selection(), selection);
            }
            assert_eq!(editor.undo_len(), 2);
            assert!(editor.undo());
            assert!(editor.undo());
            assert_eq!(editor.document().to_bytes(), original);
            assert_eq!(editor.selection(), selection);
        }
    }
}

#[test]
fn formatting_shortcut_produces_formatted_layout_sections() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("styled");
    editor.select_all();
    frame(
        &context,
        &mut editor,
        vec![key(Key::I, Modifiers::COMMAND)],
        true,
    );
    let mut checked = false;
    context
        .run_ui(egui::RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let appearance = Appearance {
                    font: FontId::proportional(16.0),
                    bold_family: None,
                    color: Color32::WHITE,
                    strong_color: Color32::WHITE,
                    code_background: Color32::BLACK,
                    link_color: Color32::LIGHT_BLUE,
                    width: 400.0,
                };
                let job = paragraph_job(
                    editor.document().paragraph(0).expect("paragraph"),
                    &appearance,
                    400.0,
                );
                assert!(job.sections.iter().all(|section| section.format.italics));
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                assert_eq!(galley.job.text, "styled");
                checked = true;
            });
        })
        .drop_without_applying_deltas();
    assert!(checked);
}

#[test]
fn character_hit_positions_snap_to_whole_graphemes() {
    let context = egui::Context::default();
    for text in ["", "plain", "a👨‍👩‍👧‍👦e\u{301} 🇺🇸", "\u{600}a b"] {
        let editor = Editor::from_text(text);
        let layouts = cached_layout_frame(&context, &mut Cache::default(), &editor, &appearance());
        let layout = layouts.get(0).unwrap();
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect();
        let mut previous = 0;
        let mut x = layout.rect.left() - 4.0;
        while x < layout.rect.left() + layout.galley.rect.right() + 4.0 {
            let (position, _) = hit_test(&editor, &layouts, Vec2::new(x, layout.rect.center().y));
            assert!(
                boundaries.contains(&position.byte) && position.byte >= previous,
                "text={text:?}, x={x}, byte={}",
                position.byte
            );
            previous = position.byte;
            x += 0.25;
        }
    }
}

#[test]
fn clicks_inside_multi_scalar_graphemes_snap_to_the_visually_nearer_edge() {
    let context = egui::Context::default();
    for cluster in ["🇺🇸", "👍🏽", "e\u{301}"] {
        let text = format!("a{cluster}b");
        let end = 1 + cluster.len();
        let editor = Editor::from_text(&text);
        let layouts = cached_layout_frame(&context, &mut Cache::default(), &editor, &appearance());
        let caret = |byte| {
            document_caret_rect(
                editor.document(),
                &layouts,
                Pos2::ZERO,
                Position::new(0, byte),
                true,
            )
            .center()
        };
        let (left, right) = (caret(1), caret(end));
        for (fraction, expected) in [(0.25, 1), (0.75, end)] {
            let point = left.to_vec2() + (right - left) * fraction;
            assert_eq!(
                hit_test(&editor, &layouts, point).0,
                Position::new(0, expected),
                "text={text:?}, fraction={fraction}"
            );
        }
    }
}

#[test]
fn selection_paints_hard_break_only_after_the_final_wrapped_row() {
    let context = egui::Context::default();
    let editor = Editor::from_text("first second third fourth fifth\nnext");
    let mut cache = Cache::default();
    let mut appearance = appearance();
    appearance.width = 70.0;
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    let rows = &layouts.get(0).unwrap().galley.rows;
    assert!(rows.len() >= 3);
    let last_start = rows[..rows.len() - 1]
        .iter()
        .map(|row| row.char_count_including_newline().0)
        .sum();
    let byte = editor
        .document()
        .paragraph(0)
        .unwrap()
        .byte_from_scalar(last_start)
        .unwrap();
    let origin = Pos2::new(10.0, 10.0);
    let color = Color32::from_rgb(4, 5, 6);
    let rectangles = |selection| {
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            paint_document(
                ui.painter(),
                &mut PaintCache::default(),
                editor.document(),
                &layouts,
                origin,
                &[],
                Some((selection, color)),
                &FontFamily::Proportional,
                Color32::WHITE,
                Color32::GRAY,
            );
        });
        output.textures_delta.clear();
        output
            .shapes
            .into_iter()
            .filter_map(|clipped| match clipped.shape {
                egui::epaint::Shape::Rect(rectangle) if rectangle.fill == color => {
                    Some(rectangle.rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let selected = rectangles(Selection::new(Position::new(0, byte), Position::new(1, 0)));
    assert_eq!(selected.len(), 1);
    let final_row = rows.last().unwrap();
    assert_eq!(selected[0].top(), origin.y + final_row.rect().top());
    assert_eq!(
        selected[0].right(),
        origin.x + final_row.rect().right() + 5.0
    );

    let selected = rectangles(Selection::new(Position::default(), Position::new(1, 0)));
    assert_eq!(selected.len(), rows.len());
    for (index, (rect, row)) in selected.iter().zip(rows).enumerate() {
        let extension = if index + 1 == rows.len() { 5.0 } else { 0.0 };
        assert_eq!(rect.right(), origin.x + row.rect().right() + extension);
    }
}

#[test]
fn clicking_a_wrap_seam_keeps_the_caret_on_the_clicked_row() {
    for read_only in [false, true] {
        let context = egui::Context::default();
        let id = Id::new("wrap seam");
        let mut editor = Editor::from_text("first second third fourth fifth sixth seventh");
        let mut run_frame = |events| {
            let mut rect = Rect::NOTHING;
            context
                .run_ui(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                        events,
                        ..Default::default()
                    },
                    |ui| {
                        let output = RichTextEditor::new(&mut editor)
                            .id(id)
                            .desired_width(120.0)
                            .min_rows(1)
                            .read_only(read_only)
                            .show(ui);
                        assert!(output.errors.is_empty());
                        rect = output.response.rect;
                    },
                )
                .drop_without_applying_deltas();
            rect
        };
        let rect = run_frame(vec![]);
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        let layout = &cache.layout.layouts.get(0).unwrap();
        assert!(layout.galley.rows.len() >= 3);
        let row = &layout.galley.rows[0];
        let pointer = rect.min
            + Vec2::splat(PADDING)
            + layout.rect.min.to_vec2()
            + Vec2::new(row.rect().right() + 1.0, row.rect().center().y);
        let click = |pressed| Event::PointerButton {
            pos: pointer,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        run_frame(vec![Event::PointerMoved(pointer), click(true)]);
        run_frame(vec![click(false)]);
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        let navigation = cache
            .interaction
            .navigation
            .as_ref()
            .expect("pointer affinity");
        assert!(!navigation.prefer_next_row);
        assert_eq!(
            navigation.selection.focus.byte,
            row.char_count_including_newline().0
        );
        let source = &cache.layout.layouts.caches(0..1).next().unwrap().paragraph;
        assert_eq!(
            cache
                .layout
                .layouts
                .get(0)
                .unwrap()
                .galley
                .layout_from_cursor(CCursor {
                    index: CharIndex(
                        source
                            .scalar_index(navigation.selection.focus.byte)
                            .unwrap()
                    ),
                    prefer_next_row: navigation.prefer_next_row,
                })
                .row,
            0
        );

        let caret_rect = cache.interaction.caret_rect;
        run_frame(vec![key(Key::F1, Modifiers::NONE)]);
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        assert_eq!(cache.interaction.caret_rect, caret_rect);
        for shortcut in [Key::B, Key::I, Key::U] {
            run_frame(vec![key(shortcut, Modifiers::COMMAND)]);
            let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
            assert_eq!(cache.interaction.caret_rect, caret_rect, "{shortcut:?}");
        }
        if read_only {
            for ignored in [
                key(Key::Backspace, Modifiers::NONE),
                key(Key::Delete, Modifiers::NONE),
                key(Key::Enter, Modifiers::NONE),
            ] {
                run_frame(vec![ignored.clone()]);
                let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
                assert_eq!(cache.interaction.caret_rect, caret_rect, "{ignored:?}");
            }
        }

        run_frame(vec![key(Key::ArrowDown, Modifiers::NONE)]);
        let cache = context.data(|data| data.get_temp::<Cache>(id)).unwrap();
        let navigation = cache.interaction.navigation.as_ref().unwrap();
        let source = &cache.layout.layouts.caches(0..1).next().unwrap().paragraph;
        assert_eq!(
            cache
                .layout
                .layouts
                .get(0)
                .unwrap()
                .galley
                .layout_from_cursor(CCursor {
                    index: CharIndex(
                        source
                            .scalar_index(navigation.selection.focus.byte)
                            .unwrap()
                    ),
                    prefer_next_row: navigation.prefer_next_row,
                })
                .row,
            1
        );
    }
}

#[test]
fn focused_clipped_editor_scrolls_to_input_and_keeps_ime_geometry() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    scrolling_frame(&context, &mut editor, vec![], false, 500.0);
    let (output, offset, _, visible) = scrolling_frame(
        &context,
        &mut editor,
        vec![Event::Text("typed ".into())],
        true,
        500.0,
    );
    assert!(!visible, "the editor starts entirely below the viewport");
    assert_eq!(editor.document().plain_text(), "typed original");
    assert!(
        offset.y > 400.0,
        "focused input must bring the caret into view: {offset:?}"
    );
    assert!(output.platform_output.ime.is_some());
    let (output, _, viewport, visible) =
        scrolling_frame(&context, &mut editor, vec![], true, 500.0);
    assert!(visible);
    assert!(viewport.contains_rect(output.platform_output.ime.unwrap().cursor_rect));
}

#[test]
fn external_selection_changes_scroll_the_focused_caret_into_view() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&format!("{}end", "line\n".repeat(20)));
    let (_, offset, _, _) = scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    assert_eq!(offset.y, 0.0);
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    let (_, offset, _, _) = scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    assert!(offset.y > 100.0);
    let (output, _, viewport, _) = scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    assert!(viewport.contains_rect(output.platform_output.ime.unwrap().cursor_rect));
    assert!(!editor.can_undo());
}

#[test]
fn growing_preedit_scrolls_after_its_new_height_is_allocated() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    let text = format!("{}end", "line\n".repeat(20));
    let cursor = text.chars().count();
    let (_, offset, _, _) = scrolling_frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text,
            active_range_chars: Some(cursor..cursor),
        })],
        true,
        0.0,
    );
    assert_eq!(editor.document().plain_text(), "");
    assert_eq!(
        offset.y, 0.0,
        "the first allocation precedes the new preedit"
    );
    let cache = context
        .data(|data| data.get_temp::<Cache>(Id::new("scroll editor")))
        .unwrap();
    assert!(cache.interaction.scroll_to_caret);
    let (_, offset, _, _) = scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    assert!(offset.y > 100.0);
    let cache = context
        .data(|data| data.get_temp::<Cache>(Id::new("scroll editor")))
        .unwrap();
    assert!(!cache.interaction.scroll_to_caret);
    let (output, _, viewport, _) = scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    assert!(viewport.contains_rect(output.platform_output.ime.unwrap().cursor_rect));
    assert!(editor.composition().is_some());
    assert!(!editor.can_undo());
}

#[test]
fn ordered_list_reserves_space_for_long_numbers() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("item");
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 0,
            start: Some(1_000_000),
        })
        .expect("valid list");
    context
        .run_ui(egui::RawInput::default(), |ui| {
            let appearance = Appearance {
                font: FontId::proportional(16.0),
                bold_family: None,
                color: Color32::WHITE,
                strong_color: Color32::WHITE,
                code_background: Color32::BLACK,
                link_color: Color32::LIGHT_BLUE,
                width: 400.0,
            };
            let mut cache = Cache::default();
            let layouts = cache.layout(ui, &editor, &appearance);
            let layout = layouts.get(0).unwrap();
            let marker = layout.marker.as_ref().expect("ordered marker");
            assert_eq!(marker.job.text, "1000000.");
            assert!(layouts.get(0).unwrap().marker_position.x >= 0.0);
            assert!(layouts.get(0).unwrap().rect.left() >= marker.size().x + 8.0);
        })
        .drop_without_applying_deltas();
}

#[test]
fn empty_paste_preserves_selection_and_typing_history() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("original");
    editor.insert_text("typed ").unwrap();
    editor.select_all();
    let selection = editor.selection();
    let revision = editor.document().revision();
    frame(
        &context,
        &mut editor,
        vec![Event::Paste(String::new())],
        true,
    );
    assert_eq!(editor.document().plain_text(), "typed original");
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.selection(), selection);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(!editor.can_undo());

    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("a".into()),
            Event::Paste(String::new()),
            Event::Text("b".into()),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "aboriginal");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(!editor.can_undo());

    for preedit in ["", "候"] {
        editor.update_composition(preedit, None).unwrap();
        let composition = editor.composition().cloned();
        frame(
            &context,
            &mut editor,
            vec![Event::Paste(String::new())],
            true,
        );
        assert_eq!(editor.composition(), composition.as_ref());
        assert_eq!(editor.document().plain_text(), "original");
        assert!(!editor.can_undo());
    }
}

#[test]
fn double_click_at_paragraph_end_selects_the_trailing_word() {
    let word_selection = |editor: &Editor, position: Position| {
        let range = editor.document().word_range_at(position).unwrap();
        Selection::new(range.start, range.end)
    };
    for (text, expected) in [
        ("hello café", "café"),
        ("hello 👩🏽‍💻", "👩🏽‍💻"),
        ("hello 🇺🇸🇨🇦", "🇨🇦"),
        ("e\u{301}", "e\u{301}"),
        ("hello ", " "),
    ] {
        let editor = Editor::from_text(text);
        let selection = word_selection(&editor, editor.document().end());
        assert_eq!(
            &text[selection.range().start.byte..selection.range().end.byte],
            expected,
            "{text:?}"
        );
        editor
            .document()
            .validate_position(selection.anchor)
            .unwrap();
        editor
            .document()
            .validate_position(selection.focus)
            .unwrap();
    }
    let cluster = format!("x{}", "\u{301}".repeat(1_024));
    let text = format!("hello {cluster}");
    let editor = Editor::from_text(&text);
    let selection = word_selection(&editor, editor.document().end());
    assert_eq!(
        &text[selection.range().start.byte..selection.range().end.byte],
        cluster
    );
    let editor = Editor::from_text("");
    assert!(word_selection(&editor, editor.document().end()).is_caret());
}

#[test]
fn input_batches_consume_editing_and_preserve_host_events_in_order() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    frame(&context, &mut editor, vec![], true);
    let mut events = Vec::new();
    let mut retained = Vec::new();
    for index in 0..64 {
        events.push(key(Key::ArrowLeft, Modifiers::NONE));
        let mut shortcut = key(Key::F1, Modifiers::NONE);
        if let Event::Key { repeat, .. } = &mut shortcut {
            *repeat = index > 0;
        }
        events.push(shortcut.clone());
        retained.push(shortcut);
        let pointer = Event::PointerMoved(Pos2::new(index as f32, 0.0));
        events.push(pointer.clone());
        retained.push(pointer);
        events.push(Event::Text("x".into()));
    }
    frame(&context, &mut editor, events, true);
    assert_eq!(editor.document().plain_text(), "x".repeat(64));
    assert_eq!(context.input(|input| input.events.clone()), retained);
}

#[test]
fn paste_is_independent_from_adjacent_typing() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("one".into()),
            Event::Paste("two".into()),
            Event::Text("three".into()),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "onetwothree");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "onetwo");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "one");
}

#[test]
fn enter_then_movement_in_same_frame_uses_current_layout() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("first".into()),
            key(Key::Enter, Modifiers::NONE),
            Event::Text("second".into()),
            key(Key::ArrowUp, Modifiers::NONE),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "first\nsecond");
    assert_eq!(editor.selection().focus.paragraph, 0);
}

#[test]
fn typing_then_row_boundary_in_same_frame_uses_current_layout() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("hello".into()),
            key(Key::Home, Modifiers::NONE),
            Event::Text("say ".into()),
            key(Key::End, Modifiers::NONE),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "say hello");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 9)));
    frame(
        &context,
        &mut editor,
        vec![
            key(Key::Home, Modifiers::NONE),
            Event::Text("ok ".into()),
            key(Key::ArrowRight, Modifiers::MAC_CMD | Modifiers::COMMAND),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "ok say hello");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 12)));
}

#[test]
fn enter_then_tab_in_a_list_in_same_frame_indents_the_new_item() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("item");
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(0, 4)))
        .unwrap();
    frame(&context, &mut editor, vec![], true);
    frame(&context, &mut editor, vec![], true);
    frame(
        &context,
        &mut editor,
        vec![
            key(Key::Enter, Modifiers::NONE),
            key(Key::Tab, Modifiers::NONE),
        ],
        true,
    );
    let kinds: Vec<_> = editor
        .document()
        .paragraphs()
        .iter()
        .map(|paragraph| paragraph.kind())
        .collect();
    assert_eq!(
        kinds,
        [
            ParagraphKind::Bullet { indent: 0 },
            ParagraphKind::Bullet { indent: 1 }
        ]
    );
}

#[test]
fn vertical_navigation_keeps_the_column_across_short_paragraphs() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("aaaaaaaaa\nx\naaaaaaaaa");
    editor
        .set_selection(Selection::caret(Position::new(0, 8)))
        .unwrap();
    frame(&context, &mut editor, vec![], true);
    for position in [Position::new(1, 1), Position::new(2, 8)] {
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            true,
        );
        assert_eq!(editor.selection().focus, position);
    }
}

#[test]
fn ignored_keys_and_pending_style_shortcuts_preserve_the_navigation_column() {
    for read_only in [false, true] {
        let mut keys = vec![
            (key(Key::F1, Modifiers::NONE), false),
            (key(Key::A, Modifiers::NONE), false),
            (key(Key::PageDown, Modifiers::COMMAND), false),
            (key(Key::C, Modifiers::COMMAND), false),
            (key(Key::B, Modifiers::COMMAND), true),
            (key(Key::I, Modifiers::COMMAND), true),
            (key(Key::U, Modifiers::COMMAND), true),
        ];
        if read_only {
            keys.extend([
                (key(Key::Backspace, Modifiers::NONE), true),
                (key(Key::Delete, Modifiers::NONE), true),
                (key(Key::Enter, Modifiers::NONE), true),
            ]);
        }
        for (ignored, consumed) in keys {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("aaaaaaaaa\nx\naaaaaaaaa");
            editor
                .set_selection(Selection::caret(Position::new(0, 8)))
                .unwrap();
            frame_options(
                &context,
                &mut editor,
                vec![key(Key::ArrowDown, Modifiers::NONE)],
                true,
                read_only,
            );
            assert_eq!(editor.selection().focus, Position::new(1, 1));
            frame_options(
                &context,
                &mut editor,
                vec![ignored.clone()],
                true,
                read_only,
            );
            assert!(context.input(|input| if consumed {
                input.events.is_empty()
            } else {
                input.events == [ignored.clone()]
            }));
            frame_options(
                &context,
                &mut editor,
                vec![key(Key::ArrowDown, Modifiers::NONE)],
                true,
                read_only,
            );
            assert_eq!(editor.selection().focus, Position::new(2, 8), "{ignored:?}");
        }
    }
}

#[test]
fn moves_and_edits_that_keep_the_selection_reset_the_navigation_column() {
    for stationary in [
        key(Key::ArrowRight, Modifiers::NONE),
        key(Key::ArrowDown, Modifiers::COMMAND),
        key(Key::End, Modifiers::NONE),
        key(Key::Delete, Modifiers::NONE),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("aaaaaaaaa\nx");
        editor
            .set_selection(Selection::caret(Position::new(0, 8)))
            .unwrap();
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowDown, Modifiers::NONE)],
            true,
        );
        assert_eq!(editor.selection().focus, Position::new(1, 1));
        frame(&context, &mut editor, vec![stationary.clone()], true);
        assert_eq!(editor.document().plain_text(), "aaaaaaaaa\nx");
        assert_eq!(
            editor.selection().focus,
            Position::new(1, 1),
            "{stationary:?}"
        );
        frame(
            &context,
            &mut editor,
            vec![key(Key::ArrowUp, Modifiers::NONE)],
            true,
        );
        assert_eq!(
            editor.selection().focus,
            Position::new(0, 1),
            "{stationary:?}"
        );
    }
}

#[test]
fn vertical_navigation_preserves_wrapped_row_affinity_and_horizontal_position() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("aaaaaaaaa x aaaaaaaaa");
    editor
        .set_selection(Selection::caret(Position::new(0, 8)))
        .unwrap();
    let mut cache = Cache::default();
    let mut appearance = appearance();
    appearance.font = FontId::monospace(16.0);
    appearance.width = 100.0;
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    assert_eq!(layouts.get(0).unwrap().galley.rows.len(), 3);
    let mut navigation = None;
    let mut errors = Vec::new();
    for (key, expected_row, expected_byte) in [
        (Key::ArrowDown, 1, 12),
        (Key::ArrowDown, 2, 20),
        (Key::ArrowUp, 1, 12),
        (Key::ArrowUp, 0, 8),
    ] {
        assert!(handle_key(
            &mut editor,
            key,
            None,
            Modifiers::NONE,
            &layouts,
            &mut navigation,
            &mut errors,
            &input_options(false),
        ));
        assert_eq!(editor.selection().focus.byte, expected_byte);
        let navigation = navigation.as_ref().unwrap();
        let cursor = CCursor {
            index: CharIndex(expected_byte),
            prefer_next_row: navigation.prefer_next_row,
        };
        assert_eq!(
            layouts
                .get(0)
                .unwrap()
                .galley
                .layout_from_cursor(cursor)
                .row,
            expected_row
        );
    }
    assert!(errors.is_empty());
}

#[test]
fn vertical_navigation_crosses_combining_graphemes_split_at_indented_wraps() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("previous\na \u{301}bbbbbbbbbbbbbbbbbbbbbbbb");
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::default()))
        .unwrap();
    let mut cache = Cache::default();
    let mut appearance = appearance();
    appearance.font = FontId::proportional(14.0);
    appearance.width = 104.0;
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    let galley = &layouts.get(1).unwrap().galley;
    assert!(galley.rows.len() >= 3);
    assert_eq!(galley.rows[0].char_count_excluding_newline().0, 2);
    let mut navigation = None;
    let mut errors = Vec::new();
    for (key, paragraph, row, byte) in [
        (Key::ArrowDown, 1, 0, Some(0)),
        (Key::ArrowDown, 1, 1, Some("a \u{301}".len())),
        (Key::ArrowDown, 1, 2, None),
        (Key::ArrowUp, 1, 1, Some("a \u{301}".len())),
        (Key::ArrowUp, 1, 0, Some(0)),
        (Key::ArrowUp, 0, 0, Some(0)),
    ] {
        assert!(handle_key(
            &mut editor,
            key,
            None,
            Modifiers::NONE,
            &layouts,
            &mut navigation,
            &mut errors,
            &input_options(false),
        ));
        let focus = editor.selection().focus;
        editor.document().validate_position(focus).unwrap();
        assert_eq!(focus.paragraph, paragraph);
        if let Some(byte) = byte {
            assert_eq!(focus.byte, byte);
        }
        let cursor = CCursor {
            index: CharIndex(
                editor
                    .document()
                    .paragraph(paragraph)
                    .unwrap()
                    .scalar_index(focus.byte)
                    .unwrap(),
            ),
            prefer_next_row: navigation.as_ref().unwrap().prefer_next_row,
        };
        assert_eq!(
            layouts
                .get(paragraph)
                .unwrap()
                .galley
                .layout_from_cursor(cursor)
                .row,
            row
        );
    }
    assert!(errors.is_empty());
}

#[test]
fn vertical_navigation_onto_multi_scalar_graphemes_lands_on_the_visually_nearer_edge() {
    let context = egui::Context::default();
    let narrow = "i".repeat(16);
    let mut editor = Editor::from_text(&format!("{narrow}\na🇺🇸b\n{narrow}"));
    let layouts = cached_layout_frame(&context, &mut Cache::default(), &editor, &appearance());
    let x = |paragraph, byte| {
        document_caret_rect(
            editor.document(),
            &layouts,
            Pos2::ZERO,
            Position::new(paragraph, byte),
            true,
        )
        .center()
        .x
    };
    let (left, right) = (x(1, 1), x(1, 9));
    let mut moves = Vec::new();
    for (paragraph, key) in [(0, Key::ArrowDown), (2, Key::ArrowUp)] {
        for (from, to, expected) in [(0.1, 0.4, 1), (0.6, 0.9, 9)] {
            let column = (0..=narrow.len())
                .find(|&byte| {
                    let x = x(paragraph, byte);
                    x > left + from * (right - left) && x < left + to * (right - left)
                })
                .expect("a narrow column over the flag");
            moves.push((paragraph, column, key, expected));
        }
    }
    for (paragraph, column, key, expected) in moves {
        editor
            .set_selection(Selection::caret(Position::new(paragraph, column)))
            .unwrap();
        let mut navigation = None;
        let mut errors = Vec::new();
        assert!(handle_key(
            &mut editor,
            key,
            None,
            Modifiers::NONE,
            &layouts,
            &mut navigation,
            &mut errors,
            &input_options(false),
        ));
        assert!(errors.is_empty());
        assert_eq!(
            editor.selection().focus,
            Position::new(1, expected),
            "{key:?} from column {column}"
        );
    }
}

#[test]
fn vertical_navigation_roundtrips_graphemes_spanning_entire_visual_rows() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&format!("a {} end", "👨\u{200d}".repeat(8)));
    let source = editor.document().paragraphs()[0].clone();
    let mut cache = Cache::default();
    let mut appearance = appearance();
    appearance.font = FontId::proportional(14.0);
    appearance.width = 32.0;
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
    let galley = &layouts.get(0).unwrap().galley;
    assert_eq!(galley.rows.len(), 6);
    let mut navigation = None;
    let mut errors = Vec::new();
    let mut previous_row = 0;
    for (key, expected_row, scalar) in [
        (Key::ArrowDown, 1, 2),
        (Key::ArrowDown, 4, 18),
        (Key::ArrowDown, 5, 19),
        (Key::ArrowUp, 4, 18),
        (Key::ArrowUp, 1, 2),
        (Key::ArrowUp, 0, 0),
    ] {
        assert!(handle_key(
            &mut editor,
            key,
            None,
            Modifiers::NONE,
            &layouts,
            &mut navigation,
            &mut errors,
            &input_options(false),
        ));
        let focus = editor.selection().focus;
        editor.document().validate_position(focus).unwrap();
        assert_eq!(
            focus,
            Position::new(0, source.byte_from_scalar(scalar).unwrap())
        );
        let cursor = CCursor {
            index: CharIndex(scalar),
            prefer_next_row: navigation.as_ref().unwrap().prefer_next_row,
        };
        let row = galley.layout_from_cursor(cursor).row;
        assert_eq!(row, expected_row);
        assert!(if key == Key::ArrowDown {
            row > previous_row
        } else {
            row < previous_row
        });
        previous_row = row;
    }
    assert!(errors.is_empty());
    assert_eq!(editor.selection(), Selection::caret(Position::default()));
}

#[test]
fn read_only_vertical_selection_extends_and_external_or_horizontal_movement_resets_column() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("aaaaaaaaa\nx\naaaaaaaaa");
    let anchor = Position::new(0, 8);
    editor.set_selection(Selection::caret(anchor)).unwrap();
    for focus in [Position::new(1, 1), Position::new(2, 8)] {
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::ArrowDown, Modifiers::SHIFT)],
            true,
            true,
        );
        assert_eq!(editor.selection(), Selection::new(anchor, focus));
    }
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    frame_options(
        &context,
        &mut editor,
        vec![key(Key::ArrowDown, Modifiers::NONE)],
        true,
        true,
    );
    assert_eq!(editor.selection().focus, Position::new(2, 0));

    editor.set_selection(Selection::caret(anchor)).unwrap();
    frame_options(
        &context,
        &mut editor,
        vec![
            key(Key::ArrowDown, Modifiers::NONE),
            key(Key::ArrowLeft, Modifiers::NONE),
            key(Key::ArrowDown, Modifiers::NONE),
        ],
        true,
        true,
    );
    assert_eq!(editor.selection().focus, Position::new(2, 0));
    assert_eq!(editor.document().plain_text(), "aaaaaaaaa\nx\naaaaaaaaa");
    assert!(!editor.can_undo());
}

#[test]
fn empty_preedit_then_empty_commit_deletes_replacement() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("selected");
    editor.select_all();
    frame(
        &context,
        &mut editor,
        vec![
            Event::Ime(ImeEvent::Preedit {
                text: "candidate".into(),
                active_range_chars: None,
            }),
            Event::Ime(ImeEvent::Preedit {
                text: "".into(),
                active_range_chars: None,
            }),
            Event::Ime(ImeEvent::Commit("".into())),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "selected");
}

#[test]
fn empty_preedit_restores_visible_selection_before_a_later_empty_commit() {
    for backward in [false, true] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("selected 色\nsecond");
        editor.select_all();
        if backward {
            let selection = editor.selection();
            editor
                .set_selection(Selection::new(selection.focus, selection.anchor))
                .unwrap();
        }
        let selection = editor.selection();
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "candidate".into(),
                active_range_chars: None,
            })],
            true,
        );
        let output = frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None,
            })],
            true,
        );
        let painted_text: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(painted_text, ["selected 色", "second"]);
        assert_eq!(editor.selection(), selection);
        assert_eq!(editor.composition().unwrap().replacement, selection);
        assert!(!editor.can_undo());
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Commit(String::new()))],
            true,
        );
        assert_eq!(editor.document().plain_text(), "");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "selected 色\nsecond");
        assert_eq!(editor.selection(), selection);
    }
}

#[test]
fn canceled_empty_preedit_allows_typing_to_resume() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    frame(
        &context,
        &mut editor,
        vec![
            Event::Ime(ImeEvent::Preedit {
                text: "候".into(),
                active_range_chars: None,
            }),
            Event::Ime(ImeEvent::Preedit {
                text: "".into(),
                active_range_chars: None,
            }),
            Event::Text("text".into()),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "text");
    assert!(editor.composition().is_none());
}

#[test]
fn ime_preview_replaces_selected_text_reflows_and_reuses_document() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("before OLD after");
    editor
        .set_selection(Selection::new(Position::new(0, 7), Position::new(0, 10)))
        .expect("valid selection");
    let revision = editor.document().revision();
    let preedit = "candidate ".repeat(80);
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: preedit.clone(),
            active_range_chars: None,
        })],
        true,
    );
    let id = context
        .memory(|memory| memory.focused())
        .expect("focused editor");
    let first = context
        .data(|data| data.get_temp::<Cache>(id))
        .expect("cached widget");
    let preview = first.preview.expect("transient composition document");
    assert_eq!(
        preview.document.plain_text(),
        format!("before {preedit} after")
    );
    assert!(!preview.document.plain_text().contains("OLD"));
    assert_eq!(editor.document().plain_text(), "before OLD after");
    assert_eq!(editor.document().revision(), revision);
    assert!(!editor.can_undo());
    assert!(
        preview
            .render_cache
            .layouts
            .get(0)
            .unwrap()
            .galley
            .job
            .sections
            .iter()
            .any(|section| section.format.underline.width > 0.0)
    );
    let preview_paragraph = preview.document.paragraphs()[0].clone();
    frame(&context, &mut editor, vec![], true);
    let second = context
        .data(|data| data.get_temp::<Cache>(id))
        .expect("cached widget");
    assert!(Arc::ptr_eq(
        &preview_paragraph,
        &second
            .preview
            .expect("retained preview")
            .document
            .paragraphs()[0]
    ));
    assert_eq!(editor.document().revision(), revision);
}

#[test]
fn ime_cursor_is_hidden_when_preedit_range_is_none() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("");
    let hidden = frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "に".into(),
            active_range_chars: None,
        })],
        true,
    );
    let vertical_lines = |output: &egui::FullOutput| {
        output.shapes.iter().filter(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::LineSegment { points, .. } if points[0].x == points[1].x)
        }).count()
    };
    assert_eq!(vertical_lines(&hidden), 0);
    let visible = frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "に".into(),
            active_range_chars: Some(1..1),
        })],
        true,
    );
    assert_eq!(vertical_lines(&visible), 1);
}

#[test]
fn surrounding_deletion_expands_graphemes_and_undo_restores_caret() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("e\u{301}!\nnext");
    let original = Selection::caret(Position::new(0, "e\u{301}".len()));
    editor.set_selection(original).expect("valid caret");
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars: 1,
            after_chars: 0,
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "!\nnext");
    assert!(editor.undo());
    assert_eq!(editor.selection(), original);
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .expect("valid paragraph caret");
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars: 1,
            after_chars: 0,
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "e\u{301}!next");
}

#[test]
fn surrounding_offsets_cross_empty_paragraphs_and_clamp_large_counts() {
    let editor = Editor::from_text("aé\n\ne\u{301}👩🏽‍💻!\nlast");
    let document = editor.document();
    let focus = Position::new(2, "e\u{301}👩🏽‍💻".len());
    for (count, expected) in [
        (0, focus),
        (1, Position::new(2, "e\u{301}".len())),
        (4, Position::new(2, "e\u{301}".len())),
        (5, Position::new(2, 0)),
        (6, Position::new(2, 0)),
        (7, Position::new(1, 0)),
        (8, Position::new(0, "aé".len())),
        (9, Position::new(0, 1)),
        (usize::MAX, Position::default()),
    ] {
        assert_eq!(
            surrounding_position(document, focus, count, false),
            expected
        );
    }
    let focus = Position::new(0, 1);
    for (count, expected) in [
        (0, focus),
        (1, Position::new(0, "aé".len())),
        (2, Position::new(1, 0)),
        (3, Position::new(2, 0)),
        (4, Position::new(2, "e\u{301}".len())),
        (5, Position::new(2, "e\u{301}".len())),
        (6, Position::new(2, "e\u{301}👩🏽‍💻".len())),
        (usize::MAX, document.end()),
    ] {
        assert_eq!(surrounding_position(document, focus, count, true), expected);
    }
}

#[test]
fn surrounding_deletion_uses_selection_focus_and_restores_it_on_undo() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("prefix\n\né!\nsuffix");
    let selection = Selection::new(Position::new(3, 3), Position::new(2, 0));
    editor.set_selection(selection).unwrap();
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::DeleteSurrounding {
            before_chars: 2,
            after_chars: 1,
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "prefix!\nsuffix");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "prefix\n\né!\nsuffix");
    assert_eq!(editor.selection(), selection);
}

#[test]
fn accessibility_selection_preserves_requested_wrap_affinity() {
    use egui::accesskit::{
        Action, ActionData, ActionRequest, Role, TextPosition, TextSelection, TreeId,
    };

    fn narrow(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
        widget.desired_width(120.0)
    }
    fn narrow_read_only(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
        narrow(widget).read_only(true)
    }
    for configure in [narrow, narrow_read_only] {
        let context = egui::Context::default();
        context.enable_accesskit();
        context.global_style_mut(|style| {
            style.visuals.text_cursor.blink = true;
            style.visuals.text_cursor.on_duration = 0.5;
            style.visuals.text_cursor.off_duration = 0.5;
        });
        let text = "first second third fourth fifth sixth seventh";
        let mut editor = Editor::from_text(text);
        let mut time = 10.0;
        let (output, widget) =
            widget_frame(&context, &mut editor, vec![], Some(time), true, configure);
        let cache = widget_cache(&context);
        let layout = cache.layout.layouts.get(0).unwrap();
        let seam = Position::new(0, layout.galley.rows[0].char_count_including_newline().0);
        let update = output.platform_output.accesskit_update.unwrap();
        let (run_id, run) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == Role::TextRun && node.value() == Some(&text[..seam.byte])
            })
            .unwrap();
        let upper = TextPosition {
            node: *run_id,
            character_index: run.character_lengths().len(),
        };
        let lower = cache
            .accessibility
            .as_ref()
            .unwrap()
            .position(seam)
            .unwrap();
        assert_ne!(upper, lower);
        let select = |position| {
            Event::AccessKitActionRequest(ActionRequest {
                action: Action::SetTextSelection,
                target_node: Id::new(WIDGET_ID).accesskit_id(),
                target_tree: TreeId::ROOT,
                data: Some(ActionData::SetTextSelection(TextSelection {
                    anchor: position,
                    focus: position,
                })),
            })
        };
        for (position, prefer_next_row) in [(upper, false), (lower, true), (upper, false)] {
            let (idle, _) = widget_frame(
                &context,
                &mut editor,
                vec![],
                Some(time + 0.7),
                true,
                configure,
            );
            assert_eq!(caret_segments(&idle), 0);
            time += 0.8;
            let (output, actual) = widget_frame(
                &context,
                &mut editor,
                vec![select(position)],
                Some(time),
                true,
                configure,
            );
            assert_eq!(editor.selection(), Selection::caret(seam));
            assert_eq!(caret_segments(&output), 1, "moved caret must be visible");
            let expected = document_caret_rect(
                editor.document(),
                &cache.layout.layouts,
                widget.response.rect.min + Vec2::splat(PADDING),
                seam,
                prefer_next_row,
            );
            assert_eq!(actual.caret_rect, expected);
            let update = output.platform_output.accesskit_update.unwrap();
            let node = &update
                .nodes
                .iter()
                .find(|(id, _)| *id == Id::new(WIDGET_ID).accesskit_id())
                .unwrap()
                .1;
            assert_eq!(node.text_selection().unwrap().focus, position);
        }
        widget_frame(
            &context,
            &mut editor,
            vec![
                select(lower),
                select(upper),
                key(Key::ArrowDown, Modifiers::NONE),
            ],
            None,
            true,
            configure,
        );
        let cache = widget_cache(&context);
        let navigation = cache.interaction.navigation.as_ref().unwrap();
        let layout = cache.layout.layouts.get(0).unwrap();
        assert_eq!(
            layout
                .galley
                .layout_from_cursor(CCursor {
                    index: CharIndex(editor.selection().focus.byte),
                    prefer_next_row: navigation.prefer_next_row,
                })
                .row,
            1
        );
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn accessibility_exports_text_runs_and_applies_selection_action() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role, TextSelection, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("first\nsecond");
    let output = frame(&context, &mut editor, vec![], true);
    let update = output
        .platform_output
        .accesskit_update
        .expect("accessibility tree");
    let (root_id, node) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .expect("editor node");
    let caret = node.text_selection().expect("native text selection").focus;
    assert!(
        update
            .nodes
            .iter()
            .any(|(id, node)| *id == caret.node && node.role() == Role::TextRun)
    );
    let second_run = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun && node.value() == Some("second"))
        .expect("second paragraph");
    let focus = egui::accesskit::TextPosition {
        node: second_run.0,
        character_index: 3,
    };
    frame(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetTextSelection,
            target_node: *root_id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::SetTextSelection(TextSelection {
                anchor: caret,
                focus,
            })),
        })],
        true,
    );
    assert_eq!(editor.selection().focus, Position::new(1, 3));
    assert_eq!(editor.selected_text(), "first\nsec");
}

#[test]
fn accessibility_emoji_selection_and_replacement_use_graphemes() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role, TextSelection, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("a👩🏽‍💻e\u{301}");
    let output = frame(&context, &mut editor, vec![], true);
    let update = output
        .platform_output
        .accesskit_update
        .expect("accessibility tree");
    let (root_id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .expect("editor node");
    let (run_id, run) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::TextRun)
        .expect("grapheme run");
    assert_eq!(run.character_lengths(), &[1, 15, 3]);
    let anchor = egui::accesskit::TextPosition {
        node: *run_id,
        character_index: 1,
    };
    let focus = egui::accesskit::TextPosition {
        node: *run_id,
        character_index: 2,
    };
    frame(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetTextSelection,
            target_node: *root_id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::SetTextSelection(TextSelection {
                anchor,
                focus,
            })),
        })],
        true,
    );
    assert_eq!(editor.selected_text(), "👩🏽‍💻");
    frame(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::ReplaceSelectedText,
            target_node: *root_id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::Value("x".into())),
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "axe\u{301}");
    assert!(editor.undo());
    assert_eq!(editor.selected_text(), "👩🏽‍💻");
}

#[test]
fn accessibility_set_value_undo_restores_the_previous_selection() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("aé\nhello");
    let selection = Selection::new(Position::new(1, 2), Position::new(0, 1));
    editor.set_selection(selection).unwrap();
    let output = frame(&context, &mut editor, vec![], true);
    let update = output.platform_output.accesskit_update.unwrap();
    let (root_id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .unwrap();
    frame(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_node: *root_id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::Value("replacement".into())),
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "replacement");
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "aé\nhello");
    assert_eq!(editor.selection(), selection);
    assert!(!editor.can_undo());
}

#[test]
fn accessibility_set_value_uses_the_document_start_style() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("plain bold");
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 10)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    let selection = editor.selection();
    assert!(editor.typing_style().bold);
    let output = frame(&context, &mut editor, vec![], true);
    let update = output.platform_output.accesskit_update.unwrap();
    let (root_id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .unwrap();
    frame(
        &context,
        &mut editor,
        vec![Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target_node: *root_id,
            target_tree: TreeId::ROOT,
            data: Some(ActionData::Value("replacement".into())),
        })],
        true,
    );
    assert_eq!(editor.document().plain_text(), "replacement");
    assert!(
        editor
            .document()
            .paragraph(0)
            .unwrap()
            .spans()
            .iter()
            .all(|span| !span.style.bold)
    );
    assert!(!editor.typing_style().bold);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "plain bold");
    assert_eq!(editor.selection(), selection);
    assert!(editor.typing_style().bold);
}

#[test]
fn malformed_and_stale_accessibility_actions_are_consumed_without_edits() {
    use egui::accesskit::{Action, ActionData, ActionRequest, Role, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("original");
    editor.select_all();
    let output = frame(&context, &mut editor, vec![], false);
    let update = output.platform_output.accesskit_update.unwrap();
    let (root_id, _) = update
        .nodes
        .iter()
        .find(|(_, node)| node.role() == Role::MultilineTextInput)
        .unwrap();
    let run_action = |editor: &mut Editor, action, data, expected: AccessibilityError| {
        let input = egui::RawInput {
            events: vec![Event::AccessKitActionRequest(ActionRequest {
                action,
                target_node: *root_id,
                target_tree: TreeId::ROOT,
                data,
            })],
            ..Default::default()
        };
        context
            .run_ui(input, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let id = ui.make_persistent_id("editor");
                    let output = RichTextEditor::new(editor).id(id).show(ui);
                    assert!(output.errors.is_empty());
                    assert_eq!(output.accessibility_errors, vec![expected.clone()]);
                    assert!(ui.input(|input| input.events.is_empty()));
                });
            })
            .drop_without_applying_deltas();
    };
    for (action, data) in [
        (Action::SetValue, None),
        (Action::ReplaceSelectedText, None),
        (
            Action::SetTextSelection,
            Some(ActionData::Value("wrong".into())),
        ),
    ] {
        run_action(
            &mut editor,
            action,
            data,
            AccessibilityError::InvalidActionData,
        );
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selected_text(), "original");
        assert!(!editor.can_undo());
    }
    editor.insert_text("external").unwrap();
    let revision = editor.document().revision();
    let selection = editor.selection();
    run_action(
        &mut editor,
        Action::SetValue,
        Some(ActionData::Value("replacement".into())),
        AccessibilityError::StaleDocument,
    );
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.document().plain_text(), "external");
    assert_eq!(editor.selection(), selection);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "original");
    assert!(!editor.can_undo());
}

#[test]
fn accessibility_and_keyboard_input_follow_their_batch_order() {
    use egui::accesskit::{Action, ActionData, ActionRequest, TextSelection, TreeId};
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("original");
    frame(&context, &mut editor, vec![], true);
    let id = context.memory(|memory| memory.focused()).unwrap();
    let snapshot = context
        .data(|data| data.get_temp::<Cache>(id))
        .unwrap()
        .accessibility
        .unwrap();
    let end = snapshot.position(editor.document().end()).unwrap();
    let move_to_end = Event::AccessKitActionRequest(ActionRequest {
        action: Action::SetTextSelection,
        target_node: id.accesskit_id(),
        target_tree: TreeId::ROOT,
        data: Some(ActionData::SetTextSelection(TextSelection {
            anchor: end,
            focus: end,
        })),
    });
    let unrelated = key(Key::F1, Modifiers::NONE);
    let output = context.run_ui(
        egui::RawInput {
            events: vec![
                Event::Text("typed ".into()),
                move_to_end.clone(),
                unrelated.clone(),
            ],
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let id = ui.make_persistent_id("editor");
                let output = RichTextEditor::new(&mut editor).id(id).show(ui);
                assert!(output.errors.is_empty());
                assert_eq!(
                    output.accessibility_errors,
                    vec![AccessibilityError::StaleDocument]
                );
            });
        },
    );
    output.drop_without_applying_deltas();
    assert_eq!(editor.document().plain_text(), "typed original");
    assert_eq!(editor.selection().focus, Position::new(0, 6));
    assert_eq!(context.input(|input| input.events.clone()), vec![unrelated]);
    assert!(editor.undo());
    frame(&context, &mut editor, vec![], true);

    frame(
        &context,
        &mut editor,
        vec![move_to_end, Event::Text(" typed".into())],
        true,
    );
    assert_eq!(editor.document().plain_text(), "original typed");
    assert_eq!(editor.selection().focus, editor.document().end());
    assert!(context.input(|input| input.events.is_empty()));
}

const WIDGET_ID: &str = "widget editor";

fn widget_frame(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    time: Option<f64>,
    focus: bool,
    configure: fn(RichTextEditor<'_>) -> RichTextEditor<'_>,
) -> (egui::FullOutput, RichTextEditorOutput) {
    let id = Id::new(WIDGET_ID);
    let mut widget = None;
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
            events,
            time,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                if focus {
                    ui.memory_mut(|memory| {
                        if !memory.has_focus(id) {
                            memory.request_focus(id);
                        }
                    });
                }
                let output = configure(RichTextEditor::new(editor).id(id)).show(ui);
                assert!(output.errors.is_empty(), "{:?}", output.errors);
                assert!(output.accessibility_errors.is_empty());
                widget = Some(output);
                let _ = ui.button("next");
            });
        },
    );
    output.textures_delta.clear();
    (output, widget.expect("widget shown"))
}

fn unchanged(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
    widget
}

fn widget_cache(context: &egui::Context) -> Cache {
    context
        .data(|data| data.get_temp::<Cache>(Id::new(WIDGET_ID)))
        .expect("cached widget")
}

fn primary(pos: Pos2, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    }
}

fn caret_segments(output: &egui::FullOutput) -> usize {
    output
        .shapes
        .iter()
        .filter(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::LineSegment { points, .. }
                if points[0].x == points[1].x)
        })
        .count()
}

fn caret_point(context: &egui::Context, rect: Rect, editor: &Editor, position: Position) -> Pos2 {
    let cache = widget_cache(context);
    let (document, layouts) = cache
        .preview
        .as_ref()
        .map_or((editor.document(), &cache.layout.layouts), |preview| {
            (&preview.document, &preview.render_cache.layouts)
        });
    document_caret_rect(
        document,
        layouts,
        rect.min + Vec2::splat(PADDING),
        position,
        true,
    )
    .center()
        + Vec2::new(1.0, 0.0)
}

#[test]
fn ime_area_stays_next_to_the_caret_and_its_preedit_in_long_documents() {
    let context = egui::Context::default();
    let text = (0..200)
        .map(|index| format!("paragraph {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut editor = Editor::from_text(&text);
    editor
        .set_selection(Selection::caret(Position::new(150, 3)))
        .unwrap();
    let id = Id::new("ime area");
    let run = |editor: &mut Editor, events| {
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                        ui.memory_mut(|memory| memory.request_focus(id));
                        RichTextEditor::new(editor).id(id).show(ui);
                    });
                });
            },
        );
        output.textures_delta.clear();
        output
            .platform_output
            .ime
            .expect("focused editable IME output")
    };
    for _ in 0..3 {
        run(&mut editor, Vec::new());
    }
    let ime = run(&mut editor, Vec::new());
    assert_eq!(ime.rect, ime.cursor_rect, "{ime:?}");
    assert!(ime.rect.height() < 40.0, "{ime:?}");
    assert!(
        ime.rect.top() >= 0.0 && ime.rect.bottom() <= 400.0,
        "{ime:?}"
    );

    let ime = run(
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "にほん".into(),
            active_range_chars: Some(3..3),
        })],
    );
    let ime = if ime.rect == ime.cursor_rect {
        run(&mut editor, Vec::new())
    } else {
        ime
    };
    assert!(ime.rect.contains_rect(ime.cursor_rect), "{ime:?}");
    assert_eq!(ime.rect.right(), ime.cursor_rect.right());
    assert!(
        ime.rect.width() > 20.0,
        "the area covers the preedit: {ime:?}"
    );
    assert_eq!(ime.rect.height(), ime.cursor_rect.height());
}

#[test]
fn first_preedit_shares_untouched_galleys_with_the_committed_layout() {
    for (preedit, shifted) in [("あ", 0), ("x\ny", 1)] {
        let context = egui::Context::default();
        let mut cache = Cache::default();
        let mut editor = Editor::from_text("first\nOLD\nlast\ntail");
        let appearance = appearance();
        let committed = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        cached_layout_frame(&context, &mut cache, &editor, &appearance);
        editor
            .set_selection(Selection::new(Position::new(1, 0), Position::new(1, 3)))
            .unwrap();
        editor.update_composition(preedit, None).unwrap();
        let preview = cached_preview_frame(&context, &mut cache, &editor, &appearance);
        assert_eq!(preview.len(), 4 + shifted);
        for (source, displayed) in [(0, 0), (2, 2 + shifted), (3, 3 + shifted)] {
            assert!(
                Arc::ptr_eq(
                    &committed.get(source).unwrap().galley,
                    &preview.get(displayed).unwrap().galley
                ),
                "{preedit:?}: paragraph {source} was shaped again"
            );
        }
    }
}

#[test]
fn pointer_press_places_the_caret_and_a_drag_extends_from_the_press_point() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
    let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), false, unchanged);
    let press = caret_point(&context, widget.response.rect, &editor, Position::new(0, 6));
    widget_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(press),
            primary(press, true, Modifiers::NONE),
        ],
        Some(0.1),
        false,
        unchanged,
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 6)));
    assert!(context.memory(|memory| memory.has_focus(Id::new(WIDGET_ID))));
    let release = press + Vec2::new(2.0, 0.0);
    widget_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(release),
            primary(release, false, Modifiers::NONE),
        ],
        Some(0.15),
        false,
        unchanged,
    );
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, 6)),
        "release jitter keeps the pressed caret"
    );

    editor
        .set_selection(Selection::caret(Position::default()))
        .unwrap();
    let end = caret_point(
        &context,
        widget.response.rect,
        &editor,
        Position::new(0, 13),
    );
    for (index, events) in [
        vec![primary(press, true, Modifiers::NONE)],
        vec![Event::PointerMoved(press + Vec2::new(12.0, 0.0))],
        vec![Event::PointerMoved(end)],
        vec![primary(end, false, Modifiers::NONE)],
    ]
    .into_iter()
    .enumerate()
    {
        widget_frame(
            &context,
            &mut editor,
            events,
            Some(2.0 + index as f64 * 0.05),
            false,
            unchanged,
        );
    }
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 6), Position::new(0, 13))
    );

    let selection = editor.selection();
    let secondary = |pos, pressed| Event::PointerButton {
        pos,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    for (index, events) in [
        vec![Event::PointerMoved(press), secondary(press, true)],
        vec![Event::PointerMoved(press + Vec2::new(40.0, 0.0))],
        vec![secondary(press + Vec2::new(40.0, 0.0), false)],
    ]
    .into_iter()
    .enumerate()
    {
        widget_frame(
            &context,
            &mut editor,
            events,
            Some(4.0 + index as f64 * 0.05),
            false,
            unchanged,
        );
    }
    assert_eq!(
        editor.selection(),
        selection,
        "secondary drags do not select"
    );
}

#[test]
fn a_drag_batched_with_its_press_keeps_the_original_anchor() {
    for transform in [
        egui::emath::TSTransform::IDENTITY,
        egui::emath::TSTransform::new(Vec2::new(25.0, 40.0), 1.5),
    ] {
        let context = egui::Context::default();
        context.set_transform_layer(egui::LayerId::background(), transform);
        let mut editor = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
        let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), false, unchanged);
        let point = |byte| {
            transform
                * caret_point(
                    &context,
                    widget.response.rect,
                    &editor,
                    Position::new(0, byte),
                )
        };
        let (press, moved, end) = (point(3), point(13), point(19));
        widget_frame(
            &context,
            &mut editor,
            vec![
                Event::PointerMoved(press),
                primary(press, true, Modifiers::NONE),
                Event::PointerMoved(moved),
            ],
            Some(0.1),
            false,
            unchanged,
        );
        assert_eq!(editor.selection(), Selection::caret(Position::new(0, 3)),);
        widget_frame(
            &context,
            &mut editor,
            vec![Event::PointerMoved(end)],
            Some(0.2),
            false,
            unchanged,
        );
        assert_eq!(
            editor.selection(),
            Selection::new(Position::new(0, 3), Position::new(0, 19)),
        );
    }
}

#[test]
fn shift_click_uses_the_modifiers_at_the_press() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
    editor
        .set_selection(Selection::caret(Position::new(0, 2)))
        .unwrap();
    let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), true, unchanged);
    let point = caret_point(
        &context,
        widget.response.rect,
        &editor,
        Position::new(0, 13),
    );
    widget_frame(
        &context,
        &mut editor,
        vec![
            Event::ModifiersChanged(Modifiers::SHIFT),
            Event::PointerMoved(point),
            primary(point, true, Modifiers::SHIFT),
            Event::ModifiersChanged(Modifiers::NONE),
        ],
        Some(0.1),
        false,
        unchanged,
    );
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 2), Position::new(0, 13)),
    );
}

#[test]
fn a_drag_finishes_at_its_release_before_later_pointer_motion() {
    for transform in [
        egui::emath::TSTransform::IDENTITY,
        egui::emath::TSTransform::new(Vec2::new(25.0, 40.0), 1.5),
    ] {
        for pointer_gone in [false, true] {
            let context = egui::Context::default();
            context.set_transform_layer(egui::LayerId::background(), transform);
            let mut editor = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
            let (_, widget) =
                widget_frame(&context, &mut editor, vec![], Some(0.0), false, unchanged);
            let point = |byte| {
                transform
                    * caret_point(
                        &context,
                        widget.response.rect,
                        &editor,
                        Position::new(0, byte),
                    )
            };
            let (press, moved, release, after) = (point(3), point(13), point(19), point(23));
            for (index, events) in [
                vec![
                    Event::PointerMoved(press),
                    primary(press, true, Modifiers::NONE),
                ],
                vec![Event::PointerMoved(moved)],
                vec![
                    Event::PointerMoved(release),
                    primary(release, false, Modifiers::NONE),
                    if pointer_gone {
                        Event::PointerGone
                    } else {
                        Event::PointerMoved(after)
                    },
                ],
            ]
            .into_iter()
            .enumerate()
            {
                widget_frame(
                    &context,
                    &mut editor,
                    events,
                    Some(0.1 + index as f64 * 0.1),
                    false,
                    unchanged,
                );
            }
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(0, 3), Position::new(0, 19)),
            );
        }
    }
}

#[test]
fn a_drag_release_is_not_replaced_by_a_later_click_in_another_editor() {
    two_editors_pointer_gestures(true);
}

#[test]
fn a_click_press_is_not_replaced_by_a_later_press_in_another_editor() {
    two_editors_pointer_gestures(false);
}

fn two_editors_pointer_gestures(drag: bool) {
    let context = egui::Context::default();
    let mut first = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
    let mut second = Editor::from_text("abcdefghijklmnopqrstuvwxyz");
    let run = |first: &mut Editor, second: &mut Editor, events, time| {
        let mut rectangles = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
                events,
                time: Some(time),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    let a = RichTextEditor::new(first).id(Id::new(WIDGET_ID)).show(ui);
                    let b = RichTextEditor::new(second)
                        .id(Id::new("second editor"))
                        .show(ui);
                    assert!(a.errors.is_empty(), "{:?}", a.errors);
                    assert!(b.errors.is_empty(), "{:?}", b.errors);
                    rectangles = Some((a.response.rect, b.response.rect));
                });
            },
        );
        output.textures_delta.clear();
        rectangles.unwrap()
    };
    let (first_rect, second_rect) = run(&mut first, &mut second, vec![], 0.0);
    let point = |byte| caret_point(&context, first_rect, &first, Position::new(0, byte));
    let (press, moved, release) = (point(3), point(13), point(19));
    let second_point = press + second_rect.min.to_vec2() - first_rect.min.to_vec2();
    let batches = if drag {
        vec![
            vec![
                Event::PointerMoved(press),
                primary(press, true, Modifiers::NONE),
            ],
            vec![Event::PointerMoved(moved)],
            vec![
                Event::PointerMoved(release),
                primary(release, false, Modifiers::NONE),
                Event::PointerMoved(second_point),
                primary(second_point, true, Modifiers::NONE),
                primary(second_point, false, Modifiers::NONE),
            ],
        ]
    } else {
        vec![
            vec![
                Event::PointerMoved(press),
                primary(press, true, Modifiers::NONE),
            ],
            vec![
                primary(press, false, Modifiers::NONE),
                Event::PointerMoved(second_point),
                primary(second_point, true, Modifiers::NONE),
            ],
        ]
    };
    for (index, events) in batches.into_iter().enumerate() {
        run(&mut first, &mut second, events, 0.1 + index as f64 * 0.1);
    }
    assert_eq!(
        first.selection(),
        Selection::new(
            Position::new(0, 3),
            Position::new(0, if drag { 19 } else { 3 })
        ),
    );
    if drag {
        assert_eq!(second.selection(), Selection::caret(Position::new(0, 3)));
    }
}

#[test]
fn space_and_enter_are_text_input_not_clicks_or_ime_interruptions() {
    for (key, text) in [(Key::Space, Some(" ")), (Key::Enter, None)] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("hello");
        widget_frame(&context, &mut editor, vec![], None, true, unchanged);
        widget_frame(&context, &mut editor, vec![], None, true, unchanged);
        let mut events = vec![self::key(key, Modifiers::NONE)];
        events.extend(text.map(|text| Event::Text(text.into())));
        let (output, widget) = widget_frame(&context, &mut editor, events, None, true, unchanged);
        assert!(!widget.response.clicked(), "{key:?}");
        assert!(
            !output
                .platform_output
                .ime
                .unwrap()
                .should_interrupt_composition
        );
        assert!(
            !output
                .platform_output
                .events
                .iter()
                .any(|event| matches!(event, egui::output::OutputEvent::Clicked(_)))
        );
        assert_ne!(editor.document().plain_text(), "hello");
    }
}

#[test]
fn home_and_end_follow_wrapped_visual_rows_and_their_affinity() {
    fn narrow(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
        widget.desired_width(140.0)
    }
    let context = egui::Context::default();
    let text = "first second third fourth fifth sixth seventh eighth ninth tenth eleventh";
    let mut editor = Editor::from_text(text);
    widget_frame(&context, &mut editor, vec![], None, true, narrow);
    let cache = widget_cache(&context);
    let galley = cache.layout.layouts.get(0).unwrap().galley.clone();
    assert!(galley.rows.len() >= 3, "{}", galley.rows.len());
    let starts: Vec<usize> = galley
        .rows
        .iter()
        .scan(0, |start, row| {
            let row_start = *start;
            *start += row.char_count_including_newline().0;
            Some(row_start)
        })
        .collect();
    let row_of = |context: &egui::Context, editor: &Editor| {
        let cache = widget_cache(context);
        let navigation = cache.interaction.navigation.expect("visual affinity");
        assert_eq!(navigation.selection, editor.selection());
        galley
            .layout_from_cursor(CCursor {
                index: CharIndex(editor.selection().focus.byte),
                prefer_next_row: navigation.prefer_next_row,
            })
            .row
    };
    let last = starts.len() - 1;
    let press = |editor: &mut Editor, key, modifiers| {
        widget_frame(
            &context,
            editor,
            vec![self::key(key, modifiers)],
            None,
            true,
            narrow,
        );
    };
    editor
        .set_selection(Selection::caret(Position::new(0, starts[last] + 2)))
        .unwrap();
    press(&mut editor, Key::Home, Modifiers::NONE);
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, starts[last]))
    );
    assert_eq!(row_of(&context, &editor), last);
    press(&mut editor, Key::End, Modifiers::NONE);
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, text.len()))
    );

    editor
        .set_selection(Selection::caret(Position::new(0, starts[1] + 2)))
        .unwrap();
    press(&mut editor, Key::End, Modifiers::NONE);
    assert_eq!(editor.selection().focus, Position::new(0, starts[2]));
    assert_eq!(row_of(&context, &editor), 1);
    press(&mut editor, Key::Home, Modifiers::NONE);
    assert_eq!(editor.selection().focus, Position::new(0, starts[1]));
    assert_eq!(row_of(&context, &editor), 1);
    press(&mut editor, Key::End, Modifiers::SHIFT);
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, starts[1]), Position::new(0, starts[2]))
    );
    let mac_command = Modifiers::MAC_CMD | Modifiers::COMMAND;
    press(&mut editor, Key::ArrowLeft, mac_command);
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, starts[1]))
    );
    press(&mut editor, Key::ArrowRight, mac_command);
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, starts[2]))
    );
    press(&mut editor, Key::End, Modifiers::COMMAND);
    assert_eq!(editor.selection().focus, editor.document().end());
    press(&mut editor, Key::Home, Modifiers::COMMAND);
    assert_eq!(editor.selection().focus, Position::default());
}

#[test]
fn page_down_and_up_move_about_one_viewport_and_keep_the_column() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&"line\n".repeat(100));
    editor
        .set_selection(Selection::caret(Position::new(0, 2)))
        .unwrap();
    scrolling_frame(&context, &mut editor, vec![], true, 0.0);
    scrolling_frame(
        &context,
        &mut editor,
        vec![key(Key::PageDown, Modifiers::NONE)],
        true,
        0.0,
    );
    let first = editor.selection().focus;
    assert!((3..=6).contains(&first.paragraph), "{first:?}");
    assert_eq!(first.byte, 2);
    let (_, offset, _, _) = scrolling_frame(
        &context,
        &mut editor,
        vec![key(Key::PageDown, Modifiers::SHIFT)],
        true,
        0.0,
    );
    assert!(offset.y > 0.0);
    let second = editor.selection();
    assert_eq!(second.anchor, first);
    assert!(second.focus.paragraph > first.paragraph + 2);
    assert_eq!(second.focus.byte, 2);
    for _ in 0..3 {
        scrolling_frame(
            &context,
            &mut editor,
            vec![key(Key::PageUp, Modifiers::NONE)],
            true,
            0.0,
        );
    }
    assert_eq!(editor.selection(), Selection::caret(Position::default()));
    scrolling_frame(
        &context,
        &mut editor,
        vec![key(Key::PageDown, Modifiers::COMMAND)],
        true,
        0.0,
    );
    assert_eq!(editor.selection(), Selection::caret(Position::default()));
    assert!(
        context.input(|input| matches!(
            input.events.as_slice(),
            [Event::Key {
                key: Key::PageDown,
                modifiers: Modifiers::COMMAND,
                ..
            }]
        )),
        "Ctrl/Cmd+PageDown is left to the host"
    );
}

#[test]
fn selection_is_spliced_between_code_backgrounds_and_glyphs() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("plain code");
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 10)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            code: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor.select_all();
    let (output, _) = widget_frame(&context, &mut editor, vec![], None, true, unchanged);
    let visuals = context.global_style().visuals.clone();
    let selection = visuals.selection.bg_fill;
    let galley = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "plain code" => {
                Some(text.galley.clone())
            }
            _ => None,
        })
        .expect("painted paragraph");
    let row = &galley.rows[0];
    let mesh = &row.visuals.mesh;
    let first_use = |color| {
        let vertex = mesh
            .vertices
            .iter()
            .position(|vertex| vertex.color == color)
            .expect("colored quad") as u32;
        mesh.indices
            .iter()
            .position(|&index| index == vertex)
            .unwrap()
    };
    assert!(first_use(visuals.code_bg_color) < first_use(selection));
    assert!(first_use(selection) < row.visuals.glyph_index_start);
    assert!(
        mesh.indices[row.visuals.glyph_index_start..]
            .iter()
            .all(|&index| mesh.vertices[index as usize].color != selection)
    );
    let cache = widget_cache(&context);
    let cached = &cache.layout.layouts.get(0).unwrap().galley;
    assert!(!Arc::ptr_eq(cached, &galley));
    assert!(
        cached.rows[0]
            .visuals
            .mesh
            .vertices
            .iter()
            .all(|vertex| vertex.color != selection)
    );
    assert!(!output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::epaint::Shape::Rect(rect) if rect.fill == selection)
    }));
}

#[test]
fn fills_paint_behind_glyphs_replace_code_backgrounds_and_keep_selection_visible() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("plain fill code");
    let blue = crate::Color([0, 112, 192, 128]);
    for (range, patch) in [
        (
            6..10,
            StylePatch {
                background: Some(Some(crate::Color::YELLOW)),
                ..Default::default()
            },
        ),
        (
            11..15,
            StylePatch {
                code: Some(true),
                background: Some(Some(blue)),
                ..Default::default()
            },
        ),
    ] {
        editor
            .set_selection(Selection::new(
                Position::new(0, range.start),
                Position::new(0, range.end),
            ))
            .unwrap();
        editor.apply_style(patch).unwrap();
    }
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 8)))
        .unwrap();
    let (output, _) = widget_frame(&context, &mut editor, vec![], None, true, unchanged);
    let visuals = context.global_style().visuals.clone();
    let selection = visuals.selection.bg_fill;
    let yellow = Color32::from_rgb(255, 255, 0);
    let translucent = Color32::from_rgba_unmultiplied(0, 112, 192, 128);
    let galley = output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) if text.galley.job.text == "plain fill code" => {
                Some(text.galley.clone())
            }
            _ => None,
        })
        .expect("painted paragraph");
    let backgrounds: Vec<_> = galley
        .job
        .sections
        .iter()
        .map(|section| (section.format.background, section.format.expand_bg))
        .collect();
    assert_eq!(
        backgrounds,
        [
            (Color32::TRANSPARENT, 1.0),
            (yellow, 0.0),
            (Color32::TRANSPARENT, 1.0),
            (translucent, 0.0),
        ]
    );
    let row = &galley.rows[0];
    let mesh = &row.visuals.mesh;
    let first_use = |color| {
        let vertex = mesh
            .vertices
            .iter()
            .position(|vertex| vertex.color == color)
            .expect("colored quad") as u32;
        mesh.indices
            .iter()
            .position(|&index| index == vertex)
            .unwrap()
    };
    assert!(first_use(yellow) < first_use(selection));
    assert!(first_use(translucent) < first_use(selection));
    assert!(first_use(selection) < row.visuals.glyph_index_start);
    assert!(
        mesh.vertices
            .iter()
            .all(|vertex| vertex.color != visuals.code_bg_color)
    );
}

#[test]
fn retained_paint_reuses_galleys_until_the_font_cache_changes() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("cached painted café 👩‍💻");
    let painted = |output: &egui::FullOutput| {
        output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.job.text == "cached painted café 👩‍💻" =>
                {
                    Some(text.galley.clone())
                }
                _ => None,
            })
            .unwrap()
    };
    let first = painted(&frame(&context, &mut editor, vec![], false));
    for _ in 0..3 {
        let idle = painted(&frame(&context, &mut editor, vec![], false));
        assert!(Arc::ptr_eq(&first, &idle));
    }
    let mut refreshed = None;
    context
        .run_ui(egui::RawInput::default(), |ui| {
            refreshed = Some(ui.fonts_mut(|fonts| fonts.layout_job((*first.job).clone())));
        })
        .drop_without_applying_deltas();
    let refreshed = refreshed.unwrap();
    // The paragraph job was not resubmitted during idle frames, so egui evicted it.
    assert!(!Arc::ptr_eq(&first, &refreshed));
    assert_eq!(first, refreshed);
    // Hiding the editor also evicts its probe, but the atlas is unchanged.
    let returned = painted(&frame(&context, &mut editor, vec![], false));
    assert!(Arc::ptr_eq(&first, &returned));
}

#[test]
fn retained_paint_matches_fresh_widgets_after_selection_and_environment_changes() {
    let contexts = [egui::Context::default(), egui::Context::default()];
    let mut editor = Editor::from_text(
        "filled café and 👩‍💻 text\nordered words wrapping across several rows\nquote text",
    );
    editor
        .set_selection(Selection::new(Position::new(0, 0), Position::new(0, 6)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            background: Some(Some(crate::Color::YELLOW)),
            code: Some(true),
            ..Default::default()
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    editor
        .set_paragraph_kind(ParagraphKind::Ordered {
            indent: 1,
            start: Some(12),
        })
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(2, 0)))
        .unwrap();
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    let id = Id::new("retained-paint");
    let mut width = 220.0;
    for step in 0..12 {
        match step {
            0 => editor.select_all(),
            2 => editor
                .set_selection(Selection::caret(Position::new(0, 0)))
                .unwrap(),
            4 => {
                for context in &contexts {
                    context
                        .run_ui(egui::RawInput::default(), |ui| {
                            ui.label("editor hidden");
                        })
                        .drop_without_applying_deltas();
                }
                continue;
            }
            6 => {
                let mut definitions = egui::FontDefinitions::default();
                definitions.families.insert(
                    FontFamily::Proportional,
                    definitions.families[&FontFamily::Monospace].clone(),
                );
                for context in &contexts {
                    context.set_fonts(definitions.clone());
                }
            }
            7 => width = 100.0,
            8 => {
                for context in &contexts {
                    context.set_pixels_per_point(2.0);
                }
            }
            9 => {
                for context in &contexts {
                    context.set_visuals(egui::Visuals::light());
                }
            }
            10 => {
                editor.select_all();
                editor
                    .apply_style(StylePatch {
                        foreground: Some(Some(crate::Color([40, 100, 200, 255]))),
                        ..Default::default()
                    })
                    .unwrap();
            }
            11 => editor = Editor::from_text("replacement document\nnew second line"),
            _ => {}
        }
        let highlights = if step == 3 || step == 5 {
            vec![Position::new(0, 1)..Position::new(1, 7)]
        } else {
            vec![]
        };
        let mut shapes = Vec::new();
        for (index, context) in contexts.iter().enumerate() {
            if index == 1 {
                RichTextEditor::forget_state(context, id);
            }
            let mut editor = editor.clone();
            let mut output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 600.0))),
                    time: Some(step as f64 * 0.1),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let output = RichTextEditor::new(&mut editor)
                            .id(id)
                            .desired_width(width)
                            .highlight_ranges(&highlights, Color32::from_rgb(20, 210, 80))
                            .show(ui);
                        assert!(output.errors.is_empty());
                    });
                },
            );
            output.textures_delta.clear();
            shapes.push(output.shapes);
        }
        assert_eq!(shapes[0], shapes[1], "step={step}");
    }
}

#[test]
fn host_highlight_ranges_paint_visible_matches_and_skip_stale_ranges() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("find me\nand find you");
    let found = editor
        .document()
        .find("find", crate::SearchOptions::default());
    assert_eq!(found.len(), 2);
    let ranges = [
        found[0].clone(),
        Position::new(0, 5)..Position::new(0, 60),
        Position::new(0, 50)..Position::new(1, 3),
        Position::new(1, 0)..Position::new(9, 0),
        found[1].clone(),
        Position::new(7, 0)..Position::new(9, 0),
    ];
    let color = Color32::from_rgb(250, 200, 10);
    let mut output = context.run_ui(egui::RawInput::default(), |ui| {
        let output = RichTextEditor::new(&mut editor)
            .highlight_ranges(&ranges, color)
            .show(ui);
        assert!(output.errors.is_empty());
    });
    output.textures_delta.clear();
    let highlighted = output
        .shapes
        .iter()
        .filter(
            |shape| matches!(&shape.shape, egui::epaint::Shape::Rect(rect) if rect.fill == color),
        )
        .count();
    assert_eq!(highlighted, 2);
}

#[test]
fn output_reports_caret_and_selection_geometry_even_when_unfocused() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&format!("{}end", "line\n".repeat(60)));
    let mut offset = Vec2::ZERO;
    let mut geometry = (Rect::NOTHING, None, Rect::NOTHING);
    let mut run = |editor: &mut Editor, scroll: bool| {
        context
            .run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let area = egui::ScrollArea::vertical()
                            .animated(false)
                            .max_height(100.0)
                            .show(ui, |ui| {
                                let output = RichTextEditor::new(editor)
                                    .id(Id::new("unfocused"))
                                    .show(ui);
                                if scroll {
                                    ui.scroll_to_rect(output.caret_rect, None);
                                }
                                (
                                    output.caret_rect,
                                    output.selection_rect,
                                    output.response.rect,
                                )
                            });
                        offset = area.state.offset;
                        geometry = area.inner;
                    });
                },
            )
            .drop_without_applying_deltas();
        (offset, geometry)
    };
    let (_, (caret, selection, rect)) = run(&mut editor, false);
    assert_eq!(selection, None);
    assert!(rect.contains_rect(caret));
    assert_eq!(caret.left(), rect.left() + PADDING);
    editor
        .set_selection(Selection::new(Position::new(1, 1), editor.document().end()))
        .unwrap();
    let (offset, (caret, selection, rect)) = run(&mut editor, false);
    assert_eq!(
        offset.y, 0.0,
        "an unfocused editor does not scroll by itself"
    );
    let selection = selection.expect("selected text bounds");
    assert!(selection.contains_rect(caret));
    assert_eq!(selection.left(), rect.left() + PADDING);
    assert!(selection.top() > rect.top() + PADDING && caret.bottom() <= rect.bottom());
    run(&mut editor, true);
    let (offset, _) = run(&mut editor, true);
    assert!(
        offset.y > 100.0,
        "hosts can reveal the selection: {offset:?}"
    );
}

#[test]
fn pointer_and_blur_commit_preedit_text_while_escape_cancels_it() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc def");
    let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), true, unchanged);
    widget_frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "にほんご".into(),
            active_range_chars: Some(4..4),
        })],
        Some(0.1),
        true,
        unchanged,
    );
    let target = Position::new(0, "にほんごa".len());
    let point = caret_point(&context, widget.response.rect, &editor, target);
    let (output, _) = widget_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(point),
            primary(point, true, Modifiers::NONE),
        ],
        Some(0.2),
        true,
        unchanged,
    );
    assert!(
        output
            .platform_output
            .ime
            .unwrap()
            .should_interrupt_composition
    );
    widget_frame(
        &context,
        &mut editor,
        vec![primary(point, false, Modifiers::NONE)],
        Some(0.25),
        true,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "にほんごabc def");
    assert!(editor.composition().is_none());
    assert_eq!(editor.selection(), Selection::caret(target));
    assert_eq!(editor.undo_len(), 1);
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abc def");

    editor.update_composition("", None).unwrap();
    widget_frame(
        &context,
        &mut editor,
        vec![primary(point, true, Modifiers::NONE)],
        Some(2.0),
        true,
        unchanged,
    );
    widget_frame(
        &context,
        &mut editor,
        vec![primary(point, false, Modifiers::NONE)],
        Some(2.05),
        true,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "abc def");
    assert!(editor.composition().is_none());

    editor.update_composition("候補", None).unwrap();
    widget_frame(&context, &mut editor, vec![], Some(3.0), true, unchanged);
    context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
    widget_frame(&context, &mut editor, vec![], Some(3.1), false, unchanged);
    assert!(editor.composition().is_none());
    assert!(editor.document().plain_text().contains("候補"));
    assert!(editor.undo());
    assert_eq!(editor.document().plain_text(), "abc def");

    editor.update_composition("候補", None).unwrap();
    widget_frame(
        &context,
        &mut editor,
        vec![key(Key::Escape, Modifiers::NONE)],
        Some(4.0),
        true,
        unchanged,
    );
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "abc def");
}

fn composing_frame(context: &egui::Context, editor: &mut Editor, preedit: &str) -> Rect {
    let (_, widget) = widget_frame(context, editor, vec![], Some(0.0), true, unchanged);
    widget_frame(
        context,
        editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: preedit.into(),
            active_range_chars: None,
        })],
        Some(0.1),
        true,
        unchanged,
    );
    widget.response.rect
}

fn candidate_commit(text: &str) -> [Event; 2] {
    [
        Event::Ime(ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        }),
        Event::Ime(ImeEvent::Commit(text.into())),
    ]
}

#[test]
fn a_first_queued_preedit_keeps_its_selection_before_a_click_or_blur() {
    let preedit = || {
        Event::Ime(ImeEvent::Preedit {
            text: "候".into(),
            active_range_chars: None,
        })
    };
    for before_click in [false, true] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc def");
        let selection = Selection::new(Position::default(), Position::new(0, 3));
        editor.set_selection(selection).unwrap();
        let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), true, unchanged);
        let point = caret_point(
            &context,
            widget.response.rect,
            &editor,
            editor.document().end(),
        );
        let mut events = vec![
            Event::PointerMoved(point),
            primary(point, true, Modifiers::NONE),
        ];
        events.insert(if before_click { 0 } else { events.len() }, preedit());
        widget_frame(&context, &mut editor, events, Some(0.1), false, unchanged);
        if before_click {
            assert_eq!(editor.document().plain_text(), "候 def");
            assert!(editor.composition().is_none());
            assert_eq!(
                editor.selection(),
                Selection::caret(editor.document().end())
            );
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "abc def");
            assert_eq!(editor.selection(), selection);
        } else {
            assert_eq!(editor.document().plain_text(), "abc def");
            assert_eq!(
                editor.composition().unwrap().replacement,
                Selection::caret(editor.document().end()),
            );
        }
        assert!(context.input(|input| {
            !input
                .events
                .iter()
                .any(|event| matches!(event, Event::Ime(_)))
        }));
    }

    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc def");
    editor
        .set_selection(Selection::new(Position::default(), Position::new(0, 3)))
        .unwrap();
    widget_frame(&context, &mut editor, vec![], Some(0.0), true, unchanged);
    context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
    widget_frame(
        &context,
        &mut editor,
        vec![preedit()],
        Some(0.1),
        false,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "候 def");
    assert!(editor.composition().is_none());
    assert!(context.input(|input| input.events.is_empty()));
}

#[test]
fn ime_events_queued_before_a_click_apply_before_the_click_commits() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc def");
    let rect = composing_frame(&context, &mut editor, "ni");
    let point = caret_point(&context, rect, &editor, Position::new(0, "nia".len()));
    let mut events = candidate_commit("你").to_vec();
    events.extend([
        Event::PointerMoved(point),
        primary(point, true, Modifiers::NONE),
    ]);
    let (output, _) = widget_frame(&context, &mut editor, events, Some(0.2), true, unchanged);
    assert_eq!(editor.document().plain_text(), "你abc def");
    assert!(editor.composition().is_none());
    assert_eq!(editor.undo_len(), 1);
    assert!(
        !output
            .platform_output
            .ime
            .unwrap()
            .should_interrupt_composition
    );
    assert!(context.input(|input| {
        !input
            .events
            .iter()
            .any(|event| matches!(event, Event::Ime(_)))
    }));

    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc");
    let rect = composing_frame(&context, &mut editor, "n");
    let point = caret_point(&context, rect, &editor, Position::new(0, "na".len()));
    widget_frame(
        &context,
        &mut editor,
        vec![
            Event::Ime(ImeEvent::Preedit {
                text: "ni".into(),
                active_range_chars: None,
            }),
            Event::PointerMoved(point),
            primary(point, true, Modifiers::NONE),
            Event::Text("x".into()),
        ],
        Some(0.2),
        true,
        unchanged,
    );
    let text = editor.document().plain_text();
    assert!(text.starts_with("ni") && text.contains('x'), "{text}");
    assert_eq!(text.len(), "niabcx".len());
}

#[test]
fn a_click_after_queued_ime_commits_hit_tests_the_committed_text() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc");
    let rect = composing_frame(&context, &mut editor, "ni");
    let row = caret_point(&context, rect, &editor, Position::default()).y;
    let point = Pos2::new(rect.right() - PADDING - 1.0, row);
    let mut events = candidate_commit("你").to_vec();
    events.extend([
        Event::PointerMoved(point),
        primary(point, true, Modifiers::NONE),
    ]);
    widget_frame(&context, &mut editor, events, Some(0.2), true, unchanged);
    assert_eq!(editor.document().plain_text(), "你abc");
    assert_eq!(
        editor.selection(),
        Selection::caret(editor.document().end())
    );
}

#[test]
fn ime_events_queued_with_a_focus_change_apply_before_blur_commits() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc");
    composing_frame(&context, &mut editor, "ni");
    context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
    widget_frame(
        &context,
        &mut editor,
        candidate_commit("你").to_vec(),
        Some(0.2),
        false,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "你abc");
    assert!(editor.composition().is_none());
    assert!(context.input(|input| input.events.is_empty()));

    let mut other = Editor::from_text("abc");
    widget_frame(
        &context,
        &mut other,
        candidate_commit("你").to_vec(),
        Some(0.3),
        false,
        unchanged,
    );
    assert_eq!(other.document().plain_text(), "abc");
    assert_eq!(context.input(|input| input.events.len()), 2);
}

#[test]
fn an_input_method_echoing_a_click_commit_is_inserted_once() {
    let click_commit = |context: &egui::Context, editor: &mut Editor| {
        let rect = composing_frame(context, editor, "한");
        let point = caret_point(context, rect, editor, Position::new(0, "한ab".len()));
        let (output, _) = widget_frame(
            context,
            editor,
            vec![
                Event::PointerMoved(point),
                primary(point, true, Modifiers::NONE),
            ],
            Some(0.2),
            true,
            unchanged,
        );
        assert!(
            output
                .platform_output
                .ime
                .unwrap()
                .should_interrupt_composition
        );
        widget_frame(
            context,
            editor,
            vec![primary(point, false, Modifiers::NONE)],
            Some(0.25),
            true,
            unchanged,
        );
        assert_eq!(editor.document().plain_text(), "한abc");
    };
    let commit = |text: &str| Event::Ime(ImeEvent::Commit(text.into()));

    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc");
    click_commit(&context, &mut editor);
    let mut echo = candidate_commit("한").to_vec();
    #[allow(deprecated)]
    echo.push(Event::Ime(ImeEvent::Disabled));
    widget_frame(&context, &mut editor, echo, Some(0.3), true, unchanged);
    assert_eq!(editor.document().plain_text(), "한abc");
    widget_frame(
        &context,
        &mut editor,
        vec![commit("한")],
        Some(0.35),
        true,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "한ab한c");

    for (events, time) in [
        (vec![Event::Text("x".into()), commit("한")], 0.3),
        (vec![commit("하")], 0.3),
        (vec![commit("한")], 1.0),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        click_commit(&context, &mut editor);
        let typed = events.len() == 2;
        widget_frame(&context, &mut editor, events, Some(time), true, unchanged);
        let text = editor.document().plain_text();
        assert_eq!(
            text.chars().count(),
            "한abc".chars().count() + 1 + usize::from(typed),
            "{text}"
        );
    }
}

#[test]
fn blur_commit_echoes_are_ignored_after_focus_returns() {
    let commit = || Event::Ime(ImeEvent::Commit("한".into()));
    for (events, time, expected) in [
        (candidate_commit("한").to_vec(), 0.3, "한abc"),
        (
            vec![
                Event::Ime(ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: None,
                }),
                commit(),
            ],
            0.3,
            "한한abc",
        ),
        (vec![Event::Text("x".into()), commit()], 0.3, "한x한abc"),
        (vec![commit()], 1.0, "한한abc"),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        composing_frame(&context, &mut editor, "한");
        context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
        widget_frame(&context, &mut editor, vec![], Some(0.2), false, unchanged);
        assert_eq!(editor.document().plain_text(), "한abc");
        widget_frame(&context, &mut editor, events, Some(time), true, unchanged);
        assert_eq!(editor.document().plain_text(), expected);
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn ignored_input_preserves_canceled_and_blur_commit_echoes() {
    let mut events = Vec::new();
    for newline in ["\n", "\r", "\r\n"] {
        events.push(Event::Ime(ImeEvent::Preedit {
            text: newline.into(),
            active_range_chars: None,
        }));
        events.push(Event::Ime(ImeEvent::Commit(newline.into())));
    }
    events.extend([
        Event::Text("\0\t\n\r\u{7f}".into()),
        Event::Paste("\0\u{7f}".into()),
        Event::Ime(ImeEvent::Commit("\0\u{7f}".into())),
        Event::Text(String::new()),
        Event::Paste(String::new()),
        Event::Ime(ImeEvent::Commit(String::new())),
        Event::Cut,
    ]);
    for canceled in [false, true] {
        for ignored in &events {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            composing_frame(&context, &mut editor, "한");
            let cancellation = if canceled {
                vec![key(Key::Escape, Modifiers::NONE)]
            } else {
                context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
                vec![]
            };
            widget_frame(
                &context,
                &mut editor,
                cancellation,
                Some(0.2),
                canceled,
                unchanged,
            );
            let text = editor.document().plain_text();
            let undo_len = editor.undo_len();
            widget_frame(
                &context,
                &mut editor,
                vec![ignored.clone(), Event::Ime(ImeEvent::Commit("한".into()))],
                Some(0.3),
                true,
                unchanged,
            );
            assert_eq!(
                editor.document().plain_text(),
                text,
                "canceled={canceled}, ignored={ignored:?}",
            );
            assert_eq!(editor.undo_len(), undo_len);
            assert!(editor.composition().is_none());
            assert!(context.input(|input| input.events.is_empty()));
        }
    }
}

#[test]
fn ignored_text_preserves_a_cleared_preedit() {
    for ignored in [
        Event::Text(String::new()),
        Event::Text("\0\t\r\n\u{7f}".into()),
        Event::Paste(String::new()),
        Event::Paste("\0\u{7f}".into()),
    ] {
        for canceled in [false, true] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            editor.select_all();
            composing_frame(&context, &mut editor, "한");
            widget_frame(
                &context,
                &mut editor,
                vec![
                    Event::Ime(ImeEvent::Preedit {
                        text: String::new(),
                        active_range_chars: None,
                    }),
                    ignored.clone(),
                ],
                Some(0.2),
                true,
                unchanged,
            );
            assert!(
                editor
                    .composition()
                    .is_some_and(|composition| composition.text.is_empty()),
                "{ignored:?}",
            );
            assert_eq!(editor.selected_text(), "abc");
            assert!(!editor.can_undo());
            let mut events = Vec::new();
            if canceled {
                events.push(key(Key::Escape, Modifiers::NONE));
            }
            events.push(Event::Ime(ImeEvent::Commit("한".into())));
            widget_frame(&context, &mut editor, events, Some(0.3), true, unchanged);
            assert_eq!(
                editor.document().plain_text(),
                if canceled { "abc" } else { "한" },
                "canceled={canceled}, ignored={ignored:?}",
            );
        }
    }
}

#[test]
fn accepted_input_ends_a_previous_cancellation_echo_guard() {
    for (event, expected) in [
        (Event::Text("x".into()), "x한"),
        (Event::Paste("x".into()), "x한"),
        (Event::Cut, "한"),
        (key(Key::ArrowRight, Modifiers::NONE), "abc한"),
        (
            Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 0,
                after_chars: 0,
            }),
            "한",
        ),
        (
            Event::Ime(ImeEvent::DeleteSurrounding {
                before_chars: 1,
                after_chars: 0,
            }),
            "ab한",
        ),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        editor.select_all();
        composing_frame(&context, &mut editor, "한");
        widget_frame(
            &context,
            &mut editor,
            vec![key(Key::Escape, Modifiers::NONE)],
            Some(0.2),
            true,
            unchanged,
        );
        widget_frame(
            &context,
            &mut editor,
            vec![event.clone(), Event::Ime(ImeEvent::Commit("한".into()))],
            Some(0.3),
            true,
            unchanged,
        );
        assert_eq!(editor.document().plain_text(), expected, "{event:?}");
        assert!(editor.composition().is_none());
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn ordinary_input_cancels_a_cleared_preedit_and_rejects_its_late_commit() {
    for (event, expected) in [
        (Event::Text("x".into()), "x"),
        (Event::Paste("x".into()), "x"),
        (Event::Cut, ""),
        (key(Key::ArrowRight, Modifiers::NONE), "abc"),
        (key(Key::Home, Modifiers::NONE), "abc"),
        (key(Key::End, Modifiers::NONE), "abc"),
        (key(Key::ArrowDown, Modifiers::NONE), "abc"),
        (key(Key::PageDown, Modifiers::NONE), "abc"),
        (key(Key::Backspace, Modifiers::NONE), ""),
        (key(Key::Delete, Modifiers::NONE), ""),
        (key(Key::Enter, Modifiers::NONE), "\n"),
        (key(Key::Enter, Modifiers::SHIFT), "\n"),
        (key(Key::B, Modifiers::COMMAND), "abc"),
    ] {
        for separate_frame in [false, true] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            editor.select_all();
            composing_frame(&context, &mut editor, "한");
            let mut events = vec![
                Event::Ime(ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }),
                event.clone(),
            ];
            if separate_frame {
                widget_frame(
                    &context,
                    &mut editor,
                    std::mem::take(&mut events),
                    Some(0.2),
                    true,
                    unchanged,
                );
            }
            events.push(Event::Ime(ImeEvent::Commit("한".into())));
            widget_frame(&context, &mut editor, events, Some(0.3), true, unchanged);
            assert_eq!(
                editor.document().plain_text(),
                expected,
                "separate_frame={separate_frame}, event={event:?}",
            );
            assert!(editor.composition().is_none());
            assert!(context.input(|input| input.events.is_empty()));
        }
    }
}

#[test]
fn text_keys_preserve_a_cleared_preedit_until_their_text_event() {
    for (pressed, text) in [
        (key(Key::X, Modifiers::NONE), Event::Text("x".into())),
        (
            key(Key::V, Modifiers::CTRL | Modifiers::COMMAND),
            Event::Paste("x".into()),
        ),
    ] {
        for separate_frame in [false, true] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            editor.select_all();
            composing_frame(&context, &mut editor, "한");
            let mut events = vec![
                Event::Ime(ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }),
                pressed.clone(),
            ];
            if separate_frame {
                widget_frame(
                    &context,
                    &mut editor,
                    std::mem::take(&mut events),
                    Some(0.2),
                    true,
                    unchanged,
                );
                assert!(
                    editor
                        .composition()
                        .is_some_and(|composition| composition.text.is_empty()),
                    "{pressed:?}",
                );
            }
            events.extend([text.clone(), Event::Ime(ImeEvent::Commit("한".into()))]);
            widget_frame(&context, &mut editor, events, Some(0.3), true, unchanged);
            assert_eq!(
                editor.document().plain_text(),
                "x",
                "separate_frame={separate_frame}, pressed={pressed:?}",
            );
            assert!(editor.composition().is_none());
        }
    }
}

#[test]
fn rejected_preedit_ranges_preserve_canceled_commit_echoes() {
    for range in [Range { start: 3, end: 1 }, 0..99, usize::MAX..usize::MAX] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        composing_frame(&context, &mut editor, "한");
        widget_frame(
            &context,
            &mut editor,
            vec![key(Key::Escape, Modifiers::NONE)],
            Some(0.2),
            true,
            unchanged,
        );
        let mut errors = Vec::new();
        context
            .run_ui(
                egui::RawInput {
                    time: Some(0.3),
                    events: vec![
                        Event::Ime(ImeEvent::Preedit {
                            text: "changed\r\n👩‍💻".into(),
                            active_range_chars: Some(range),
                        }),
                        Event::Ime(ImeEvent::Commit("한".into())),
                    ],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let id = Id::new(WIDGET_ID);
                        ui.memory_mut(|memory| memory.request_focus(id));
                        errors = RichTextEditor::new(&mut editor).id(id).show(ui).errors;
                    });
                },
            )
            .drop_without_applying_deltas();
        assert_eq!(errors, vec![Error::InvalidCompositionSelection]);
        assert_eq!(editor.document().plain_text(), "abc");
        assert!(editor.composition().is_none());
        assert!(!editor.can_undo());
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
#[allow(deprecated)]
fn ignored_ime_events_preserve_cancellation_echoes_queued_before_blur() {
    for ignored in [
        Event::Ime(ImeEvent::Preedit {
            text: "\n".into(),
            active_range_chars: None,
        }),
        Event::Ime(ImeEvent::Commit("\r\n".into())),
        Event::Ime(ImeEvent::Commit("\0\u{7f}".into())),
        Event::Ime(ImeEvent::Commit(String::new())),
        Event::Ime(ImeEvent::Preedit {
            text: "rejected".into(),
            active_range_chars: Some(0..99),
        }),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        composing_frame(&context, &mut editor, "한");
        context.memory_mut(|memory| memory.surrender_focus(Id::new(WIDGET_ID)));
        let rejected = matches!(
            ignored,
            Event::Ime(ImeEvent::Preedit {
                active_range_chars: Some(_),
                ..
            })
        );
        let mut errors = Vec::new();
        context
            .run_ui(
                egui::RawInput {
                    time: Some(0.2),
                    events: vec![
                        Event::Ime(ImeEvent::Disabled),
                        ignored.clone(),
                        Event::Ime(ImeEvent::Commit("한".into())),
                    ],
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        errors = RichTextEditor::new(&mut editor)
                            .id(Id::new(WIDGET_ID))
                            .show(ui)
                            .errors;
                    });
                },
            )
            .drop_without_applying_deltas();
        assert_eq!(
            errors,
            if rejected {
                vec![Error::InvalidCompositionSelection]
            } else {
                vec![]
            },
        );
        assert_eq!(editor.document().plain_text(), "abc", "{ignored:?}");
        assert!(editor.composition().is_none());
        assert!(!editor.can_undo());
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn canceled_compositions_reject_late_commits_but_allow_new_preedit() {
    use egui::accesskit::{Action, ActionData, ActionRequest, TextSelection, TreeId};

    for cancellation in ["escape", "read-only", "disabled", "accessibility"] {
        for (new_preedit, cleared) in [(false, false), (true, false), (false, true), (true, true)] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let mut editor = Editor::from_text("abc");
            frame(
                &context,
                &mut editor,
                vec![Event::Ime(ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: Some(1..1),
                })],
                true,
            );
            if cleared {
                frame(
                    &context,
                    &mut editor,
                    vec![Event::Ime(ImeEvent::Preedit {
                        text: String::new(),
                        active_range_chars: None,
                    })],
                    true,
                );
            }
            match cancellation {
                "escape" => {
                    frame(
                        &context,
                        &mut editor,
                        vec![key(Key::Escape, Modifiers::NONE)],
                        true,
                    );
                }
                "read-only" => {
                    frame_options(
                        &context,
                        &mut editor,
                        vec![
                            Event::Ime(ImeEvent::Preedit {
                                text: "한".into(),
                                active_range_chars: None,
                            }),
                            Event::Text("ignored".into()),
                        ],
                        true,
                        true,
                    );
                }
                "disabled" => {
                    clipboard_frame(&context, &mut editor, vec![], true, false, false, None);
                }
                "accessibility" => {
                    let id = context.memory(|memory| memory.focused()).unwrap();
                    let snapshot = context
                        .data(|data| data.get_temp::<Cache>(id))
                        .unwrap()
                        .accessibility
                        .unwrap();
                    let end = snapshot.position(editor.document().end()).unwrap();
                    frame(
                        &context,
                        &mut editor,
                        vec![Event::AccessKitActionRequest(ActionRequest {
                            action: Action::SetTextSelection,
                            target_node: id.accesskit_id(),
                            target_tree: TreeId::ROOT,
                            data: Some(ActionData::SetTextSelection(TextSelection {
                                anchor: end,
                                focus: end,
                            })),
                        })],
                        true,
                    );
                }
                _ => unreachable!(),
            }
            assert!(editor.composition().is_none());
            let selection = editor.selection();
            frame(
                &context,
                &mut editor,
                vec![
                    Event::Ime(ImeEvent::Preedit {
                        text: if new_preedit { "한" } else { "" }.into(),
                        active_range_chars: None,
                    }),
                    Event::Ime(ImeEvent::Commit("한".into())),
                ],
                true,
            );
            let expected = match (new_preedit, cancellation) {
                (false, _) => "abc",
                (true, "accessibility") => "abc한",
                (true, _) => "한abc",
            };
            assert_eq!(
                editor.document().plain_text(),
                expected,
                "{cancellation}, new_preedit={new_preedit}, cleared={cleared}",
            );
            if !new_preedit {
                assert_eq!(editor.selection(), selection);
                assert!(!editor.can_undo());
            }
            assert!(context.input(|input| input.events.is_empty()));
        }
    }
}

#[test]
#[allow(deprecated)]
fn ime_cancellation_precedes_a_commit_in_the_same_input_batch() {
    for (blur, cleared) in [(false, false), (false, true), (true, false), (true, true)] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("abc");
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Preedit {
                text: "한".into(),
                active_range_chars: None,
            })],
            true,
        );
        let cancellation = if blur {
            context.memory_mut(|memory| memory.surrender_focus(memory.focused().unwrap()));
            Event::Ime(ImeEvent::Disabled)
        } else {
            key(Key::Escape, Modifiers::NONE)
        };
        let mut events = Vec::new();
        if cleared {
            events.push(Event::Ime(ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None,
            }));
        }
        events.extend([cancellation, Event::Ime(ImeEvent::Commit("한".into()))]);
        frame(&context, &mut editor, events, !blur);
        assert_eq!(
            editor.document().plain_text(),
            "abc",
            "blur={blur}, cleared={cleared}"
        );
        assert!(editor.composition().is_none());
        assert!(!editor.can_undo());
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn surrounding_deletion_keeps_a_following_commit_from_the_same_ime_edit() {
    for blur in [false, true] {
        for (before_chars, cleared) in [(0, false), (1, false), (0, true), (1, true)] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            editor
                .set_selection(Selection::caret(editor.document().end()))
                .unwrap();
            frame(
                &context,
                &mut editor,
                vec![Event::Ime(ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: None,
                })],
                true,
            );
            if blur {
                context.memory_mut(|memory| memory.surrender_focus(memory.focused().unwrap()));
            }
            let mut events = Vec::new();
            if cleared {
                events.push(Event::Ime(ImeEvent::Preedit {
                    text: String::new(),
                    active_range_chars: None,
                }));
            }
            events.extend([
                Event::Ime(ImeEvent::DeleteSurrounding {
                    before_chars,
                    after_chars: 0,
                }),
                Event::Ime(ImeEvent::Commit("한".into())),
            ]);
            frame(&context, &mut editor, events, !blur);
            let expected = if before_chars == 0 { "abc한" } else { "ab한" };
            assert_eq!(
                editor.document().plain_text(),
                expected,
                "blur={blur}, before_chars={before_chars}, cleared={cleared}",
            );
            assert!(editor.composition().is_none());
            assert_eq!(
                editor.selection(),
                Selection::caret(editor.document().end())
            );
            assert!(context.input(|input| input.events.is_empty()));
        }
    }
}

#[test]
fn cleared_preedit_still_accepts_a_normal_commit() {
    for separate_frame in [false, true] {
        for committed in ["한", "韓"] {
            let context = egui::Context::default();
            let mut editor = Editor::from_text("abc");
            frame(
                &context,
                &mut editor,
                vec![Event::Ime(ImeEvent::Preedit {
                    text: "한".into(),
                    active_range_chars: None,
                })],
                true,
            );
            let mut events = vec![Event::Ime(ImeEvent::Preedit {
                text: String::new(),
                active_range_chars: None,
            })];
            if separate_frame {
                frame(&context, &mut editor, std::mem::take(&mut events), true);
            }
            events.push(Event::Ime(ImeEvent::Commit(committed.into())));
            frame(&context, &mut editor, events, true);
            assert_eq!(editor.document().plain_text(), format!("{committed}abc"));
            assert!(editor.composition().is_none());
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "abc");
            assert!(!editor.can_undo());
        }
    }
}

#[test]
fn ime_commits_merge_like_typing_until_a_line_break() {
    let context = egui::Context::default();
    let mut editor = Editor::default();
    frame(&context, &mut editor, vec![], true);
    for text in ["a", "b", "c"] {
        frame(
            &context,
            &mut editor,
            vec![Event::Ime(ImeEvent::Commit(text.into()))],
            true,
        );
    }
    frame(&context, &mut editor, vec![Event::Text("d".into())], true);
    assert_eq!(editor.document().plain_text(), "abcd");
    assert_eq!(editor.undo_len(), 1);
    frame(
        &context,
        &mut editor,
        vec![
            Event::Ime(ImeEvent::Preedit {
                text: "に".into(),
                active_range_chars: None,
            }),
            Event::Ime(ImeEvent::Commit("日".into())),
            Event::Ime(ImeEvent::Commit("x\ny".into())),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "abcd日x\ny");
    assert_eq!(editor.undo_len(), 2);
}

#[test]
fn ime_line_break_text_is_ignored_like_egui_text_edit() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc");
    editor
        .set_selection(Selection::caret(Position::new(0, 3)))
        .unwrap();
    for text in ["\n", "\r", "\r\n"] {
        frame(
            &context,
            &mut editor,
            vec![
                Event::Ime(ImeEvent::Commit(text.into())),
                Event::Ime(ImeEvent::Preedit {
                    text: text.into(),
                    active_range_chars: None,
                }),
            ],
            true,
        );
        assert_eq!(editor.document().plain_text(), "abc");
        assert!(editor.composition().is_none());
        assert!(context.input(|input| input.events.is_empty()));
    }
    frame(
        &context,
        &mut editor,
        vec![key(Key::Enter, Modifiers::NONE)],
        true,
    );
    assert_eq!(editor.document().plain_text(), "abc\n");
}

#[test]
fn empty_preedit_keeps_the_source_caret_visible() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("hello");
    frame(&context, &mut editor, vec![], true);
    let normal = frame(&context, &mut editor, vec![], true);
    assert_eq!(caret_segments(&normal), 1);
    let hidden = frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: "a".into(),
            active_range_chars: None,
        })],
        true,
    );
    assert_eq!(caret_segments(&hidden), 0);
    let empty = frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Preedit {
            text: String::new(),
            active_range_chars: None,
        })],
        true,
    );
    assert!(editor.composition().is_some());
    assert_eq!(caret_segments(&empty), 1);
}

#[test]
fn pasted_typed_and_committed_text_drop_control_characters() {
    let context = egui::Context::default();
    let mut editor = Editor::default();
    let mut clipboard = TestClipboard::default();
    clipboard_frame(
        &context,
        &mut editor,
        vec![Event::Paste("a\u{1b}[31mb\u{7}".into())],
        true,
        false,
        true,
        Some(&mut clipboard),
    );
    assert_eq!(editor.document().plain_text(), "a[31mb");
    assert_eq!(
        clipboard.calls,
        vec![ClipboardCall::Paste("a\u{1b}[31mb\u{7}".into())],
        "the transport sees the event's own text"
    );
    frame(
        &context,
        &mut editor,
        vec![Event::Paste("\tx\ny\u{0}".into())],
        true,
    );
    assert_eq!(editor.document().plain_text(), "a[31mb\tx\ny");
    let undo_len = editor.undo_len();
    let revision = editor.document().revision();
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("p".into()),
            Event::Paste("\u{7}\u{1b}".into()),
            Event::Text("\u{8}q\t".into()),
            Event::Ime(ImeEvent::Commit("r\u{0}".into())),
        ],
        true,
    );
    assert_eq!(editor.document().plain_text(), "a[31mb\tx\nypqr");
    assert_ne!(editor.document().revision(), revision);
    assert_eq!(
        editor.undo_len(),
        undo_len + 1,
        "control-only paste keeps typing grouped"
    );

    editor.update_composition("候", None).unwrap();
    frame(
        &context,
        &mut editor,
        vec![Event::Ime(ImeEvent::Commit("\u{8}".into()))],
        true,
    );
    assert!(editor.composition().is_none());
    assert_eq!(editor.document().plain_text(), "a[31mb\tx\nypqr");
}

#[test]
fn nonempty_preedit_consumes_cut_paste_and_text() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("source");
    editor.select_all();
    frame(&context, &mut editor, vec![], true);
    editor.update_composition("候", None).unwrap();
    let composition = editor.composition().cloned();
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("x".into()),
            Event::Paste("y".into()),
            Event::Cut,
        ],
        true,
    );
    assert!(context.input(|input| input.events.is_empty()));
    assert_eq!(editor.composition(), composition.as_ref());
    assert_eq!(editor.document().plain_text(), "source");
    assert!(!editor.can_undo());
}

#[test]
fn touch_swipes_scroll_until_the_editor_is_focused() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&"line of text\n".repeat(80));
    let id = Id::new("touch");
    let mut time = 0.0;
    let mut run = |editor: &mut Editor, events: Vec<Event>| {
        time += 0.05;
        let mut offset = Vec2::ZERO;
        context
            .run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(400.0))),
                    events,
                    time: Some(time),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let area = egui::ScrollArea::vertical().animated(false).show(ui, |ui| {
                            RichTextEditor::new(editor).id(id).show(ui);
                        });
                        offset = area.state.offset;
                    });
                },
            )
            .drop_without_applying_deltas();
        offset
    };
    let touch = |phase, pos| Event::Touch {
        device_id: egui::TouchDeviceId(1),
        id: egui::TouchId(1),
        phase,
        pos,
        force: None,
    };
    let swipe = |run: &mut dyn FnMut(&mut Editor, Vec<Event>) -> Vec2, editor: &mut Editor| {
        let mut point = Pos2::new(100.0, 300.0);
        run(
            editor,
            vec![
                touch(egui::TouchPhase::Start, point),
                Event::PointerMoved(point),
                primary(point, true, Modifiers::NONE),
            ],
        );
        for _ in 0..10 {
            point.y -= 20.0;
            run(
                editor,
                vec![
                    touch(egui::TouchPhase::Move, point),
                    Event::PointerMoved(point),
                ],
            );
        }
        run(
            editor,
            vec![
                touch(egui::TouchPhase::End, point),
                primary(point, false, Modifiers::NONE),
                Event::PointerGone,
            ],
        );
        run(editor, Vec::new())
    };
    let known = Pos2::new(100.0, 300.0);
    run(
        &mut editor,
        vec![
            touch(egui::TouchPhase::Start, known),
            touch(egui::TouchPhase::End, known),
        ],
    );
    run(&mut editor, Vec::new());
    let offset = swipe(&mut run, &mut editor);
    assert!(offset.y > 0.0, "{offset:?}");
    assert_eq!(editor.selection(), Selection::caret(Position::default()));
    assert!(!context.memory(|memory| memory.has_focus(id)));

    let tap = Pos2::new(100.0, 200.0);
    run(
        &mut editor,
        vec![
            touch(egui::TouchPhase::Start, tap),
            Event::PointerMoved(tap),
            primary(tap, true, Modifiers::NONE),
        ],
    );
    run(
        &mut editor,
        vec![
            touch(egui::TouchPhase::End, tap),
            primary(tap, false, Modifiers::NONE),
            Event::PointerGone,
        ],
    );
    assert!(context.memory(|memory| memory.has_focus(id)));
    assert!(editor.selection().is_caret());
    assert_ne!(editor.selection(), Selection::caret(Position::default()));
    run(&mut editor, Vec::new());
    swipe(&mut run, &mut editor);
    assert!(
        !editor.selection().is_caret(),
        "a focused editor drag-selects"
    );
}

#[test]
fn tab_after_a_list_shortcut_in_the_previous_frame_indents_the_new_item() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("item");
    widget_frame(&context, &mut editor, vec![], None, true, unchanged);
    widget_frame(&context, &mut editor, vec![], None, false, unchanged);
    widget_frame(
        &context,
        &mut editor,
        vec![key(Key::Num8, Modifiers::COMMAND | Modifiers::SHIFT)],
        None,
        false,
        unchanged,
    );
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Bullet { indent: 0 }
    );
    widget_frame(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::NONE)],
        None,
        false,
        unchanged,
    );
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Bullet { indent: 1 }
    );
    assert!(context.memory(|memory| memory.has_focus(Id::new(WIDGET_ID))));
}

#[test]
fn min_rows_reserves_shaped_body_rows_and_paragraph_gaps() {
    fn four(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
        widget.min_rows(4)
    }
    let context = egui::Context::default();
    let mut empty = Editor::default();
    let (_, widget) = widget_frame(&context, &mut empty, vec![], None, false, four);
    let body = egui::TextStyle::Body.resolve(&context.global_style());
    let row = context.fonts_mut(|fonts| fonts.row_height(&body));
    let reserved = widget.response.rect.height() - PADDING * 2.0;
    assert!(reserved >= 4.0 * row, "{reserved} < {}", 4.0 * row);
    let mut lines = Editor::from_text("1\n2\n3\n4");
    let (_, widget) = widget_frame(&context, &mut lines, vec![], None, false, four);
    let content = widget.response.rect.height() - PADDING * 2.0;
    assert!((content - reserved).abs() < 1.0, "{content} vs {reserved}");
}

#[test]
fn forget_state_releases_the_cached_layout() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("cached");
    let (_, widget) = widget_frame(&context, &mut editor, vec![], None, false, unchanged);
    assert_eq!(widget.response.id, Id::new(WIDGET_ID));
    assert!(
        context
            .data(|data| data.get_temp::<Cache>(widget.response.id))
            .is_some()
    );
    RichTextEditor::forget_state(&context, widget.response.id);
    assert!(
        context
            .data(|data| data.get_temp::<Cache>(widget.response.id))
            .is_none()
    );
}

#[test]
fn caret_blinks_with_egui_text_cursor_visuals_and_restarts_on_input() {
    let context = egui::Context::default();
    context.global_style_mut(|style| {
        style.visuals.text_cursor.blink = true;
        style.visuals.text_cursor.on_duration = 0.5;
        style.visuals.text_cursor.off_duration = 0.5;
    });
    let mut editor = Editor::from_text("blink");
    let run = |editor: &mut Editor, events, time| {
        let (output, _) = widget_frame(&context, editor, events, Some(time), true, unchanged);
        caret_segments(&output)
    };
    assert_eq!(run(&mut editor, vec![], 10.0), 1);
    assert_eq!(run(&mut editor, vec![], 10.3), 1);
    assert_eq!(run(&mut editor, vec![], 10.7), 0);
    assert_eq!(
        run(
            &mut editor,
            vec![key(Key::ArrowRight, Modifiers::NONE)],
            10.8
        ),
        1
    );
    assert_eq!(run(&mut editor, vec![], 11.2), 1);
    context.global_style_mut(|style| style.visuals.text_cursor.blink = false);
    assert_eq!(run(&mut editor, vec![], 11.6), 1);
}

#[test]
fn disabled_heading_shortcuts_reach_the_host() {
    fn no_headings(widget: RichTextEditor<'_>) -> RichTextEditor<'_> {
        widget.heading_shortcuts(false)
    }
    let context = egui::Context::default();
    let mut editor = Editor::from_text("title");
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 2 })
        .unwrap();
    editor.select_all();
    context.set_zoom_factor(1.5);
    widget_frame(&context, &mut editor, vec![], None, true, no_headings);
    let headings = vec![
        key(Key::Num1, Modifiers::COMMAND),
        key(Key::Num6, Modifiers::COMMAND),
    ];
    widget_frame(
        &context,
        &mut editor,
        [
            vec![key(Key::Num0, Modifiers::COMMAND)],
            headings.clone(),
            vec![key(Key::B, Modifiers::COMMAND)],
        ]
        .concat(),
        None,
        true,
        no_headings,
    );
    assert_eq!(context.input(|input| input.events.clone()), headings);
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Heading { level: 2 }
    );
    widget_frame(&context, &mut editor, vec![], None, true, no_headings);
    assert_eq!(
        context.zoom_factor(),
        1.0,
        "egui's zoom reset receives Cmd+0"
    );
    assert_eq!(editor.selection_style().bold, Some(true));
}

#[test]
fn shortcuts_follow_shift_layout_and_read_only_policies() {
    let command = Modifiers::COMMAND | Modifiers::CTRL;
    let shift = command | Modifiers::SHIFT;
    let host = [
        key(Key::Enter, command),
        key(Key::Num7, command),
        key(Key::Num8, command),
        key(Key::A, shift),
        key(Key::B, shift),
        key(Key::Num1, shift),
        key(Key::CloseBracket, shift),
        key(Key::Y, shift),
    ];
    for read_only in [false, true] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text("item");
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        editor.clear_history();
        frame_options(&context, &mut editor, host.to_vec(), true, read_only);
        assert_eq!(
            context.input(|input| input.events.clone()),
            host.to_vec(),
            "read_only={read_only}"
        );
        assert_eq!(editor.document().plain_text(), "item");
        assert!(!editor.can_undo());

        frame_options(
            &context,
            &mut editor,
            vec![key(Key::CloseBracket, command)],
            true,
            read_only,
        );
        let indent = u8::from(!read_only);
        assert_eq!(
            editor.document().paragraphs()[0].kind(),
            ParagraphKind::Bullet { indent }
        );
        frame_options(
            &context,
            &mut editor,
            vec![key(Key::OpenBracket, command)],
            true,
            read_only,
        );
        assert_eq!(
            editor.document().paragraphs()[0].kind(),
            ParagraphKind::Bullet { indent: 0 }
        );
        assert!(context.input(|input| input.events.is_empty()));
    }

    let physical = |key: Key, physical: Key, modifiers| Event::Key {
        key,
        physical_key: Some(physical),
        pressed: true,
        repeat: false,
        modifiers,
    };
    let context = egui::Context::default();
    let mut editor = Editor::from_text("title");
    frame(
        &context,
        &mut editor,
        vec![physical(Key::Quote, Key::Num4, command)],
        true,
    );
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Heading { level: 4 }
    );
    frame(
        &context,
        &mut editor,
        vec![physical(Key::Slash, Key::Num7, shift)],
        true,
    );
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Ordered {
            indent: 0,
            start: Some(1)
        }
    );
    let semicolon = physical(Key::Semicolon, Key::Z, command);
    frame(&context, &mut editor, vec![semicolon.clone()], true);
    assert!(editor.can_undo());
    assert_eq!(context.input(|input| input.events.clone()), vec![semicolon]);
    frame(
        &context,
        &mut editor,
        vec![physical(Key::Z, Key::Slash, command)],
        true,
    );
    assert_eq!(
        editor.document().paragraphs()[0].kind(),
        ParagraphKind::Heading { level: 4 }
    );
}

#[cfg(feature = "winit")]
#[test]
fn egui_and_winit_resolve_command_chords_alike_across_layouts() {
    use crate::adapter::winit::{CommandModifier, WinitAdapter};
    use winit::{
        event::ElementState,
        keyboard::{Key as WinitKey, KeyCode, ModifiersState, PhysicalKey},
    };
    type LayoutKey = (KeyCode, Key, char, char);
    let layouts: [(&str, &[LayoutKey]); 4] = [
        (
            "US",
            &[
                (KeyCode::KeyB, Key::B, 'b', 'B'),
                (KeyCode::KeyZ, Key::Z, 'z', 'Z'),
                (KeyCode::Digit1, Key::Num1, '1', '!'),
                (KeyCode::Digit7, Key::Num7, '7', '&'),
                (KeyCode::Digit8, Key::Num8, '8', '*'),
                (KeyCode::Digit0, Key::Num0, '0', ')'),
                (KeyCode::BracketLeft, Key::OpenBracket, '[', '{'),
                (KeyCode::BracketRight, Key::CloseBracket, ']', '}'),
            ],
        ),
        (
            "AZERTY",
            &[
                (KeyCode::KeyQ, Key::Q, 'a', 'A'),
                (KeyCode::KeyW, Key::W, 'z', 'Z'),
                (KeyCode::Digit1, Key::Num1, '&', '1'),
                (KeyCode::Digit2, Key::Num2, 'é', '2'),
                (KeyCode::Digit3, Key::Num3, '"', '3'),
                (KeyCode::Digit4, Key::Num4, '\'', '4'),
                (KeyCode::Digit6, Key::Num6, '-', '6'),
                (KeyCode::Digit7, Key::Num7, 'è', '7'),
                (KeyCode::Digit8, Key::Num8, '_', '8'),
                (KeyCode::BracketLeft, Key::OpenBracket, '\0', '\0'),
                (KeyCode::BracketRight, Key::CloseBracket, '$', '£'),
            ],
        ),
        (
            "German",
            &[
                (KeyCode::KeyY, Key::Y, 'z', 'Z'),
                (KeyCode::Digit6, Key::Num6, '6', '&'),
                (KeyCode::Digit7, Key::Num7, '7', '/'),
                (KeyCode::Digit8, Key::Num8, '8', '('),
                (KeyCode::BracketLeft, Key::OpenBracket, 'ü', 'Ü'),
                (KeyCode::BracketRight, Key::CloseBracket, '+', '*'),
            ],
        ),
        (
            "Russian",
            &[
                (KeyCode::KeyB, Key::B, 'и', 'И'),
                (KeyCode::KeyZ, Key::Z, 'я', 'Я'),
                (KeyCode::Digit7, Key::Num7, '7', '?'),
                (KeyCode::BracketLeft, Key::OpenBracket, 'х', 'Х'),
                (KeyCode::BracketRight, Key::CloseBracket, 'ъ', 'Ъ'),
            ],
        ),
    ];
    let start = || {
        let mut editor = Editor::from_text("one\ntwo\nthree");
        editor.select_all();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
            .unwrap();
        editor
            .set_selection(Selection::caret(Position::new(1, 1)))
            .unwrap();
        editor.insert_text("x").unwrap();
        editor
            .set_selection(Selection::new(Position::new(1, 0), Position::new(1, 2)))
            .unwrap();
        editor
    };
    for (layout, keys) in layouts {
        for &(code, position, plain, shifted) in keys {
            for shift in [false, true] {
                let character = if shift { shifted } else { plain };
                let name = (character != '\0').then(|| character.to_string());
                let mut winit_editor = start();
                let mut adapter = WinitAdapter::new().command_modifier(CommandModifier::Control);
                let _ = adapter.set_focused(true, &mut winit_editor);
                adapter.set_modifiers(if shift {
                    ModifiersState::CONTROL | ModifiersState::SHIFT
                } else {
                    ModifiersState::CONTROL
                });
                let logical = name.as_ref().map_or(WinitKey::Dead(None), |name| {
                    WinitKey::Character(name.into())
                });
                let outcome = adapter
                    .handle_key(
                        &logical,
                        PhysicalKey::Code(code),
                        ElementState::Pressed,
                        None,
                        &mut winit_editor,
                    )
                    .unwrap();

                let key = name.as_deref().and_then(Key::from_name).unwrap_or(position);
                let mut egui_editor = start();
                let handled = handle_key(
                    &mut egui_editor,
                    key,
                    Some(position),
                    Modifiers {
                        ctrl: true,
                        command: true,
                        shift,
                        ..Modifiers::NONE
                    },
                    &ParagraphLayouts::default(),
                    &mut None,
                    &mut Vec::new(),
                    &input_options(false),
                );
                let chord = format!("{layout} {character:?} shift={shift}");
                if !matches!(key, Key::C | Key::X | Key::V) {
                    assert_eq!(handled, outcome.handled, "{chord}");
                }
                assert_eq!(
                    egui_editor.document().to_bytes(),
                    winit_editor.document().to_bytes(),
                    "{chord}"
                );
                assert_eq!(egui_editor.selection(), winit_editor.selection(), "{chord}");
                assert_eq!(
                    egui_editor.typing_style(),
                    winit_editor.typing_style(),
                    "{chord}"
                );
            }
        }
    }
}

#[test]
fn hint_uses_the_first_paragraph_indent_font_and_wrap_width() {
    let context = egui::Context::default();
    let shapes = |editor: &mut Editor| {
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            RichTextEditor::new(editor)
                .hint_text("Write something")
                .show(ui);
        });
        output.textures_delta.clear();
        output
            .shapes
            .into_iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Text(text) => Some((
                    text.galley.job.text.clone(),
                    text.galley.rect.translate(text.pos.to_vec2()),
                    text.galley.clone(),
                )),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let mut editor = Editor::default();
    editor
        .set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })
        .unwrap();
    let painted = shapes(&mut editor);
    let marker = painted.iter().find(|(text, ..)| text == "•").unwrap().1;
    let hint = painted
        .iter()
        .find(|(text, ..)| text == "Write something")
        .unwrap()
        .1;
    assert!(
        hint.left() >= marker.right(),
        "{hint:?} overlaps {marker:?}"
    );

    let mut editor = Editor::default();
    editor
        .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
        .unwrap();
    let painted = shapes(&mut editor);
    let row = |text: &str| {
        painted
            .iter()
            .find(|(painted, ..)| painted == text)
            .unwrap()
            .2
            .rows[0]
            .rect()
            .height()
    };
    assert_eq!(row("Write something"), row(""));
}

#[test]
fn double_and_triple_clicks_select_the_word_and_paragraph_on_release() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("first second third\nnext");
    let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), false, unchanged);
    let point = caret_point(&context, widget.response.rect, &editor, Position::new(0, 8));
    let mut time = 0.1;
    let mut click = |editor: &mut Editor| {
        for pressed in [true, false] {
            widget_frame(
                &context,
                editor,
                vec![
                    Event::PointerMoved(point),
                    primary(point, pressed, Modifiers::NONE),
                ],
                Some(time),
                false,
                unchanged,
            );
            time += 0.05;
        }
        editor.selected_text()
    };
    assert_eq!(click(&mut editor), "");
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 8)));
    assert_eq!(click(&mut editor), "second");
    assert_eq!(click(&mut editor), "first second third");
}

#[test]
fn double_clicking_the_right_half_of_a_character_selects_its_word() {
    for (text, end, expected) in [
        ("hello world", 5, "hello"),
        ("hello, world", 5, "hello"),
        ("first second third", 12, "second"),
        ("hello world", 6, " "),
    ] {
        let context = egui::Context::default();
        let mut editor = Editor::from_text(text);
        let (_, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), false, unchanged);
        let rect = widget.response.rect;
        let left = caret_point(&context, rect, &editor, Position::new(0, end - 1));
        let right = caret_point(&context, rect, &editor, Position::new(0, end));
        let point = left + (right - left) * 0.75 - Vec2::new(1.0, 0.0);
        let mut time = 0.1;
        for _ in 0..2 {
            for pressed in [true, false] {
                widget_frame(
                    &context,
                    &mut editor,
                    vec![
                        Event::PointerMoved(point),
                        primary(point, pressed, Modifiers::NONE),
                    ],
                    Some(time),
                    false,
                    unchanged,
                );
                time += 0.05;
            }
        }
        assert_eq!(editor.selected_text(), expected, "text={text:?}, end={end}");
    }
}

#[test]
fn shift_enter_starts_a_new_row_inside_the_paragraph() {
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut editor = Editor::from_text("");
    frame(&context, &mut editor, vec![], true);
    let output = frame(
        &context,
        &mut editor,
        vec![
            Event::Text("ab".into()),
            key(Key::Enter, Modifiers::SHIFT),
            Event::Text("cd".into()),
        ],
        true,
    );
    assert!(output.platform_output.events.iter().any(|event| {
        matches!(event, egui::output::OutputEvent::TextSelectionChanged(info)
                if info.current_text_value.as_deref() == Some("ab\ncd")
                && info.text_selection == Some(CharIndex(5)..CharIndex(5)))
    }));
    assert_eq!(editor.document().paragraphs().len(), 1);
    assert_eq!(
        editor.document().paragraph(0).unwrap().text(),
        "ab\u{2028}cd"
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(0, 7)));
    assert_eq!(editor.undo_len(), 1);
    for (key_press, expected) in [
        (key(Key::Home, Modifiers::NONE), 5),
        (key(Key::ArrowUp, Modifiers::NONE), 0),
        (key(Key::End, Modifiers::NONE), 2),
        (key(Key::ArrowRight, Modifiers::NONE), 5),
        (key(Key::ArrowLeft, Modifiers::NONE), 2),
        (key(Key::Home, Modifiers::NONE), 0),
        (key(Key::ArrowDown, Modifiers::NONE), 5),
        (key(Key::End, Modifiers::SHIFT), 7),
    ] {
        frame(&context, &mut editor, vec![key_press.clone()], true);
        assert_eq!(
            editor.selection().focus,
            Position::new(0, expected),
            "{key_press:?}"
        );
    }
    assert_eq!(editor.selected_text(), "cd");

    frame_options(
        &context,
        &mut editor,
        vec![key(Key::Enter, Modifiers::SHIFT)],
        true,
        true,
    );
    assert_eq!(
        editor.document().paragraph(0).unwrap().text(),
        "ab\u{2028}cd"
    );
    assert!(context.input(|input| input.events.is_empty()));
}

#[test]
fn soft_line_breaks_lay_out_rows_for_carets_and_hits() {
    let context = egui::Context::default();
    let editor = Editor::from_text("ab\u{2028}cd\u{2028}\u{2028}e\nnext");
    let mut cache = Cache::default();
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance());
    let layout = layouts.get(0).unwrap();
    let galley = &layout.galley;
    assert_eq!(galley.job.text, "ab\ncd\n\ne");
    assert_eq!(galley.end().index.0, 8);
    assert_eq!(galley.rows.len(), 4);
    assert!(galley.rows[..3].iter().all(|row| row.ends_with_newline));
    let caret = |byte, prefer_next_row| {
        document_caret_rect(
            editor.document(),
            &layouts,
            Pos2::ZERO,
            Position::new(0, byte),
            prefer_next_row,
        )
    };
    let row_top = |row: usize| layout.rect.top() + galley.rows[row].min_y();
    for prefer_next_row in [false, true] {
        let before = caret(2, prefer_next_row);
        assert_eq!(before.top(), row_top(0));
        assert_eq!(
            before.left(),
            layout.rect.left() + galley.rows[0].rect().right()
        );
        assert_eq!(caret(5, prefer_next_row).top(), row_top(1));
        assert_eq!(caret(10, prefer_next_row).top(), row_top(2));
        assert_eq!(caret(13, prefer_next_row).top(), row_top(3));
    }
    let point = |row: usize, x: f32| {
        let row = &galley.rows[row];
        layout.rect.min.to_vec2() + Vec2::new(x, row.rect().center().y)
    };
    let past = galley.rows[0].rect().right() + 20.0;
    assert_eq!(
        hit_test(&editor, &layouts, point(0, past)),
        (Position::new(0, 2), false)
    );
    assert_eq!(
        hit_test(&editor, &layouts, point(2, past)).0,
        Position::new(0, 10)
    );
    assert_eq!(
        hit_test(&editor, &layouts, point(1, 0.0)).0,
        Position::new(0, 5)
    );
}

#[test]
fn selection_and_highlights_paint_soft_line_breaks_at_row_ends() {
    let context = egui::Context::default();
    let editor = Editor::from_text("ab\u{2028}cd\u{2028}\u{2028}e");
    let mut cache = Cache::default();
    let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance());
    let rows = &layouts.get(0).unwrap().galley.rows;
    let origin = Pos2::new(10.0, 10.0);
    let color = Color32::from_rgb(4, 5, 6);
    let range = |start, end| Position::new(0, start)..Position::new(0, end);
    let rectangles = |selection: Option<Selection>, highlights: &[Range<Position>]| {
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            paint_document(
                ui.painter(),
                &mut PaintCache::default(),
                editor.document(),
                &layouts,
                origin,
                &[(highlights, color)],
                selection.map(|selection| (selection, color)),
                &FontFamily::Proportional,
                Color32::WHITE,
                Color32::GRAY,
            );
        });
        output.textures_delta.clear();
        output
            .shapes
            .into_iter()
            .filter_map(|clipped| match clipped.shape {
                egui::epaint::Shape::Rect(rectangle) if rectangle.fill == color => {
                    Some(rectangle.rect)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    let all = rectangles(
        Some(Selection::new(Position::default(), editor.document().end())),
        &[],
    );
    assert_eq!(all.len(), 4);
    for (index, (rect, row)) in all.iter().zip(rows).enumerate() {
        let extension = if index < 3 { 5.0 } else { 0.0 };
        assert_eq!(rect.top(), origin.y + row.rect().top());
        assert_eq!(rect.right(), origin.x + row.rect().right() + extension);
    }
    assert_eq!(rectangles(None, &[range(0, 14)]), all);
    let second = rectangles(
        Some(Selection::new(Position::new(0, 5), Position::new(0, 7))),
        &[],
    );
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].right(), origin.x + rows[1].rect().right());
    let only = rectangles(None, &[range(2, 5)]);
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].left(), origin.x + rows[0].rect().right());
    assert_eq!(only[0].width(), 5.0);
}

#[test]
fn clipped_highlights_and_selection_paint_the_unclipped_rows_inside_the_clip() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text(&format!(
        "{}\nmatch tail\n\nmatch",
        "café 👩‍💻 match नमस्ते 🇺🇸 \u{2028}".repeat(16)
    ));
    let mut cache = Cache::default();
    let mut appearance = appearance();
    appearance.width = 120.0;
    let mut retained = PaintCache::default();
    let colors = [
        Color32::from_rgb(1, 2, 3),
        Color32::from_rgb(4, 5, 6),
        Color32::from_rgb(7, 8, 9),
        Color32::from_rgb(10, 11, 12),
    ];
    let origin = Pos2::new(10.0, 30.0);
    for edit in [false, true] {
        if edit {
            editor
                .set_selection(Selection::caret(Position::new(0, 1)))
                .unwrap();
            editor.insert_text("inserted words ").unwrap();
        }
        let layouts = cached_layout_frame(&context, &mut cache, &editor, &appearance);
        let document = editor.document();
        let rows = layouts.get(0).unwrap().galley.rows.len();
        assert!(rows > 30, "rows={rows}");
        let found = document.find("match", crate::SearchOptions::default());
        let tail = document.paragraph(1).unwrap().text().len();
        let spans = [
            found[found.len() / 2].start..Position::new(1, 2),
            Position::new(1, tail)..Position::new(2, 0),
            Position::new(2, 0)..Position::new(3, 3),
        ];
        let scalars: Vec<_> = document
            .paragraph(0)
            .unwrap()
            .text()
            .char_indices()
            .map(|(byte, scalar)| {
                Position::new(0, byte)..Position::new(0, byte + scalar.len_utf8())
            })
            .collect();
        let paint = |paint_cache: &mut PaintCache, clip: Rect, selection: Selection| {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                paint_document(
                    &ui.painter().with_clip_rect(clip),
                    paint_cache,
                    document,
                    &layouts,
                    origin,
                    &[
                        (&found, colors[0]),
                        (&spans, colors[1]),
                        (&scalars, colors[3]),
                    ],
                    Some((selection, colors[2])),
                    &FontFamily::Proportional,
                    Color32::WHITE,
                    Color32::GRAY,
                );
            });
            output.textures_delta.clear();
            output
                .shapes
                .into_iter()
                .filter_map(|clipped| match clipped.shape {
                    egui::epaint::Shape::Rect(rectangle) if colors.contains(&rectangle.fill) => {
                        Some((rectangle.fill, rectangle.rect))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let bottom = origin.y + layouts.content_height().unwrap() + 20.0;
        for selection in [
            Selection::new(found[3].end, found[found.len() - 4].start),
            Selection::new(Position::default(), document.end()),
        ] {
            let full = paint(&mut PaintCache::default(), Rect::EVERYTHING, selection);
            assert!(full.len() > rows, "edit={edit}");
            let mut top = 0.5;
            while top < bottom {
                for height in [0.5, 6.0, 23.0, 80.0] {
                    let clip = Rect::from_min_size(Pos2::new(0.0, top), Vec2::new(1000.0, height));
                    let expected: Vec<_> = full
                        .iter()
                        .copied()
                        .filter(|(_, rect)| {
                            rect.bottom() >= clip.top() - 5.0 && rect.top() <= clip.bottom() + 5.0
                        })
                        .collect();
                    assert_eq!(
                        paint(&mut retained, clip, selection),
                        expected,
                        "edit={edit}, clip={clip:?}"
                    );
                }
                top += 9.0;
            }
        }
    }
}

#[test]
fn ime_preview_lays_out_soft_line_breaks_in_its_preedit() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("a\u{2028}b");
    editor
        .set_selection(Selection::caret(Position::new(0, 4)))
        .unwrap();
    editor.update_composition("x\u{2028}y", Some(4..4)).unwrap();
    let mut cache = Cache::default();
    let layouts = cached_preview_frame(&context, &mut cache, &editor, &appearance());
    let layout = layouts.get(0).unwrap();
    assert_eq!(layout.galley.job.text, "a\nx\nyb");
    assert_eq!(layout.galley.rows.len(), 3);
    let preview = cache.preview.as_ref().unwrap();
    assert_eq!(preview.document.paragraphs().len(), 1);
    assert_eq!(preview.caret, Position::new(0, 8));
    let caret = document_caret_rect(&preview.document, &layouts, Pos2::ZERO, preview.caret, true);
    assert_eq!(
        caret.top(),
        layout.rect.top() + layout.galley.rows[2].min_y()
    );
    assert_eq!(editor.document().paragraph(0).unwrap().text(), "a\u{2028}b");
}

fn link_frame(
    context: &egui::Context,
    editor: &mut Editor,
    events: Vec<Event>,
    time: f64,
    modifiers: Modifiers,
    read_only: bool,
) -> (egui::FullOutput, RichTextEditorOutput) {
    let id = Id::new(WIDGET_ID);
    let mut widget = None;
    let mut events = events;
    if context.input(|input| input.modifiers) != modifiers {
        events.insert(0, Event::ModifiersChanged(modifiers));
    }
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600.0, 400.0))),
            events,
            time: Some(time),
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let output = RichTextEditor::new(editor)
                    .id(id)
                    .read_only(read_only)
                    .show(ui);
                assert!(output.errors.is_empty(), "{:?}", output.errors);
                widget = Some(output);
            });
        },
    );
    output.textures_delta.clear();
    (output, widget.expect("widget shown"))
}

fn linked_widget_editor() -> (Editor, Link) {
    let link = Link::new("https://example.com/docs").unwrap();
    let mut editor = Editor::from_text("Visit example now");
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 13)))
        .unwrap();
    editor.set_link(Some(link.clone())).unwrap();
    editor.set_selection(Selection::default()).unwrap();
    (editor, link)
}

#[test]
fn command_link_click_keeps_its_press_modifiers_and_release_position() {
    let context = egui::Context::default();
    let (mut editor, link) = linked_widget_editor();
    let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
    let on_link = caret_point(&context, widget.response.rect, &editor, Position::new(0, 8));
    let off_link = caret_point(&context, widget.response.rect, &editor, Position::new(0, 2));
    let selection = editor.selection();
    link_frame(
        &context,
        &mut editor,
        vec![
            Event::ModifiersChanged(Modifiers::COMMAND),
            Event::PointerMoved(on_link),
            primary(on_link, true, Modifiers::COMMAND),
            Event::ModifiersChanged(Modifiers::NONE),
        ],
        0.1,
        Modifiers::NONE,
        false,
    );
    assert_eq!(editor.selection(), selection);
    let (_, released) = link_frame(
        &context,
        &mut editor,
        vec![
            primary(on_link, false, Modifiers::NONE),
            Event::PointerMoved(off_link),
        ],
        0.2,
        Modifiers::NONE,
        false,
    );
    assert_eq!(released.clicked_link, Some(link));
    assert_eq!(editor.selection(), selection);
}

#[test]
fn ctrl_clicks_report_links_without_moving_the_caret_and_plain_clicks_edit() {
    let context = egui::Context::default();
    let (mut editor, link) = linked_widget_editor();
    let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
    let rect = widget.response.rect;
    let on_link = caret_point(&context, rect, &editor, Position::new(0, 8));
    let off_link = caret_point(&context, rect, &editor, Position::new(0, 2));

    let mut tooltip = false;
    for (index, modifiers, cursor) in [
        (0, Modifiers::NONE, egui::CursorIcon::Text),
        (1, Modifiers::COMMAND, egui::CursorIcon::PointingHand),
        (2, Modifiers::COMMAND, egui::CursorIcon::PointingHand),
    ] {
        let events = if index == 0 {
            vec![Event::PointerMoved(on_link)]
        } else {
            Vec::new()
        };
        let (output, widget) = link_frame(
            &context,
            &mut editor,
            events,
            1.0 + f64::from(index),
            modifiers,
            false,
        );
        assert_eq!(output.platform_output.cursor_icon, cursor, "{index}");
        assert_eq!(widget.clicked_link, None);
        tooltip |= output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::epaint::Shape::Text(text)
                if text.galley.job.text == link.as_str())
        });
    }
    assert!(tooltip, "hovering a link shows its target");
    let (output, _) = link_frame(
        &context,
        &mut editor,
        vec![Event::PointerMoved(off_link)],
        4.0,
        Modifiers::COMMAND,
        false,
    );
    assert_eq!(output.platform_output.cursor_icon, egui::CursorIcon::Text);

    let selection = editor.selection();
    let (_, pressed) = link_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(on_link),
            primary(on_link, true, Modifiers::COMMAND),
        ],
        5.0,
        Modifiers::COMMAND,
        false,
    );
    assert_eq!(pressed.clicked_link, None);
    assert_eq!(editor.selection(), selection);
    let (_, released) = link_frame(
        &context,
        &mut editor,
        vec![primary(on_link, false, Modifiers::COMMAND)],
        5.05,
        Modifiers::COMMAND,
        false,
    );
    assert_eq!(released.clicked_link, Some(link.clone()));
    assert_eq!(editor.selection(), selection);
    assert!(context.memory(|memory| memory.has_focus(Id::new(WIDGET_ID))));

    for (time, point, modifiers, caret) in [
        (7.0, on_link, Modifiers::NONE, 8),
        (9.0, off_link, Modifiers::COMMAND, 2),
    ] {
        for (offset, pressed) in [(0.0, true), (0.05, false)] {
            let (_, widget) = link_frame(
                &context,
                &mut editor,
                vec![
                    Event::PointerMoved(point),
                    primary(point, pressed, modifiers),
                ],
                time + offset,
                modifiers,
                false,
            );
            assert_eq!(widget.clicked_link, None);
        }
        assert_eq!(
            editor.selection(),
            Selection::caret(Position::new(0, caret))
        );
    }

    let end = caret_point(&context, rect, &editor, Position::new(0, 17));
    for (index, events) in [
        vec![
            Event::PointerMoved(on_link),
            primary(on_link, true, Modifiers::COMMAND),
        ],
        vec![Event::PointerMoved(on_link + Vec2::new(12.0, 0.0))],
        vec![Event::PointerMoved(end)],
        vec![primary(end, false, Modifiers::COMMAND)],
    ]
    .into_iter()
    .enumerate()
    {
        let (_, widget) = link_frame(
            &context,
            &mut editor,
            events,
            11.0 + index as f64 * 0.05,
            Modifiers::COMMAND,
            false,
        );
        assert_eq!(widget.clicked_link, None);
    }
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(0, 8), Position::new(0, 17))
    );
}

#[test]
fn link_drags_discard_anchors_from_a_changed_document() {
    let context = egui::Context::default();
    let (mut editor, _) = linked_widget_editor();
    let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
    let point = caret_point(&context, widget.response.rect, &editor, Position::new(0, 8));
    link_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(point),
            primary(point, true, Modifiers::COMMAND),
        ],
        1.0,
        Modifiers::COMMAND,
        false,
    );
    editor.select_all();
    editor.insert_text("new").unwrap();
    let (_, widget) = link_frame(
        &context,
        &mut editor,
        vec![Event::PointerMoved(point + Vec2::new(30.0, 0.0))],
        1.1,
        Modifiers::COMMAND,
        false,
    );
    assert_eq!(widget.clicked_link, None);
    assert_eq!(
        editor.selection(),
        Selection::caret(editor.document().end())
    );
}

#[test]
fn link_releases_discard_presses_from_a_changed_document() {
    let context = egui::Context::default();
    let (mut editor, link) = linked_widget_editor();
    let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
    let point = caret_point(&context, widget.response.rect, &editor, Position::new(0, 8));
    link_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(point),
            primary(point, true, Modifiers::COMMAND),
        ],
        1.0,
        Modifiers::COMMAND,
        false,
    );
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 13)))
        .unwrap();
    editor.insert_text("updated").unwrap();
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 13)))
        .unwrap();
    editor.set_link(Some(link.clone())).unwrap();
    assert_eq!(editor.document().link_at(Position::new(0, 8)), Some(&link));
    let selection = editor.selection();
    let (_, widget) = link_frame(
        &context,
        &mut editor,
        vec![primary(point, false, Modifiers::COMMAND)],
        1.1,
        Modifiers::COMMAND,
        false,
    );
    assert_eq!(widget.clicked_link, None);
    assert_eq!(editor.selection(), selection);
}

#[test]
fn link_drags_discard_presses_when_ime_changes_the_document_in_the_drag_frame() {
    for text in ["日本語", "xy"] {
        for queued in [true, false] {
            let context = egui::Context::default();
            let (mut editor, _) = linked_widget_editor();
            let (_, widget) =
                link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
            let point = caret_point(&context, widget.response.rect, &editor, Position::new(0, 8));
            link_frame(
                &context,
                &mut editor,
                vec![
                    Event::PointerMoved(point),
                    primary(point, true, Modifiers::COMMAND),
                ],
                1.0,
                Modifiers::COMMAND,
                false,
            );
            assert!(widget_cache(&context).interaction.link_press.is_some());
            let mut events = Vec::new();
            if queued {
                events.push(Event::Ime(ImeEvent::Commit(text.into())));
            } else {
                link_frame(
                    &context,
                    &mut editor,
                    vec![Event::Ime(ImeEvent::Preedit {
                        text: text.into(),
                        active_range_chars: None,
                    })],
                    1.05,
                    Modifiers::COMMAND,
                    false,
                );
                assert!(editor.composition().is_some());
            }
            events.push(Event::PointerMoved(point + Vec2::new(200.0, 0.0)));
            link_frame(
                &context,
                &mut editor,
                events,
                1.1,
                Modifiers::COMMAND,
                false,
            );
            assert_eq!(
                editor.document().plain_text(),
                format!("{text}Visit example now")
            );
            assert_eq!(
                editor.selection().anchor,
                Position::new(0, text.len()),
                "{text} queued={queued}"
            );
        }
    }
}

#[test]
fn read_only_clicks_and_accessibility_clicks_follow_links() {
    use egui::accesskit::{Action, ActionRequest, Role, TreeId};
    let context = egui::Context::default();
    let (mut editor, link) = linked_widget_editor();
    let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, true);
    let on_link = caret_point(
        &context,
        widget.response.rect,
        &editor,
        Position::new(0, 10),
    );
    let (output, _) = link_frame(
        &context,
        &mut editor,
        vec![Event::PointerMoved(on_link)],
        1.0,
        Modifiers::NONE,
        true,
    );
    assert_eq!(
        output.platform_output.cursor_icon,
        egui::CursorIcon::PointingHand
    );
    let selection = editor.selection();
    let mut clicked = Vec::new();
    for (time, pressed) in [(2.0, true), (2.05, false)] {
        let (_, widget) = link_frame(
            &context,
            &mut editor,
            vec![primary(on_link, pressed, Modifiers::NONE)],
            time,
            Modifiers::NONE,
            true,
        );
        clicked.push(widget.clicked_link);
    }
    assert_eq!(clicked, [None, Some(link.clone())]);
    assert_eq!(editor.selection(), selection);

    let context = egui::Context::default();
    context.enable_accesskit();
    for read_only in [false, true] {
        let (mut output, _) = link_frame(
            &context,
            &mut editor,
            vec![],
            3.0,
            Modifiers::NONE,
            read_only,
        );
        let update = output.platform_output.accesskit_update.take().unwrap();
        let (node_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Link)
            .expect("link node");
        let request = ActionRequest {
            action: Action::Click,
            target_node: *node_id,
            target_tree: TreeId::ROOT,
            data: None,
        };
        let revision = editor.document().revision();
        let (_, widget) = link_frame(
            &context,
            &mut editor,
            vec![Event::AccessKitActionRequest(request)],
            3.1,
            Modifiers::NONE,
            read_only,
        );
        assert_eq!(widget.clicked_link, Some(link.clone()));
        assert_eq!(editor.document().revision(), revision);
        assert_eq!(editor.selection(), selection);
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn stale_accessibility_link_clicks_are_rejected() {
    use egui::accesskit::{Action, ActionRequest, Role, TreeId};
    for edit_in_batch in [false, true] {
        let context = egui::Context::default();
        context.enable_accesskit();
        let (mut editor, _) = linked_widget_editor();
        let (output, _) = link_frame(&context, &mut editor, vec![], 0.0, Modifiers::NONE, false);
        let update = output.platform_output.accesskit_update.unwrap();
        let (node_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| node.role() == Role::Link)
            .unwrap();
        let mut events = Vec::new();
        if edit_in_batch {
            context.memory_mut(|memory| memory.request_focus(Id::new(WIDGET_ID)));
            events.push(Event::Text("prefix ".into()));
        } else {
            editor.select_all();
            editor.set_link(None).unwrap();
        }
        events.push(Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click,
            target_node: *node_id,
            target_tree: TreeId::ROOT,
            data: None,
        }));
        let (_, widget) = link_frame(&context, &mut editor, events, 1.0, Modifiers::NONE, false);
        assert_eq!(widget.clicked_link, None);
        assert_eq!(
            widget.accessibility_errors,
            [AccessibilityError::StaleDocument]
        );
        assert!(context.input(|input| input.events.is_empty()));
    }
}

#[test]
fn clicks_pressed_and_released_in_one_frame_follow_links_and_place_the_caret() {
    for (modifiers, read_only, byte, follows) in [
        (Modifiers::COMMAND, false, 8, true),
        (Modifiers::NONE, true, 8, true),
        (Modifiers::NONE, false, 8, false),
        (Modifiers::COMMAND, false, 2, false),
    ] {
        let context = egui::Context::default();
        let (mut editor, link) = linked_widget_editor();
        let (_, widget) = link_frame(&context, &mut editor, vec![], 0.0, modifiers, read_only);
        let point = caret_point(
            &context,
            widget.response.rect,
            &editor,
            Position::new(0, byte),
        );
        let (_, widget) = link_frame(
            &context,
            &mut editor,
            vec![
                Event::PointerMoved(point),
                primary(point, true, modifiers),
                primary(point, false, modifiers),
            ],
            1.0,
            modifiers,
            read_only,
        );
        let case = format!("{modifiers:?} read_only={read_only} byte={byte}");
        if follows {
            assert_eq!(widget.clicked_link, Some(link), "{case}");
            assert_eq!(editor.selection(), Selection::default(), "{case}");
        } else {
            assert_eq!(widget.clicked_link, None, "{case}");
            assert_eq!(
                editor.selection(),
                Selection::caret(Position::new(0, byte)),
                "{case}"
            );
        }
        let (_, next) = link_frame(&context, &mut editor, vec![], 1.1, modifiers, read_only);
        assert_eq!(next.clicked_link, None, "{case}");
    }

    let context = egui::Context::default();
    let mut editor = Editor::from_text("abc def");
    let rect = composing_frame(&context, &mut editor, "ni");
    let point = caret_point(&context, rect, &editor, Position::new(0, "niabc ".len()));
    widget_frame(
        &context,
        &mut editor,
        vec![
            Event::PointerMoved(point),
            primary(point, true, Modifiers::NONE),
            primary(point, false, Modifiers::NONE),
        ],
        Some(0.2),
        true,
        unchanged,
    );
    assert_eq!(editor.document().plain_text(), "niabc def");
    assert!(editor.composition().is_none());
    assert_eq!(
        editor.selection(),
        Selection::caret(Position::new(0, "niabc ".len()))
    );
}

#[test]
fn quote_bars_paint_beside_quotes_whose_indent_pointer_and_caret_follow() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("intro\none\ntwo words\nbody\nthree");
    for paragraphs in [1..3, 4..5] {
        editor
            .set_selection(Selection::new(
                Position::new(paragraphs.start, 0),
                Position::new(paragraphs.end - 1, 1),
            ))
            .unwrap();
        editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    }
    let (output, widget) = widget_frame(&context, &mut editor, vec![], Some(0.0), true, unchanged);
    let origin = widget.response.rect.min + Vec2::splat(PADDING);
    let layouts = widget_cache(&context).layout.layouts;
    let rect = |index| layouts.get(index).unwrap().rect.translate(origin.to_vec2());
    let quote_color = context.global_style().visuals.weak_text_color();
    let bars: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Rect(shape)
                if shape.fill == quote_color && shape.rect.width() < PADDING =>
            {
                Some(shape.rect)
            }
            _ => None,
        })
        .collect();
    assert_eq!(bars.len(), 2, "{bars:?}");
    assert_eq!(bars[0].top(), rect(1).top());
    assert_eq!(bars[0].bottom(), rect(2).bottom());
    assert_eq!(bars[1].y_range(), rect(4).y_range());
    for bar in &bars {
        assert!(bar.left() > origin.x && bar.right() < rect(1).left());
    }
    assert!(rect(1).left() > rect(0).left());
    assert_eq!(rect(3).left(), rect(0).left());

    let mut time = 0.1;
    let mut click = |editor: &mut Editor, point: Pos2| {
        let mut caret = Rect::NOTHING;
        for pressed in [true, false] {
            let (_, widget) = widget_frame(
                &context,
                editor,
                vec![
                    Event::PointerMoved(point),
                    primary(point, pressed, Modifiers::NONE),
                ],
                Some(time),
                true,
                unchanged,
            );
            caret = widget.caret_rect;
            time += 1.0;
        }
        caret
    };
    let caret = click(&mut editor, Pos2::new(origin.x + 1.0, rect(2).center().y));
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 0)));
    assert_eq!(caret.left(), rect(2).left());
    let point = caret_point(&context, widget.response.rect, &editor, Position::new(2, 4));
    let caret = click(&mut editor, point);
    assert_eq!(editor.selection(), Selection::caret(Position::new(2, 4)));
    assert!(caret.left() > rect(2).left());

    let text_left = rect(1).left();
    editor
        .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 3)))
        .unwrap();
    let selection_color = context.global_style().visuals.selection.bg_fill;
    let (output, _) = widget_frame(&context, &mut editor, vec![], Some(time), true, unchanged);
    let selected: Vec<_> = output
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::epaint::Shape::Rect(shape) if shape.fill == selection_color => Some(shape.rect),
            _ => None,
        })
        .collect();
    assert_eq!(selected.len(), 2);
    assert!(selected.iter().all(|selected| selected.left() == text_left));
}

#[test]
fn enter_and_backspace_leave_quotes_in_the_widget() {
    let context = egui::Context::default();
    let mut editor = Editor::from_text("quoted");
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    editor
        .set_selection(Selection::caret(editor.document().end()))
        .unwrap();
    frame(&context, &mut editor, vec![], true);
    let kinds = |editor: &Editor| -> Vec<_> {
        editor
            .document()
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.kind())
            .collect()
    };
    frame(
        &context,
        &mut editor,
        vec![key(Key::Enter, Modifiers::NONE)],
        true,
    );
    assert_eq!(kinds(&editor), [ParagraphKind::Quote; 2]);
    frame(
        &context,
        &mut editor,
        vec![key(Key::Enter, Modifiers::NONE)],
        true,
    );
    assert_eq!(kinds(&editor), [ParagraphKind::Quote, ParagraphKind::Body]);
    frame(
        &context,
        &mut editor,
        vec![
            Event::Text("x".into()),
            key(Key::ArrowLeft, Modifiers::NONE),
        ],
        true,
    );
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    frame(
        &context,
        &mut editor,
        vec![key(Key::Backspace, Modifiers::NONE)],
        true,
    );
    assert_eq!(kinds(&editor), [ParagraphKind::Quote, ParagraphKind::Body]);
    assert_eq!(editor.document().plain_text(), "quoted\nx");
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    frame(&context, &mut editor, vec![], true);
    frame(
        &context,
        &mut editor,
        vec![key(Key::Tab, Modifiers::NONE)],
        true,
    );
    assert_eq!(kinds(&editor), [ParagraphKind::Quote; 2]);
    assert_eq!(editor.document().plain_text(), "quoted\nx");
}
