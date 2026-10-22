//! Tests the AccessKit tree that `AccessKitAdapter` publishes by reading it
//! back through `accesskit_consumer`: text, lines, word and paragraph
//! navigation, formatting runs, links, blockquotes, selection actions, and
//! incremental updates that stay consistent with the editor.

#![cfg(feature = "accesskit")]

use std::ops::Range;

use accesskit::{
    Action, ActionData, ActionRequest, Node as NodeData, NodeId, Role, TextSelection, TreeId,
    TreeInfo as TreeData, TreeUpdate, Uuid,
};
use accesskit_consumer::{
    FilterResult, NodeRef as Node, TextRangePropertyValue, Tree, TreeChangeHandler,
};
use textloom::accessibility::{AccessKitAdapter, AccessKitError, Focus};
use textloom::{Color, Editor, Link, ParagraphKind, Position, Selection, StylePatch};

/// A tree change handler for updates whose individual changes the tests don't inspect.
struct Ignore;

impl TreeChangeHandler for Ignore {
    fn node_added(&mut self, _: &Node) {}
    fn node_updated(&mut self, _: &Node, _: &Node) {}
    fn focus_moved(&mut self, _: Option<&Node>, _: Option<&Node>) {}
    fn node_removed(&mut self, _: &Node) {}
}

fn text_input<'a>(tree: &'a Tree, adapter: &AccessKitAdapter) -> Node<'a> {
    tree.state()
        .node_by_tree_local_id(adapter.editor_node(), TreeId::ROOT)
        .expect("published editor node")
}

fn bold(editor: &mut Editor, range: Range<Position>) {
    editor
        .set_selection(Selection::new(range.start, range.end))
        .expect("callers pass a range between grapheme boundaries of the document");
    editor
        .apply_style(StylePatch {
            bold: Some(true),
            ..StylePatch::default()
        })
        .expect("no IME composition is active");
}

/// Returns the offset of `position` in the document's plain text, in Unicode
/// scalar values, which is how AccessKit addresses text.
fn scalar_index(editor: &Editor, position: Position) -> usize {
    let paragraphs = editor.document().paragraphs();
    paragraphs[..position.paragraph]
        .iter()
        .map(|paragraph| paragraph.text().chars().count() + 1)
        .sum::<usize>()
        + paragraphs[position.paragraph].text()[..position.byte]
            .chars()
            .count()
}

fn select(adapter: &AccessKitAdapter, selection: TextSelection) -> ActionRequest {
    ActionRequest {
        action: Action::SetTextSelection,
        target_tree: TreeId::ROOT,
        target_node: adapter.editor_node(),
        data: Some(ActionData::SetTextSelection(selection)),
    }
}

#[test]
fn styled_runs_share_their_paragraph_line() {
    let mut editor = Editor::from_text("Hello world again\nsecond");
    bold(&mut editor, Position::new(0, 6)..Position::new(0, 11));
    editor
        .set_selection(Selection::caret(Position::new(0, 6)))
        .unwrap();
    let mut adapter = AccessKitAdapter::default();
    let tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    assert_eq!(adapter.text_runs(0).count(), 3);
    let node = text_input(&tree, &adapter);
    assert_eq!(
        node.line_range_from_index(0).unwrap().text(),
        "Hello world again\n"
    );
    assert_eq!(node.line_range_from_index(1).unwrap().text(), "second");
    let start = node.document_start();
    assert_eq!(start.forward_to_line_end().to_global_usv_index(), 18);
    let caret = node.text_selection_focus().unwrap();
    assert_eq!(caret.to_global_usv_index(), 6);
    assert_eq!(caret.to_line_index(), 0);
    assert_eq!(caret.backward_to_line_start().to_global_usv_index(), 0);
}

