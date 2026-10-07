//! Borrowed semantics for custom GUI adapters, with an optional AccessKit tree.
//!
//! The renderer supplies screen bounds and caret geometry to its platform
//! accessibility integration. This module exposes document semantics and maps
//! editing actions without taking ownership of the window or event loop.

use crate::{Composition, Document, Editor, Error, ParagraphKind, Position, Selection, Span};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessiblePosition {
    pub paragraph: usize,
    /// Extended grapheme index, independent of UTF-8 byte length.
    pub grapheme: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibleSelection {
    pub anchor: AccessiblePosition,
    pub focus: AccessiblePosition,
}

#[derive(Clone, Copy, Debug)]
pub struct AccessibleParagraph<'a> {
    pub index: usize,
    pub text: &'a str,
    pub kind: ParagraphKind,
    pub spans: &'a [Span],
}

/// A zero-copy semantic view. It remains coherent by borrowing the editor.
#[derive(Debug)]
pub struct AccessibilitySnapshot<'a> {
    pub label: &'a str,
    pub focused: bool,
    pub selection: Selection,
    pub composition: Option<&'a Composition>,
    pub revision: u64,
    document: &'a Document,
}

impl<'a> AccessibilitySnapshot<'a> {
    pub fn new(editor: &'a Editor, label: &'a str, focused: bool) -> Self {
        Self {
            label,
            focused,
            selection: editor.selection(),
            composition: editor.composition(),
            revision: editor.document().revision(),
            document: editor.document(),
        }
    }

    pub fn paragraphs(
        &self,
    ) -> impl ExactSizeIterator<Item = AccessibleParagraph<'a>> + DoubleEndedIterator {
        self.document
            .paragraphs()
            .iter()
            .enumerate()
            .map(|(index, paragraph)| AccessibleParagraph {
                index,
                text: paragraph.text(),
                kind: paragraph.kind(),
                spans: paragraph.spans(),
            })
    }

    pub fn selection_graphemes(&self) -> AccessibleSelection {
        AccessibleSelection {
            anchor: accessible_position(self.document, self.selection.anchor),
            focus: accessible_position(self.document, self.selection.focus),
        }
    }

    pub fn selection_from_graphemes(
        &self,
        selection: AccessibleSelection,
    ) -> Result<Selection, Error> {
        Ok(Selection::new(
            editor_position(self.document, selection.anchor)?,
            editor_position(self.document, selection.focus)?,
        ))
    }
}

fn accessible_position(document: &Document, position: Position) -> AccessiblePosition {
    let paragraph = document
        .paragraph(position.paragraph)
        .expect("editor maintains valid selection");
    AccessiblePosition {
        paragraph: position.paragraph,
        grapheme: paragraph.text()[..position.byte].graphemes(true).count(),
    }
}