#[test]
fn fills_are_formatting_boundaries_that_ranges_report() {
    let mut editor = Editor::from_text("plain filled\nnext");
    editor
        .set_selection(Selection::new(Position::new(0, 6), Position::new(0, 12)))
        .unwrap();
    editor
        .apply_style(StylePatch {
            background: Some(Some(Color::YELLOW)),
            ..StylePatch::default()
        })
        .unwrap();
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let node = text_input(&tree, &adapter);
    let yellow = accesskit::Color {
        red: 255,
        green: 255,
        blue: 0,
        alpha: 255,
    };
    let start = node.document_start();
    assert_eq!(start.background_color(), None);
    let fill = start.forward_to_format_start();
    assert_eq!(fill.to_global_usv_index(), 6);
    assert_eq!(fill.background_color(), Some(yellow));
    assert_eq!(fill.forward_to_format_start().to_global_usv_index(), 13);
    let mut filled = fill.to_degenerate_range();
    filled.set_end(node.text_position_from_global_usv_index(12).unwrap());
    assert_eq!(filled.text(), "filled");
    assert_eq!(
        filled.background_color(),
        TextRangePropertyValue::Single(Some(yellow))
    );
    assert_eq!(
        node.document_range().background_color(),
        TextRangePropertyValue::Mixed
    );

    editor.select_all();
    editor
        .apply_style(StylePatch {
            background: Some(None),
            ..StylePatch::default()
        })
        .unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        text_input(&tree, &adapter)
            .document_range()
            .background_color(),
        TextRangePropertyValue::Single(None)
    );
}

#[test]
fn word_navigation_follows_editor_words() {
    let editor = Editor::from_text("Hello world again\nsecond");
    let mut adapter = AccessKitAdapter::default();
    let tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let node = text_input(&tree, &adapter);
    let mut position = node.document_start();
    let mut ends = Vec::new();
    for _ in 0..3 {
        position = position.forward_to_word_end();
        ends.push(position.to_global_usv_index());
    }
    assert_eq!(ends, [6, 12, 18]);
    let inside = node.text_position_from_global_usv_index(17).unwrap();
    assert_eq!(inside.backward_to_word_start().to_global_usv_index(), 12);
}

#[test]
fn word_starts_match_editor_words_across_long_runs() {
    let mut editor = Editor::from_text(&format!(
        "{}\n  h\u{e9}llo, w\u{f6}rld 👩\u{200d}💻 x\n{} tail\n",
        "ab, cd ".repeat(100),
        "\u{e9}".repeat(300)
    ));
    bold(&mut editor, Position::new(0, 8)..Position::new(0, 400));
    let mut adapter = AccessKitAdapter::default();
    let tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(node.document_range().text(), editor.document().plain_text());
    let document_end = node.document_range().text().chars().count();
    let mut paragraph_start = 0;
    for paragraph in editor.document().paragraphs() {
        let boundaries: Vec<_> = (0..=paragraph.grapheme_count())
            .map(|grapheme| paragraph.byte_from_grapheme(grapheme).unwrap())
            .collect();
        let mut starts: Vec<_> = boundaries
            .iter()
            .map(|byte| paragraph.previous_word(*byte))
            .collect();
        starts.dedup();
        let global = |byte: usize| paragraph_start + paragraph.text()[..byte].chars().count();
        for byte in &boundaries {
            let position = node
                .text_position_from_global_usv_index(global(*byte))
                .unwrap();
            if *byte > 0 {
                assert_eq!(
                    position.backward_to_word_start().to_global_usv_index(),
                    global(paragraph.previous_word(*byte)),
                    "backward from {byte} in {:?}",
                    paragraph.text()
                );
            }
            if *byte < paragraph.text().len() {
                let next = starts.iter().find(|start| **start > *byte).map_or_else(
                    || global(paragraph.text().len()) + 1,
                    |start| global(*start),
                );
                let next = next.min(document_end);
                assert_eq!(
                    position.forward_to_word_start().to_global_usv_index(),
                    next,
                    "forward from {byte} in {:?}",
                    paragraph.text()
                );
            }
        }
        paragraph_start = global(paragraph.text().len()) + 1;
    }
}

#[test]
fn invalidated_adapters_reinitialize_platform_trees() {
    let mut editor = Editor::from_text("Hello\nworld");
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let incremental = adapter.update(&editor, "Notes", Focus::Focused).unwrap();
    assert!(incremental.tree.is_none() && incremental.nodes.is_empty());

    adapter.invalidate();
    let restarted = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    assert_eq!(
        text_input(&restarted, &adapter).document_range().text(),
        "Hello\nworld"
    );
    assert!(
        adapter
            .update(&editor, "Notes", Focus::Focused)
            .unwrap()
            .nodes
            .is_empty()
    );

    editor.insert_text("> ").unwrap();
    let _discarded = adapter.update(&editor, "Notes", Focus::Focused).unwrap();
    editor.insert_paragraph().unwrap();
    adapter.invalidate();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        text_input(&tree, &adapter).document_range().text(),
        editor.document().plain_text()
    );
    editor.insert_text("more").unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        text_input(&tree, &adapter).document_range().text(),
        editor.document().plain_text()
    );
}

#[test]
fn oversized_graphemes_keep_the_tree_accessible() {
    let mut editor = Editor::from_text("first\nmiddle\nlast");
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    editor
        .set_selection(Selection::caret(Position::new(1, 3)))
        .unwrap();
    editor
        .insert_text(&format!("x{}", "\u{301}".repeat(200)))
        .unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(node.document_range().text(), editor.document().plain_text());
    assert_eq!(
        node.text_selection_focus().unwrap().to_global_usv_index(),
        scalar_index(&editor, editor.selection().focus)
    );
    let inside = node
        .text_position_from_global_usv_index(scalar_index(&editor, Position::new(1, 3)) + 150)
        .unwrap()
        .to_raw();
    let request = select(
        &adapter,
        TextSelection {
            anchor: inside,
            focus: inside,
        },
    );
    assert!(
        adapter
            .handle_action(&request, &mut editor)
            .unwrap()
            .changed
    );
    assert_eq!(editor.selection(), Selection::caret(Position::new(1, 3)));
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        text_input(&tree, &adapter)
            .text_selection_focus()
            .unwrap()
            .to_global_usv_index(),
        scalar_index(&editor, Position::new(1, 3))
    );
}

#[test]
fn grafted_subtrees_are_transparent_in_the_host_tree() {
    let subtree = TreeId(Uuid::from_u128(1));
    let mut window = NodeData::new(Role::Window);
    window.set_children(vec![NodeId(1)]);
    let mut graft = NodeData::new(Role::GenericContainer);
    graft.set_tree_id(subtree);
    let mut tree = Tree::new(
        TreeUpdate {
            nodes: vec![(NodeId(0), window), (NodeId(1), graft)],
            tree: Some(TreeData::new(NodeId(0))),
            tree_id: TreeId::ROOT,
            focus: NodeId(0),
        },
        true,
    );
    let editor = Editor::from_text("grafted");
    let mut adapter = AccessKitAdapter::default().with_tree_id(subtree);
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    let state = tree.state();
    let input = state
        .node_by_tree_local_id(adapter.editor_node(), subtree)
        .unwrap();
    assert_eq!(input.document_range().text(), "grafted");
    let children: Vec<_> = state
        .root()
        .filtered_children(|node: &Node| {
            if node.role() == Role::Window && node.id() != state.root_id() {
                FilterResult::Include
            } else {
                accesskit_consumer::common_filter(node)
            }
        })
        .map(|node| node.role())
        .collect();
    assert_eq!(children, [Role::MultilineTextInput]);
}