fn editor_position(document: &Document, position: AccessiblePosition) -> Result<Position, Error> {
    let paragraph = document
        .paragraph(position.paragraph)
        .ok_or(Error::InvalidPosition(Position::new(
            position.paragraph,
            position.grapheme,
        )))?;
    let mut boundaries = paragraph
        .text()
        .grapheme_indices(true)
        .map(|(byte, _)| byte)
        .chain(std::iter::once(paragraph.text().len()));
    let byte = boundaries
        .nth(position.grapheme)
        .ok_or(Error::InvalidPosition(Position::new(
            position.paragraph,
            position.grapheme,
        )))?;
    Ok(Position::new(position.paragraph, byte))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibleAction<'a> {
    Focus,
    SetSelection(Selection),
    ReplaceSelectedText(&'a str),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityOutcome {
    pub handled: bool,
    /// The host should focus the editor widget and, if needed, its window.
    pub focus_requested: bool,
    pub changed: bool,
}

/// All mutations pass through the editor's normal validation and undo history.
pub fn apply_action(
    editor: &mut Editor,
    action: AccessibleAction<'_>,
) -> Result<AccessibilityOutcome, Error> {
    let before = (
        editor.document().revision(),
        editor.selection(),
        editor.typing_style(),
    );
    match action {
        AccessibleAction::Focus => {
            return Ok(AccessibilityOutcome {
                handled: true,
                focus_requested: true,
                changed: false,
            });
        }
        AccessibleAction::SetSelection(selection) => editor.set_selection(selection)?,
        AccessibleAction::ReplaceSelectedText(text) => {
            editor.break_history_group();
            editor.insert_text(text)?;
            editor.break_history_group();
        }
    }
    Ok(AccessibilityOutcome {
        handled: true,
        focus_requested: false,
        changed: before
            != (
                editor.document().revision(),
                editor.selection(),
                editor.typing_style(),
            ),
    })
}

#[cfg(feature = "accesskit")]
mod accesskit_bridge {
    use super::{AccessibilityOutcome, AccessibleAction, apply_action};
    use crate::{Editor, InlineStyle, Paragraph, ParagraphKind, Position, Selection};
    use accesskit::{
        Action, ActionData, ActionRequest, Node, NodeId, Role, TextPosition, TextSelection, Tree,
        TreeId, TreeUpdate,
    };
    use std::{collections::HashMap, fmt, ops::Range, sync::Arc};
    use unicode_segmentation::UnicodeSegmentation;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AccessKitError {
        Editor(crate::Error),
        NodeIdExhausted,
        /// AccessKit stores each selectable character's UTF-8 length in a u8.
        /// Splitting a grapheme would incorrectly expose extra caret stops.
        CharacterTooLong {
            position: Position,
            bytes: usize,
        },
        InvalidTextPosition(TextPosition),
        InvalidActionData,
        /// Publish a new update before applying actions against changed text.
        StaleDocument,
    }

    impl fmt::Display for AccessKitError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Editor(error) => error.fmt(f),
                Self::NodeIdExhausted => f.write_str("AccessKit node ID space exhausted"),
                Self::CharacterTooLong { position, bytes } => write!(
                    f,
                    "grapheme at {}:{} is {bytes} bytes; AccessKit supports at most 255",
                    position.paragraph, position.byte
                ),
                Self::InvalidTextPosition(position) => {
                    write!(f, "invalid AccessKit text position {position:?}")
                }
                Self::InvalidActionData => {
                    f.write_str("AccessKit action has missing or incompatible data")
                }
                Self::StaleDocument => f.write_str(
                    "publish an accessibility update before handling actions against changed text",
                ),
            }
        }
    }

    impl std::error::Error for AccessKitError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                Self::Editor(error) => Some(error),
                _ => None,
            }
        }
    }
    impl From<crate::Error> for AccessKitError {
        fn from(error: crate::Error) -> Self {
            Self::Editor(error)
        }
    }

    #[derive(Debug)]
    struct CachedRun {
        id: NodeId,
        node: Node,
        range: Range<usize>,
        /// Includes the start and end boundary; excludes a synthetic newline.
        character_offsets: Vec<usize>,
        line_break: bool,
    }

    #[derive(Debug)]
    struct CachedParagraph {
        paragraph: Arc<Paragraph>,
        id: NodeId,
        node: Node,
        runs: Vec<CachedRun>,
        line_break: bool,
    }

    /// Produces an AccessKit tree with a window root, a multiline text input,
    /// semantic paragraph children, and styled grapheme-aware text runs.
    ///
    /// The host installs an AccessKit platform adapter, submits these updates,
    /// and handles `focus_requested`. Supply layout bounds and character
    /// geometry in the returned nodes for caret magnification and hit testing.
    /// Node IDs start at the supplied root and are reserved exclusively for
    /// this adapter. Unchanged paragraphs retain their IDs and cached nodes;
    /// ordinary caret changes only publish the editor node.
    #[derive(Debug)]
    pub struct AccessKitAdapter {
        root_id: NodeId,
        editor_id: NodeId,
        tree_id: TreeId,
        next_id: Option<u64>,
        paragraphs: Vec<Arc<CachedParagraph>>,
        last_editor: Option<Node>,
        revision: Option<u64>,
        initialized: bool,
    }

    impl Default for AccessKitAdapter {
        fn default() -> Self {
            Self::new(NodeId(1)).expect("constant root ID has room for descendants")
        }
    }

    impl AccessKitAdapter {
        pub fn new(root_id: NodeId) -> Result<Self, AccessKitError> {
            let editor_id = NodeId(
                root_id
                    .0
                    .checked_add(1)
                    .ok_or(AccessKitError::NodeIdExhausted)?,
            );
            Ok(Self {
                root_id,
                editor_id,
                tree_id: TreeId::ROOT,
                next_id: editor_id.0.checked_add(1),
                paragraphs: Vec::new(),
                last_editor: None,
                revision: None,
                initialized: false,
            })
        }

        /// For a host that grafts this tree into its own accessibility tree.
        pub fn with_tree_id(mut self, tree_id: TreeId) -> Self {
            self.tree_id = tree_id;
            self.initialized = false;
            self.last_editor = None;
            self
        }

        pub fn root_node(&self) -> NodeId {
            self.root_id
        }
        pub fn editor_node(&self) -> NodeId {
            self.editor_id
        }

        pub fn paragraph_node(&self, paragraph: usize) -> Option<NodeId> {
            self.paragraphs.get(paragraph).map(|paragraph| paragraph.id)
        }

        /// Current text run IDs and their paragraph-local UTF-8 ranges. A hard
        /// line break belongs to the last run but is excluded from this range.
        pub fn text_runs(
            &self,
            paragraph: usize,
        ) -> impl Iterator<Item = (NodeId, Range<usize>)> + '_ {
            self.paragraphs
                .get(paragraph)
                .into_iter()
                .flat_map(|paragraph| paragraph.runs.iter().map(|run| (run.id, run.range.clone())))
        }

        /// Build the initial tree or an incremental update. An error leaves the
        /// adapter's previous tree intact. Transient IME text is available from
        /// the neutral snapshot; committed document text defines this tree.
        pub fn update(
            &mut self,
            editor: &Editor,
            label: &str,
            focused: bool,
        ) -> Result<TreeUpdate, AccessKitError> {
            let mut next_id = self.next_id;
            let cached: HashMap<(usize, bool), &Arc<CachedParagraph>> = self
                .paragraphs
                .iter()
                .map(|paragraph| {
                    (
                        (
                            Arc::as_ptr(&paragraph.paragraph) as usize,
                            paragraph.line_break,
                        ),
                        paragraph,
                    )
                })
                .collect();
            let mut paragraphs = Vec::with_capacity(editor.document().paragraphs().len());
            let mut nodes = Vec::new();
            for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
                let line_break = index + 1 < editor.document().paragraphs().len();
                let key = (Arc::as_ptr(paragraph) as usize, line_break);
                let existing = cached.get(&key);
                let next = if let Some(existing) = existing {
                    Arc::clone(existing)
                } else {
                    Arc::new(build_paragraph(
                        Arc::clone(paragraph),
                        index,
                        line_break,
                        &mut next_id,
                    )?)
                };
                if existing.is_none() || !self.initialized {
                    nodes.push((next.id, next.node.clone()));
                    nodes.extend(next.runs.iter().map(|run| (run.id, run.node.clone())));
                }
                paragraphs.push(next);
            }
            let mut editor_node = Node::new(Role::MultilineTextInput);
            editor_node.set_label(label);
            editor_node.add_action(Action::Focus);
            editor_node.add_action(Action::SetTextSelection);
            editor_node.add_action(Action::ReplaceSelectedText);
            editor_node.set_children(
                paragraphs
                    .iter()
                    .map(|paragraph| paragraph.id)
                    .collect::<Vec<_>>(),
            );
            editor_node.set_text_selection(TextSelection {
                anchor: to_text_position(&paragraphs, editor.selection().anchor)?,
                focus: to_text_position(&paragraphs, editor.selection().focus)?,
            });
            if self.last_editor.as_ref() != Some(&editor_node) {
                nodes.push((self.editor_id, editor_node.clone()));
            }
            if !self.initialized {
                let mut root = Node::new(Role::Window);
                root.set_children(vec![self.editor_id]);
                nodes.push((self.root_id, root));
            }
            let tree = if !self.initialized {
                let mut tree = Tree::new(self.root_id);
                tree.toolkit_name = Some("textloom".into());
                tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
                Some(tree)
            } else {
                None
            };
            self.next_id = next_id;
            self.paragraphs = paragraphs;
            self.last_editor = Some(editor_node);
            self.revision = Some(editor.document().revision());
            self.initialized = true;
            Ok(TreeUpdate {
                nodes,
                tree,
                tree_id: self.tree_id,
                focus: if focused {
                    self.editor_id
                } else {
                    self.root_id
                },
            })
        }

        pub fn to_text_position(
            &self,
            editor: &Editor,
            position: Position,
        ) -> Result<TextPosition, AccessKitError> {
            self.validate_document(editor)?;
            editor.document().validate_position(position)?;
            to_text_position(&self.paragraphs, position)
        }

        pub fn from_text_position(
            &self,
            editor: &Editor,
            position: TextPosition,
        ) -> Result<Position, AccessKitError> {
            self.validate_document(editor)?;
            from_text_position(&self.paragraphs, position)
        }

        pub fn handle_action(
            &self,
            request: &ActionRequest,
            editor: &mut Editor,
        ) -> Result<AccessibilityOutcome, AccessKitError> {
            if request.target_tree != self.tree_id || request.target_node != self.editor_id {
                return Ok(AccessibilityOutcome::default());
            }
            if request.action == Action::Focus {
                return Ok(apply_action(editor, AccessibleAction::Focus)?);
            }
            match request.action {
                Action::SetTextSelection => {
                    self.validate_document(editor)?;
                    let Some(ActionData::SetTextSelection(selection)) = &request.data else {
                        return Err(AccessKitError::InvalidActionData);
                    };
                    let selection = Selection::new(
                        from_text_position(&self.paragraphs, selection.anchor)?,
                        from_text_position(&self.paragraphs, selection.focus)?,
                    );
                    Ok(apply_action(
                        editor,
                        AccessibleAction::SetSelection(selection),
                    )?)
                }
                Action::ReplaceSelectedText => {
                    self.validate_document(editor)?;
                    let Some(ActionData::Value(value)) = &request.data else {
                        return Err(AccessKitError::InvalidActionData);
                    };
                    Ok(apply_action(
                        editor,
                        AccessibleAction::ReplaceSelectedText(value),
                    )?)
                }
                _ => Ok(AccessibilityOutcome::default()),
            }
        }

        fn validate_document(&self, editor: &Editor) -> Result<(), AccessKitError> {
            if self.revision != Some(editor.document().revision())
                || self.paragraphs.len() != editor.document().paragraphs().len()
                || !self
                    .paragraphs
                    .iter()
                    .zip(editor.document().paragraphs())
                    .all(|(cached, actual)| Arc::ptr_eq(&cached.paragraph, actual))
            {
                return Err(AccessKitError::StaleDocument);
            }
            Ok(())
        }
    }

    fn allocate(next_id: &mut Option<u64>) -> Result<NodeId, AccessKitError> {
        let id = next_id.ok_or(AccessKitError::NodeIdExhausted)?;
        *next_id = id.checked_add(1);
        Ok(NodeId(id))
    }

    fn build_paragraph(
        paragraph: Arc<Paragraph>,
        index: usize,
        line_break: bool,
        next_id: &mut Option<u64>,
    ) -> Result<CachedParagraph, AccessKitError> {
        let id = allocate(next_id)?;
        let mut node = Node::new(match paragraph.kind() {
            ParagraphKind::Body => Role::Paragraph,
            ParagraphKind::Heading { .. } => Role::Heading,
            ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } => Role::ListItem,
        });
        match paragraph.kind() {
            ParagraphKind::Heading { level } => node.set_level(usize::from(level)),
            ParagraphKind::Bullet { indent } => {
                node.set_level(usize::from(indent) + 1);
                node.set_list_style(accesskit::ListStyle::Disc);
            }
            ParagraphKind::Ordered { indent, .. } => {
                node.set_level(usize::from(indent) + 1);
                node.set_list_style(accesskit::ListStyle::Numeric);
            }
            ParagraphKind::Body => {}
        }
        let ranges: Vec<_> = if paragraph.spans().is_empty() {
            vec![(0..paragraph.text().len(), InlineStyle::default())]
        } else {
            paragraph
                .spans()
                .iter()
                .map(|span| (span.range.clone(), span.style))
                .collect()
        };
        let mut runs = Vec::with_capacity(ranges.len());
        for (run_index, (range, style)) in ranges.iter().enumerate() {
            let run_id = allocate(next_id)?;
            let text = &paragraph.text()[range.clone()];
            let mut lengths = Vec::new();
            let mut offsets = vec![range.start];
            for (byte, grapheme) in text.grapheme_indices(true) {
                lengths.push(u8::try_from(grapheme.len()).map_err(|_| {
                    AccessKitError::CharacterTooLong {
                        position: Position::new(index, range.start + byte),
                        bytes: grapheme.len(),
                    }
                })?);
                offsets.push(range.start + byte + grapheme.len());
            }
            let run_break = line_break && run_index + 1 == ranges.len();
            let mut value = text.to_owned();
            if run_break {
                value.push('\n');
                lengths.push(1);
            }
            let mut run_node = Node::new(Role::TextRun);
            run_node.set_value(value);
            run_node.set_character_lengths(lengths);
            apply_style(&mut run_node, *style);
            runs.push(CachedRun {
                id: run_id,
                node: run_node,
                range: range.clone(),
                character_offsets: offsets,
                line_break: run_break,
            });
        }
        node.set_children(runs.iter().map(|run| run.id).collect::<Vec<_>>());
        Ok(CachedParagraph {
            paragraph,
            id,
            node,
            runs,
            line_break,
        })
    }

    fn apply_style(node: &mut Node, style: InlineStyle) {
        node.set_font_weight(if style.bold { 700.0 } else { 400.0 });
        if style.italic {
            node.set_italic();
        }
        if style.code {
            node.set_font_family("monospace");
        }
        let color = style
            .foreground
            .map(|crate::Color([red, green, blue, alpha])| accesskit::Color {
                red,
                green,
                blue,
                alpha,
            });
        if let Some(color) = color {
            node.set_foreground_color(color);
        }
        let decoration = accesskit::TextDecoration {
            style: accesskit::TextDecorationStyle::Solid,
            color: color.unwrap_or(accesskit::Color {
                red: 0,
                green: 0,
                blue: 0,
                alpha: 255,
            }),
        };
        if style.underline {
            node.set_underline(decoration);
        }
        if style.strikethrough {
            node.set_strikethrough(decoration);
        }
    }

    fn to_text_position(
        paragraphs: &[Arc<CachedParagraph>],
        position: Position,
    ) -> Result<TextPosition, AccessKitError> {
        if let Some(paragraph) = paragraphs.get(position.paragraph) {
            for run in &paragraph.runs {
                if let Ok(character_index) = run.character_offsets.binary_search(&position.byte) {
                    return Ok(TextPosition {
                        node: run.id,
                        character_index,
                    });
                }
            }
        }
        Err(AccessKitError::Editor(crate::Error::InvalidPosition(
            position,
        )))
    }

    fn from_text_position(
        paragraphs: &[Arc<CachedParagraph>],
        position: TextPosition,
    ) -> Result<Position, AccessKitError> {
        for (index, paragraph) in paragraphs.iter().enumerate() {
            if let Some(run) = paragraph.runs.iter().find(|run| run.id == position.node) {
                if let Some(byte) = run.character_offsets.get(position.character_index) {
                    return Ok(Position::new(index, *byte));
                }
                if run.line_break && position.character_index == run.character_offsets.len() {
                    return Ok(Position::new(index + 1, 0));
                }
                break;
            }
        }
        Err(AccessKitError::InvalidTextPosition(position))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn request(
            adapter: &AccessKitAdapter,
            action: Action,
            data: Option<ActionData>,
        ) -> ActionRequest {
            ActionRequest {
                action,
                data,
                target_tree: TreeId::ROOT,
                target_node: adapter.editor_node(),
            }
        }

        #[test]
        fn tree_uses_graphemes_line_breaks_and_incremental_updates() {
            let mut editor = Editor::from_text("a👩🏽‍💻\nlast");
            let mut adapter = AccessKitAdapter::default();
            let tree = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(tree.focus, adapter.editor_node());
            let first = adapter.text_runs(0).next().unwrap().0;
            let run = &tree.nodes.iter().find(|(id, _)| *id == first).unwrap().1;
            assert_eq!(run.value(), Some("a👩🏽‍💻\n"));
            assert_eq!(run.character_lengths(), &[1, 15, 1]);
            let end = adapter
                .to_text_position(&editor, Position::new(0, 16))
                .unwrap();
            assert_eq!(end.character_index, 2);
            assert_eq!(
                adapter.from_text_position(&editor, end).unwrap(),
                Position::new(0, 16)
            );
            assert_eq!(
                adapter
                    .from_text_position(
                        &editor,
                        TextPosition {
                            node: first,
                            character_index: 3
                        }
                    )
                    .unwrap(),
                Position::new(1, 0)
            );
            assert!(
                adapter
                    .update(&editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );
            editor
                .set_selection(Selection::caret(Position::new(1, 1)))
                .unwrap();
            let selection_only = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(selection_only.nodes.len(), 1);
            assert_eq!(selection_only.nodes[0].0, adapter.editor_node());
        }

        #[test]
        fn accesskit_actions_validate_data_and_preserve_direction() {
            let mut editor = Editor::from_text("aé\nhello");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", false).unwrap();
            let focus = request(&adapter, Action::Focus, None);
            assert!(
                adapter
                    .handle_action(&focus, &mut editor)
                    .unwrap()
                    .focus_requested
            );
            let selection = TextSelection {
                anchor: adapter
                    .to_text_position(&editor, Position::new(1, 2))
                    .unwrap(),
                focus: adapter
                    .to_text_position(&editor, Position::new(0, 1))
                    .unwrap(),
            };
            let select = request(
                &adapter,
                Action::SetTextSelection,
                Some(ActionData::SetTextSelection(selection)),
            );
            assert!(adapter.handle_action(&select, &mut editor).unwrap().changed);
            assert_eq!(
                editor.selection(),
                Selection::new(Position::new(1, 2), Position::new(0, 1))
            );
            let bad = request(&adapter, Action::SetTextSelection, None);
            assert_eq!(
                adapter.handle_action(&bad, &mut editor),
                Err(AccessKitError::InvalidActionData)
            );
            let replace = request(
                &adapter,
                Action::ReplaceSelectedText,
                Some(ActionData::Value("X".into())),
            );
            adapter.handle_action(&replace, &mut editor).unwrap();
            assert_eq!(editor.document().plain_text(), "aXllo");
            assert_eq!(
                adapter.handle_action(&replace, &mut editor),
                Err(AccessKitError::StaleDocument)
            );
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "aé\nhello");
        }

        #[test]
        fn styled_runs_roundtrip_and_keep_ids_for_unchanged_paragraphs() {
            let mut editor = Editor::from_text("abc\nxyz");
            editor
                .set_selection(Selection::new(Position::new(0, 1), Position::new(0, 2)))
                .unwrap();
            editor
                .apply_style(crate::StylePatch {
                    bold: Some(true),
                    ..crate::StylePatch::default()
                })
                .unwrap();
            let mut adapter = AccessKitAdapter::default();
            let tree = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(adapter.text_runs(0).count(), 3);
            assert!(
                tree.nodes
                    .iter()
                    .any(|(_, node)| node.font_weight() == Some(700.0))
            );
            for byte in 0..=3 {
                let position = Position::new(0, byte);
                let text_position = adapter.to_text_position(&editor, position).unwrap();
                assert_eq!(
                    adapter.from_text_position(&editor, text_position).unwrap(),
                    position
                );
            }
            let unchanged_id = adapter.paragraph_node(1).unwrap();
            editor.insert_text("q").unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(adapter.paragraph_node(1), Some(unchanged_id));
        }

        #[test]
        fn pathological_clusters_and_node_ids_return_errors_without_panics() {
            let editor = Editor::from_text(&format!("x{}", "\u{0301}".repeat(200)));
            let mut adapter = AccessKitAdapter::default();
            assert!(matches!(
                adapter.update(&editor, "Notes", true),
                Err(AccessKitError::CharacterTooLong { bytes: 401, .. })
            ));
            assert!(matches!(
                AccessKitAdapter::new(NodeId(u64::MAX)),
                Err(AccessKitError::NodeIdExhausted)
            ));
            let mut exhausted = AccessKitAdapter::new(NodeId(u64::MAX - 1)).unwrap();
            assert_eq!(
                exhausted.update(&Editor::from_text("a"), "Notes", true),
                Err(AccessKitError::NodeIdExhausted)
            );
            // An unsuccessful update does not corrupt the previous ID allocator.
            assert!(
                adapter
                    .update(&Editor::from_text("ok"), "Notes", true)
                    .is_ok()
            );
        }
    }
}
#[cfg(feature = "accesskit")]
pub use accesskit_bridge::{AccessKitAdapter, AccessKitError};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_exposes_semantics_and_grapheme_selections() {
        let mut editor = Editor::from_text("a👩🏽‍💻\nitem");
        editor
            .set_selection(Selection::new(Position::new(0, 1), Position::new(1, 4)))
            .unwrap();
        editor
            .set_paragraph_kind(ParagraphKind::Bullet { indent: 1 })
            .unwrap();
        let snapshot = AccessibilitySnapshot::new(&editor, "Notes", true);
        assert_eq!(snapshot.paragraphs().count(), 2);
        assert_eq!(
            snapshot.paragraphs().nth(1).unwrap().kind,
            ParagraphKind::Bullet { indent: 1 }
        );
        let selection = snapshot.selection_graphemes();
        assert_eq!(selection.anchor.grapheme, 1);
        assert_eq!(
            snapshot.selection_from_graphemes(selection).unwrap(),
            editor.selection()
        );
        assert!(
            snapshot
                .selection_from_graphemes(AccessibleSelection {
                    anchor: AccessiblePosition {
                        paragraph: 0,
                        grapheme: 20
                    },
                    focus: selection.focus,
                })
                .is_err()
        );
    }

    #[test]
    fn actions_request_focus_validate_selection_and_use_undo() {
        let mut editor = Editor::from_text("hello");
        assert!(
            apply_action(&mut editor, AccessibleAction::Focus)
                .unwrap()
                .focus_requested
        );
        assert!(
            apply_action(
                &mut editor,
                AccessibleAction::SetSelection(Selection::caret(Position::new(0, 99)))
            )
            .is_err()
        );
        editor.select_all();
        assert!(
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("new"))
                .unwrap()
                .changed
        );
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "hello");
    }
}