#[test]
fn consumer_tracks_the_editor_through_incremental_updates() {
    let mut editor = Editor::from_text("alpha beta\ngamma\n\ndelta");
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let mut seed = 0x1234_5678_9abc_def0_u64;
    let mut random = |limit: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % limit as u64) as usize
    };
    let long_word = "x".repeat(300);
    let long_cluster = format!("y{}", "\u{301}".repeat(150));
    for step in 0..1500 {
        let paragraphs = editor.document().paragraphs();
        let index = random(paragraphs.len());
        let paragraph = &paragraphs[index];
        let byte = paragraph
            .byte_from_grapheme(random(paragraph.grapheme_count() + 1))
            .unwrap();
        let caret = Position::new(index, byte);
        let _ = editor.set_selection(Selection::caret(caret));
        let _ = match random(14) {
            0 | 1 => editor.insert_text("x"),
            2 => editor.insert_paragraph(),
            3 => editor.delete_backward(),
            4 => editor.delete_forward(),
            5 => editor.insert_text("é👩\u{200d}💻 q\nr\u{2028}\n"),
            11 => editor.insert_line_break(),
            6 => {
                editor.undo();
                Ok(())
            }
            7 => {
                editor.redo();
                Ok(())
            }
            8 => {
                let end = editor.document().end();
                let _ = editor.set_selection(Selection::new(caret, end));
                editor.apply_style(StylePatch {
                    italic: Some(random(2) == 0),
                    ..StylePatch::default()
                })
            }
            9 => editor.insert_text(if random(2) == 0 {
                &long_word
            } else {
                &long_cluster
            }),
            10 => editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }),
            12 => {
                let paragraph = &editor.document().paragraphs()[index];
                let end = paragraph
                    .byte_from_grapheme(random(paragraph.grapheme_count() + 1))
                    .unwrap();
                let end = Position::new(index, end);
                let _ = editor.set_selection(Selection::new(caret.min(end), caret.max(end)));
                let link = ["https://a.example", "https://b.example"][random(2)];
                editor.set_link((random(3) != 0).then(|| Link::try_new(link).unwrap()))
            }
            _ => editor.delete_word_backward(),
        };
        if random(3) == 0 {
            continue;
        }
        if random(50) == 0 {
            adapter.invalidate();
        }
        tree.update_and_process_changes(
            adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
            &mut Ignore,
        );
        let node = text_input(&tree, &adapter);
        assert_eq!(
            node.document_range().text(),
            editor.document().plain_text(),
            "step {step}"
        );
        assert_eq!(
            node.text_selection_anchor().unwrap().to_global_usv_index(),
            scalar_index(&editor, editor.selection().anchor),
            "step {step}"
        );
        assert_eq!(
            node.text_selection_focus().unwrap().to_global_usv_index(),
            scalar_index(&editor, editor.selection().focus),
            "step {step}"
        );
        if step % 25 == 0 {
            let plain = editor.document().plain_text();
            for (line, expected) in plain.split('\n').enumerate() {
                let text = node.line_range_from_index(line).unwrap().text();
                assert_eq!(
                    text.strip_suffix('\n').unwrap_or(&text),
                    expected,
                    "step {step}"
                );
            }
        }
        if random(4) == 0 {
            let total = node.document_range().text().chars().count();
            let target = node
                .text_position_from_global_usv_index(random(total + 1))
                .unwrap();
            let raw = target.to_raw();
            let expected = target.to_global_usv_index();
            let request = select(
                &adapter,
                TextSelection {
                    anchor: raw,
                    focus: raw,
                },
            );
            adapter.handle_action(&request, &mut editor).unwrap();
            let focus = editor.selection().focus;
            assert!(scalar_index(&editor, focus) <= expected, "step {step}");
            assert_eq!(
                adapter.text_position(&editor, focus).map(|_| ()),
                Ok(()),
                "step {step}"
            );
        }
    }
}

#[test]
fn soft_line_breaks_are_lines_within_one_paragraph() {
    let mut editor = Editor::from_text("Hello\u{2028}  world\u{2028}\nnext");
    bold(&mut editor, Position::new(0, 2)..Position::new(0, 10));
    editor
        .set_selection(Selection::caret(Position::new(0, 8)))
        .unwrap();
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(node.document_range().text(), "Hello\n  world\n\nnext");
    assert_eq!(node.document_range().text(), editor.document().plain_text());
    let lines: Vec<_> = (0..4)
        .map(|line| node.line_range_from_index(line).unwrap().text())
        .collect();
    assert_eq!(lines, ["Hello\n", "  world\n", "\n", "next"]);

    let caret = node.text_selection_focus().unwrap();
    assert_eq!(caret.to_global_usv_index(), 6);
    assert_eq!(caret.to_line_index(), 1);
    assert!(caret.is_line_start());
    assert_eq!(caret.forward_to_line_end().to_global_usv_index(), 14);
    assert_eq!(caret.backward_to_character_start().to_global_usv_index(), 5);
    let end_of_first = node.text_position_from_global_usv_index(5).unwrap();
    assert_eq!(end_of_first.to_line_index(), 0);
    assert_eq!(
        end_of_first.backward_to_line_start().to_global_usv_index(),
        0
    );

    let paragraph = editor.document().paragraph(0).unwrap().clone();
    for grapheme in 0..=paragraph.grapheme_count() {
        let position = Position::new(0, paragraph.byte_from_grapheme(grapheme).unwrap());
        editor.set_selection(Selection::caret(position)).unwrap();
        tree.update_and_process_changes(
            adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
            &mut Ignore,
        );
        let node = text_input(&tree, &adapter);
        let focus = node.text_selection_focus().unwrap();
        assert_eq!(
            focus.to_global_usv_index(),
            scalar_index(&editor, position),
            "grapheme {grapheme}"
        );
        let raw = node
            .text_position_from_global_usv_index(scalar_index(&editor, position))
            .unwrap()
            .to_raw();
        editor
            .set_selection(Selection::caret(Position::default()))
            .unwrap();
        adapter
            .handle_action(
                &select(
                    &adapter,
                    TextSelection {
                        anchor: raw,
                        focus: raw,
                    },
                ),
                &mut editor,
            )
            .unwrap();
        assert_eq!(editor.selection().focus, position, "grapheme {grapheme}");
    }

    let node = text_input(&tree, &adapter);
    let hello_end = node.text_position_from_global_usv_index(3).unwrap();
    assert_eq!(hello_end.forward_to_word_start().to_global_usv_index(), 6);
    let world = node.text_position_from_global_usv_index(8).unwrap();
    assert_eq!(world.backward_to_word_start().to_global_usv_index(), 6);
}

/// A published link node, as read back from the AccessKit tree.
#[derive(Debug, PartialEq)]
struct LinkNode {
    url: String,
    label: String,
    /// The concatenated values of the link's text runs.
    text: String,
}

/// Returns the link nodes below `node`, in tree order.
fn link_nodes(node: Node<'_>) -> Vec<LinkNode> {
    let mut links = Vec::new();
    for child in node.children() {
        if child.role() == Role::Link {
            let text = child
                .children()
                .map(|run| {
                    assert_eq!(
                        run.role(),
                        Role::TextRun,
                        "link children should be text runs"
                    );
                    run.data()
                        .value()
                        .expect("the adapter gives every text run a value")
                        .to_owned()
                })
                .collect();
            links.push(LinkNode {
                url: child
                    .url()
                    .expect("the adapter gives every link node a URL")
                    .to_owned(),
                label: child
                    .label()
                    .expect("the adapter labels every link node with its text"),
                text,
            });
        } else {
            links.extend(link_nodes(child));
        }
    }
    links
}

#[test]
fn links_are_link_nodes_around_their_runs_and_keep_text_navigation() {
    let docs = Link::try_new("https://example.com/docs").unwrap();
    let mut editor = Editor::from_text("Read the docs\u{2028}now, then stop\nnext line");
    editor
        .set_selection(Selection::new(Position::new(0, 5), Position::new(0, 19)))
        .unwrap();
    editor.set_link(Some(docs.clone())).unwrap();
    bold(&mut editor, Position::new(0, 9)..Position::new(0, 13));
    editor
        .set_selection(Selection::new(Position::new(1, 5), Position::new(1, 9)))
        .unwrap();
    editor
        .set_link(Some(Link::try_new("https://b.example").unwrap()))
        .unwrap();
    editor
        .set_selection(Selection::caret(Position::new(0, 11)))
        .unwrap();
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(
        link_nodes(node),
        [
            LinkNode {
                url: docs.to_string(),
                label: "the docs now".to_owned(),
                text: "the docs\nnow".to_owned(),
            },
            LinkNode {
                url: "https://b.example".to_owned(),
                label: "line".to_owned(),
                text: "line".to_owned(),
            },
        ]
    );
    let link = node
        .children()
        .flat_map(|paragraph| paragraph.children())
        .find(|child| child.role() == Role::Link)
        .unwrap();
    assert!(link.is_clickable(&accesskit_consumer::common_filter));
    assert_eq!(
        accesskit_consumer::common_filter(&link),
        FilterResult::Include
    );

    assert_eq!(node.document_range().text(), editor.document().plain_text());
    let lines: Vec<_> = (0..3)
        .map(|line| node.line_range_from_index(line).unwrap().text())
        .collect();
    assert_eq!(lines, ["Read the docs\n", "now, then stop\n", "next line"]);
    let caret = node.text_selection_focus().unwrap();
    assert_eq!(caret.to_global_usv_index(), 11);
    assert_eq!(caret.to_line_index(), 0);
    assert_eq!(caret.backward_to_word_start().to_global_usv_index(), 9);
    assert_eq!(caret.forward_to_word_end().to_global_usv_index(), 14);
    assert_eq!(caret.backward_to_line_start().to_global_usv_index(), 0);
    let paragraph = editor.document().paragraph(0).unwrap().clone();
    for grapheme in 0..=paragraph.grapheme_count() {
        let byte = paragraph.byte_from_grapheme(grapheme).unwrap();
        let position = node
            .text_position_from_global_usv_index(paragraph.text()[..byte].chars().count())
            .unwrap();
        if byte > 0 {
            assert_eq!(
                position.backward_to_word_start().to_global_usv_index(),
                paragraph.text()[..paragraph.previous_word(byte)]
                    .chars()
                    .count(),
                "{byte}"
            );
        }
        let raw = position.to_raw();
        adapter
            .handle_action(
                &select(
                    &adapter,
                    TextSelection {
                        anchor: raw,
                        focus: raw,
                    },
                ),
                &mut editor,
            )
            .unwrap();
        assert_eq!(editor.selection().focus, Position::new(0, byte), "{byte}");
    }
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );

    let (link_id, _) = text_input(&tree, &adapter)
        .children()
        .flat_map(|paragraph| paragraph.children())
        .find(|child| child.role() == Role::Link)
        .unwrap()
        .locate();
    assert_eq!(adapter.locate_link(link_id), Some((0, 5..19)));
    assert_eq!(adapter.locate_link(adapter.editor_node()), None);
    let click = ActionRequest {
        action: Action::Click,
        target_tree: TreeId::ROOT,
        target_node: link_id,
        data: None,
    };
    let revision = editor.document().revision();
    let selection = editor.selection();
    for read_only in [false, true] {
        adapter.set_read_only(read_only, &mut editor);
        let outcome = adapter.handle_action(&click, &mut editor).unwrap();
        assert!(outcome.handled, "read_only {read_only}");
        assert!(!outcome.changed, "read_only {read_only}");
        assert_eq!(
            outcome.activated_link,
            Some(docs.clone()),
            "read_only {read_only}"
        );
    }
    assert_eq!(editor.document().revision(), revision);
    assert_eq!(editor.selection(), selection);
    let focus = ActionRequest {
        action: Action::Focus,
        ..click.clone()
    };
    assert_eq!(
        adapter.handle_action(&focus, &mut editor).unwrap(),
        Default::default()
    );
    adapter.set_read_only(false, &mut editor);

    editor
        .set_selection(Selection::caret(Position::new(0, 7)))
        .unwrap();
    editor.set_link(None).unwrap();
    assert!(matches!(
        adapter.handle_action(&click, &mut editor),
        Err(AccessKitError::StaleDocument)
    ));
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(link_nodes(node).len(), 1);
    assert_eq!(node.document_range().text(), editor.document().plain_text());
    assert_eq!(adapter.locate_link(link_id), None);
    let outcome = adapter.handle_action(&click, &mut editor).unwrap();
    assert_eq!(outcome.activated_link, None);
    assert!(!outcome.handled);
}

/// A child of the text input node: a paragraph, or a blockquote around
/// consecutive quote paragraphs.
#[derive(Debug, PartialEq)]
enum Block {
    /// The index of a paragraph outside any quote.
    Paragraph(usize),
    /// The indices of the paragraphs inside one blockquote.
    Quote(Vec<usize>),
}

/// Returns the blocks published under the text input node, in tree order.
fn blocks(tree: &Tree, adapter: &AccessKitAdapter, editor: &Editor) -> Vec<Block> {
    let paragraphs = editor.document().paragraphs().len();
    let index = |id: NodeId| {
        (0..paragraphs)
            .position(|paragraph| adapter.paragraph_node(paragraph) == Some(id))
            .expect("a published paragraph")
    };
    let state = tree.state();
    text_input(tree, adapter)
        .data()
        .children()
        .iter()
        .map(|&id| {
            let node = state
                .node_by_tree_local_id(id, TreeId::ROOT)
                .expect("every child of the text input is in the tree");
            if node.role() != Role::Blockquote {
                return Block::Paragraph(index(id));
            }
            let children = node.data().children();
            assert!(
                !children.is_empty(),
                "a blockquote should contain at least one paragraph"
            );
            let members = children
                .iter()
                .map(|&child| {
                    let paragraph = state
                        .node_by_tree_local_id(child, TreeId::ROOT)
                        .expect("every child of a blockquote is in the tree");
                    assert_eq!(
                        paragraph.role(),
                        Role::Paragraph,
                        "blockquotes should contain only paragraphs"
                    );
                    assert_eq!(
                        paragraph.parent_id(),
                        Some(node.id()),
                        "a quoted paragraph's parent should be its blockquote"
                    );
                    index(child)
                })
                .collect();
            Block::Quote(members)
        })
        .collect()
}

/// Returns the blocks that the paragraph kinds of `editor`'s document call for.
fn expected_blocks(editor: &Editor) -> Vec<Block> {
    let mut blocks = Vec::new();
    for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
        if paragraph.kind() != ParagraphKind::Quote {
            blocks.push(Block::Paragraph(index));
        } else if let Some(Block::Quote(members)) = blocks.last_mut() {
            members.push(index);
        } else {
            blocks.push(Block::Quote(vec![index]));
        }
    }
    blocks
}

/// Returns the ID of the blockquote that holds paragraph `index`.
fn container(tree: &Tree, adapter: &AccessKitAdapter, index: usize) -> NodeId {
    let paragraph = tree
        .state()
        .node_by_tree_local_id(
            adapter
                .paragraph_node(index)
                .expect("callers pass the index of a published paragraph"),
            TreeId::ROOT,
        )
        .expect("published paragraphs are in the tree");
    let parent = paragraph
        .parent()
        .expect("every paragraph is inside the text input or a blockquote");
    assert_eq!(
        parent.role(),
        Role::Blockquote,
        "paragraph {index} should be quoted"
    );
    *text_input(tree, adapter)
        .data()
        .children()
        .iter()
        .find(|&&id| {
            tree.state()
                .node_by_tree_local_id(id, TreeId::ROOT)
                .expect("every child of the text input is in the tree")
                .id()
                == parent.id()
        })
        .expect("a blockquote is a child of the text input")
}

#[test]
fn consecutive_quotes_share_a_blockquote_that_text_navigation_passes_through() {
    let mut editor = Editor::from_text("intro\none two\nthree\nbody\nlast");
    for (range, kind) in [
        (
            Position::new(1, 0)..Position::new(2, 1),
            ParagraphKind::Quote,
        ),
        (
            Position::new(4, 0)..Position::new(4, 0),
            ParagraphKind::Quote,
        ),
    ] {
        editor
            .set_selection(Selection::new(range.start, range.end))
            .unwrap();
        editor.set_paragraph_kind(kind).unwrap();
    }
    editor
        .set_selection(Selection::caret(Position::new(1, 4)))
        .unwrap();
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    assert_eq!(
        blocks(&tree, &adapter, &editor),
        [
            Block::Paragraph(0),
            Block::Quote(vec![1, 2]),
            Block::Paragraph(3),
            Block::Quote(vec![4])
        ]
    );
    let node = text_input(&tree, &adapter);
    assert_eq!(node.document_range().text(), editor.document().plain_text());
    let lines: Vec<_> = (0..5)
        .map(|line| node.line_range_from_index(line).unwrap().text())
        .collect();
    assert_eq!(lines, ["intro\n", "one two\n", "three\n", "body\n", "last"]);
    let caret = node.text_selection_focus().unwrap();
    assert_eq!(caret.to_global_usv_index(), 10);
    assert_eq!(caret.forward_to_word_start().to_global_usv_index(), 14);
    let intro_end = node.text_position_from_global_usv_index(5).unwrap();
    assert_eq!(intro_end.forward_to_word_start().to_global_usv_index(), 6);
    assert_eq!(
        caret.forward_to_paragraph_end().to_global_usv_index(),
        14,
        "paragraph end inside the quote"
    );
    let three = node.text_position_from_global_usv_index(15).unwrap();
    assert_eq!(
        three.backward_to_paragraph_start().to_global_usv_index(),
        14
    );
    let raw = |index| {
        node.text_position_from_global_usv_index(index)
            .unwrap()
            .to_raw()
    };
    let request = select(
        &adapter,
        TextSelection {
            anchor: raw(16),
            focus: raw(22),
        },
    );
    adapter.handle_action(&request, &mut editor).unwrap();
    assert_eq!(
        editor.selection(),
        Selection::new(Position::new(2, 2), Position::new(3, 2))
    );
    assert_eq!(editor.selected_text(), "ree\nbo");

    let first = container(&tree, &adapter, 1);
    editor
        .set_selection(Selection::caret(Position::new(1, 0)))
        .unwrap();
    editor.insert_text("x").unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(container(&tree, &adapter, 1), first);
    assert_eq!(container(&tree, &adapter, 2), first);
    let last = container(&tree, &adapter, 4);
    assert_ne!(last, first);

    editor
        .set_selection(Selection::caret(Position::new(3, 0)))
        .unwrap();
    editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        blocks(&tree, &adapter, &editor),
        [Block::Paragraph(0), Block::Quote(vec![1, 2, 3, 4])]
    );
    assert_eq!(container(&tree, &adapter, 4), first);
    assert!(
        tree.state()
            .node_by_tree_local_id(last, TreeId::ROOT)
            .is_none()
    );
    assert_eq!(
        text_input(&tree, &adapter).document_range().text(),
        editor.document().plain_text()
    );

    editor
        .set_selection(Selection::caret(Position::new(2, 0)))
        .unwrap();
    editor.delete_backward().unwrap();
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(
        blocks(&tree, &adapter, &editor),
        [
            Block::Paragraph(0),
            Block::Quote(vec![1]),
            Block::Paragraph(2),
            Block::Quote(vec![3, 4])
        ]
    );
    assert_eq!(container(&tree, &adapter, 1), first);
    assert!(editor.undo());
    tree.update_and_process_changes(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        &mut Ignore,
    );
    assert_eq!(blocks(&tree, &adapter, &editor), expected_blocks(&editor));
    assert_eq!(container(&tree, &adapter, 4), first);

    adapter.invalidate();
    let restarted = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    assert_eq!(
        blocks(&restarted, &adapter, &editor),
        expected_blocks(&editor)
    );
    assert_eq!(
        text_input(&restarted, &adapter).document_range().text(),
        editor.document().plain_text()
    );
}

#[test]
fn consumer_tracks_blockquote_containers_through_incremental_updates() {
    let mut editor = Editor::from_text("alpha\nbeta\ngamma\ndelta\nepsilon");
    let mut adapter = AccessKitAdapter::default();
    let mut tree = Tree::new(
        adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
        true,
    );
    let mut seed = 0x0dd5_eed5_ba5e_ba11_u64;
    let mut random = |limit: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % limit as u64) as usize
    };
    for step in 0..800 {
        let paragraphs = editor.document().paragraphs();
        let index = random(paragraphs.len());
        let paragraph = &paragraphs[index];
        let byte = paragraph
            .byte_from_grapheme(random(paragraph.grapheme_count() + 1))
            .unwrap();
        let caret = Position::new(index, byte);
        let _ = editor.set_selection(Selection::caret(caret));
        let _ = match random(12) {
            0..=2 => editor.set_paragraph_kind(ParagraphKind::Quote),
            3 => editor.set_paragraph_kind(ParagraphKind::Body),
            4 => editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }),
            5 => editor.insert_paragraph(),
            6 => editor.delete_backward(),
            7 => editor.insert_text("q"),
            8 => {
                editor.undo();
                Ok(())
            }
            9 => {
                editor.redo();
                Ok(())
            }
            10 => {
                let start = Position::new(index, 0);
                let end = if index + 1 < editor.document().paragraphs().len() {
                    Position::new(index + 1, 0)
                } else {
                    editor.document().end()
                };
                let _ = editor.set_selection(Selection::new(start, end));
                let fragment = editor.selected_fragment();
                editor.set_selection(Selection::caret(caret)).unwrap();
                editor.insert_fragment(&fragment)
            }
            _ => editor.delete_forward(),
        };
        if random(3) == 0 {
            continue;
        }
        if random(40) == 0 {
            adapter.invalidate();
        }
        tree.update_and_process_changes(
            adapter.update(&editor, "Notes", Focus::Focused).unwrap(),
            &mut Ignore,
        );
        assert_eq!(
            blocks(&tree, &adapter, &editor),
            expected_blocks(&editor),
            "step {step}"
        );
        let node = text_input(&tree, &adapter);
        assert_eq!(
            node.document_range().text(),
            editor.document().plain_text(),
            "step {step}"
        );
        assert_eq!(
            node.text_selection_focus().unwrap().to_global_usv_index(),
            scalar_index(&editor, editor.selection().focus),
            "step {step}"
        );
    }
}
