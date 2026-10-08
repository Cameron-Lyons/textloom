//! Borrowed semantics for custom GUI adapters, with an optional AccessKit tree.
//!
//! The renderer supplies screen bounds and caret geometry to its platform
//! accessibility integration. This module exposes document semantics and maps
//! editing actions without taking ownership of the window or event loop.

use crate::{Composition, Document, Editor, Error, ParagraphKind, Position, Selection, Span};

/// A paragraph-local caret stop expressed in extended graphemes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessiblePosition {
    /// Zero-based paragraph index.
    pub paragraph: usize,
    /// Extended grapheme index, independent of UTF-8 byte length.
    pub grapheme: usize,
}

/// A directional selection using grapheme indices rather than UTF-8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibleSelection {
    /// The fixed endpoint when extending selection.
    pub anchor: AccessiblePosition,
    /// The active caret endpoint.
    pub focus: AccessiblePosition,
}

/// Borrowed text, paragraph semantics, and inline styles for one paragraph.
#[derive(Clone, Copy, Debug)]
pub struct AccessibleParagraph<'a> {
    /// Zero-based index in the document.
    pub index: usize,
    /// Committed text, excluding the paragraph separator.
    pub text: &'a str,
    /// Body, heading, or list semantics.
    pub kind: ParagraphKind,
    /// Normalized inline styles with paragraph-local UTF-8 byte ranges.
    pub spans: &'a [Span],
}

/// A zero-copy semantic view. It remains coherent by borrowing the editor.
///
/// Snapshot state is immutable, so selection conversion always uses validated
/// editor coordinates:
///
/// ```compile_fail
/// use textloom::{Editor, Position, Selection, accessibility::AccessibilitySnapshot};
/// let editor = Editor::from_text("text");
/// let mut snapshot = AccessibilitySnapshot::new(&editor, "Notes", true);
/// snapshot.selection = Selection::caret(Position::new(99, 99));
/// ```
#[derive(Debug)]
pub struct AccessibilitySnapshot<'a> {
    label: &'a str,
    focused: bool,
    selection: Selection,
    composition: Option<&'a Composition>,
    revision: u64,
    document: &'a Document,
}

impl<'a> AccessibilitySnapshot<'a> {
    /// Borrow a coherent editor view with a host-supplied name and focus state.
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

    /// Return the human-readable name supplied by the host.
    pub fn label(&self) -> &'a str {
        self.label
    }

    /// Whether the host considered the editor focused when captured.
    pub fn is_focused(&self) -> bool {
        self.focused
    }

    /// Return the captured directional selection in validated byte coordinates.
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// Return transient preedit text, separate from committed paragraphs.
    pub fn composition(&self) -> Option<&'a Composition> {
        self.composition
    }

    /// Return the revision of the borrowed committed document.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Iterate committed paragraphs in document order without copying text.
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

    /// Convert the current directional selection to paragraph-local graphemes.
    pub fn selection_graphemes(&self) -> AccessibleSelection {
        AccessibleSelection {
            anchor: accessible_position(self.document, self.selection.anchor),
            focus: accessible_position(self.document, self.selection.focus),
        }
    }

    /// Convert grapheme endpoints to validated document byte coordinates.
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
        grapheme: paragraph
            .grapheme_index(position.byte)
            .expect("editor maintains valid selection"),
    }
}

fn editor_position(document: &Document, position: AccessiblePosition) -> Result<Position, Error> {
    let paragraph = document
        .paragraph(position.paragraph)
        .ok_or(Error::InvalidPosition(Position::new(
            position.paragraph,
            position.grapheme,
        )))?;
    let byte = paragraph
        .byte_from_grapheme(position.grapheme)
        .ok_or(Error::InvalidPosition(Position::new(
            position.paragraph,
            position.grapheme,
        )))?;
    Ok(Position::new(position.paragraph, byte))
}

/// Renderer-independent actions offered to assistive technology.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibleAction<'a> {
    /// Ask the host to focus the editor and its window.
    Focus,
    /// Move or extend selection and cancel any active composition.
    SetSelection(Selection),
    /// Cancel preedit and replace the committed selection as one undo step.
    ReplaceSelectedText(&'a str),
}

/// Result of an accessibility action for the host to apply and redraw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityOutcome {
    /// Whether the action was recognized and applied.
    pub handled: bool,
    /// The host should focus the editor widget and, if needed, its window.
    pub focus_requested: bool,
    /// Whether document, selection, typing style, or composition changed.
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
        editor.composition().is_some(),
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
            editor.cancel_composition();
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
                editor.composition().is_some(),
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

    /// Failure to construct a tree or apply a validated AccessKit action.
    #[non_exhaustive]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AccessKitError {
        /// The editing core rejected the requested operation.
        Editor(crate::Error),
        /// The adapter has exhausted its reserved 64-bit node ID sequence.
        NodeIdExhausted,
        /// AccessKit stores each selectable character's UTF-8 length in a u8.
        /// Splitting a grapheme would incorrectly expose extra caret stops.
        CharacterTooLong {
            /// Start of the unsupported grapheme in document coordinates.
            position: Position,
            /// UTF-8 byte length of the grapheme.
            bytes: usize,
        },
        /// An action refers to an unknown text run or invalid character index.
        InvalidTextPosition(TextPosition),
        /// The requested action lacks its required data or uses the wrong type.
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
        run_locations: HashMap<NodeId, (usize, usize)>,
        last_editor: Option<Node>,
        last_selection: Option<Selection>,
        document_identity: Option<Arc<()>>,
        initialized: bool,
    }

    impl Default for AccessKitAdapter {
        fn default() -> Self {
            Self::new(NodeId(1)).expect("constant root ID has room for descendants")
        }
    }

    impl AccessKitAdapter {
        /// Reserve IDs beginning at `root_id` for a window, editor, and descendants.
        ///
        /// Returns [`AccessKitError::NodeIdExhausted`] if no editor ID is available.
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
                run_locations: HashMap::new(),
                last_editor: None,
                last_selection: None,
                document_identity: None,
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

        /// Return the reserved window root ID.
        pub fn root_node(&self) -> NodeId {
            self.root_id
        }
        /// Return the reserved multiline editor ID.
        pub fn editor_node(&self) -> NodeId {
            self.editor_id
        }

        /// Return a paragraph ID from the most recently published update.
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
            let document_identity = editor.document().content_identity();
            let selection = editor.selection();
            let same_document = self
                .document_identity
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, &document_identity));
            if self.initialized && same_document {
                let mut nodes = Vec::new();
                if self.last_selection != Some(selection)
                    || self.last_editor.as_ref().and_then(Node::label) != Some(label)
                {
                    // Selection and labels reuse all paragraph nodes and run
                    // locations. Only the editor node needs to be republished.
                    let mut node = self.last_editor.clone().expect("initialized editor node");
                    node.set_label(label);
                    node.set_text_selection(TextSelection {
                        anchor: to_text_position(&self.paragraphs, selection.anchor)?,
                        focus: to_text_position(&self.paragraphs, selection.focus)?,
                    });
                    nodes.push((self.editor_id, node.clone()));
                    self.last_editor = Some(node);
                    self.last_selection = Some(selection);
                }
                // Focus is carried independently of the nodes. Idle frames
                // and focus changes require no node clones.
                return Ok(TreeUpdate {
                    nodes,
                    tree: None,
                    tree_id: self.tree_id,
                    focus: if focused {
                        self.editor_id
                    } else {
                        self.root_id
                    },
                });
            }
            let mut next_id = self.next_id;
            let mut cached: HashMap<(usize, bool), Vec<&Arc<CachedParagraph>>> = HashMap::new();
            // Rich paste can repeat the same shared paragraph allocation. Each
            // occurrence needs its own IDs, and each cached occurrence is used
            // at most once. Reverse insertion preserves document order on pop.
            for paragraph in self.paragraphs.iter().rev() {
                cached
                    .entry((
                        Arc::as_ptr(&paragraph.paragraph) as usize,
                        paragraph.line_break,
                    ))
                    .or_default()
                    .push(paragraph);
            }
            let mut paragraphs = Vec::with_capacity(editor.document().paragraphs().len());
            let mut nodes = Vec::new();
            for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
                let line_break = index + 1 < editor.document().paragraphs().len();
                let key = (Arc::as_ptr(paragraph) as usize, line_break);
                let existing = cached.get_mut(&key).and_then(Vec::pop);
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
            self.run_locations = paragraphs
                .iter()
                .enumerate()
                .flat_map(|(paragraph_index, paragraph)| {
                    paragraph
                        .runs
                        .iter()
                        .enumerate()
                        .map(move |(run_index, run)| (run.id, (paragraph_index, run_index)))
                })
                .collect();
            self.paragraphs = paragraphs;
            self.last_editor = Some(editor_node);
            self.last_selection = Some(selection);
            self.document_identity = Some(document_identity);
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

        /// Map a valid byte position into the current published text runs.
        ///
        /// Publish an update after document edits before calling this method.
        pub fn to_text_position(
            &self,
            editor: &Editor,
            position: Position,
        ) -> Result<TextPosition, AccessKitError> {
            self.validate_document(editor)?;
            editor.document().validate_position(position)?;
            to_text_position(&self.paragraphs, position)
        }

        /// Decode a caret stop from the current published text runs.
        ///
        /// Synthetic paragraph separators map to the next paragraph's start.
        pub fn from_text_position(
            &self,
            editor: &Editor,
            position: TextPosition,
        ) -> Result<Position, AccessKitError> {
            self.validate_document(editor)?;
            from_text_position(&self.paragraphs, &self.run_locations, position)
        }

        /// Apply actions targeted at this editor; other targets remain unhandled.
        ///
        /// The host owns focus changes and must suppress edits for read-only
        /// editors. Text actions require the latest committed document update.
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
                        from_text_position(
                            &self.paragraphs,
                            &self.run_locations,
                            selection.anchor,
                        )?,
                        from_text_position(&self.paragraphs, &self.run_locations, selection.focus)?,
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
            if !self
                .document_identity
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, &editor.document().content_identity()))
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
            // Prefer the preceding run at a shared style boundary, preserving
            // the adapter's published selection representation.
            let index = paragraph
                .runs
                .partition_point(|run| run.range.end < position.byte);
            if let Some(run) = paragraph.runs.get(index)
                && let Ok(character_index) = run.character_offsets.binary_search(&position.byte)
            {
                return Ok(TextPosition {
                    node: run.id,
                    character_index,
                });
            }
        }
        Err(AccessKitError::Editor(crate::Error::InvalidPosition(
            position,
        )))
    }

    fn from_text_position(
        paragraphs: &[Arc<CachedParagraph>],
        run_locations: &HashMap<NodeId, (usize, usize)>,
        position: TextPosition,
    ) -> Result<Position, AccessKitError> {
        if let Some(&(index, run_index)) = run_locations.get(&position.node) {
            let run = &paragraphs[index].runs[run_index];
            if let Some(byte) = run.character_offsets.get(position.character_index) {
                return Ok(Position::new(index, *byte));
            }
            if run.line_break && position.character_index == run.character_offsets.len() {
                return Ok(Position::new(index + 1, 0));
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
        fn idle_updates_keep_focus_and_refresh_labels_and_replacement_documents() {
            let editor = Editor::from_text("original");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();

            let blurred = adapter.update(&editor, "Notes", false).unwrap();
            assert!(blurred.nodes.is_empty());
            assert_eq!(blurred.focus, adapter.root_node());
            let focused = adapter.update(&editor, "Notes", true).unwrap();
            assert!(focused.nodes.is_empty());
            assert_eq!(focused.focus, adapter.editor_node());

            let renamed = adapter.update(&editor, "Renamed", true).unwrap();
            assert_eq!(renamed.nodes.len(), 1);
            assert_eq!(renamed.nodes[0].1.label(), Some("Renamed"));
            assert!(
                adapter
                    .update(&editor, "Renamed", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );

            // Separate documents can share a revision and selection without
            // sharing text. An idle update must not reuse the old text tree.
            let replacement = Editor::from_text("replacement");
            assert_eq!(
                replacement.document().revision(),
                editor.document().revision()
            );
            assert_eq!(replacement.selection(), editor.selection());
            let update = adapter.update(&replacement, "Renamed", true).unwrap();
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.value() == Some("replacement"))
            );
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
        fn text_positions_track_structural_edits_and_reject_old_or_unknown_runs() {
            let mut editor = Editor::from_text("first\naé👩‍💻\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let position = Position::new(1, 3);
            let published = adapter.to_text_position(&editor, position).unwrap();

            // A cloned content state is valid, but a separate same-revision
            // document must not decode positions from the previous tree.
            let cloned = Editor::new(editor.document().clone());
            assert_eq!(
                adapter.from_text_position(&cloned, published).unwrap(),
                position
            );
            let unrelated = Editor::from_text("other\ntext\nhere");
            assert_eq!(
                adapter.from_text_position(&unrelated, published),
                Err(AccessKitError::StaleDocument)
            );

            editor.insert_text("new\n").unwrap();
            assert_eq!(
                adapter.from_text_position(&editor, published),
                Err(AccessKitError::StaleDocument)
            );
            adapter.update(&editor, "Notes", true).unwrap();
            // The untouched paragraph retains its node, but its document index
            // changes when a preceding paragraph is inserted.
            assert_eq!(
                adapter.from_text_position(&editor, published).unwrap(),
                Position::new(2, 3)
            );

            let invalid = TextPosition {
                node: NodeId(u64::MAX),
                character_index: 0,
            };
            assert_eq!(
                adapter.from_text_position(&editor, invalid),
                Err(AccessKitError::InvalidTextPosition(invalid))
            );
            let invalid = TextPosition {
                character_index: usize::MAX,
                ..published
            };
            assert_eq!(
                adapter.from_text_position(&editor, invalid),
                Err(AccessKitError::InvalidTextPosition(invalid))
            );

            let unsupported = Editor::from_text(&format!("x{}", "\u{301}".repeat(200)));
            assert!(matches!(
                adapter.update(&unsupported, "Notes", true),
                Err(AccessKitError::CharacterTooLong { .. })
            ));
            // Failed updates keep the previously published tree usable.
            assert_eq!(
                adapter.from_text_position(&editor, published).unwrap(),
                Position::new(2, 3)
            );
            assert!(
                adapter
                    .update(&editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );
        }

        #[test]
        fn rich_paste_of_shared_paragraphs_keeps_each_accessible_occurrence_distinct() {
            let mut editor = Editor::from_text("copy\n\n");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let original_id = adapter.paragraph_node(0).unwrap();
            let fragment = editor
                .document()
                .fragment(Position::new(0, 0)..Position::new(1, 0))
                .unwrap();
            editor
                .set_selection(Selection::caret(Position::new(1, 0)))
                .unwrap();
            editor.insert_fragment(&fragment).unwrap();
            assert!(Arc::ptr_eq(
                &editor.document().paragraphs()[0],
                &editor.document().paragraphs()[1]
            ));

            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(adapter.paragraph_node(0), Some(original_id));
            let paragraph_ids: Vec<_> = (0..editor.document().paragraphs().len())
                .map(|index| adapter.paragraph_node(index).unwrap())
                .collect();
            let unique: std::collections::HashSet<_> = paragraph_ids.iter().collect();
            assert_eq!(unique.len(), paragraph_ids.len());
            for index in 0..2 {
                let position = Position::new(index, 2);
                assert_eq!(
                    adapter
                        .from_text_position(
                            &editor,
                            adapter.to_text_position(&editor, position).unwrap()
                        )
                        .unwrap(),
                    position
                );
            }
            assert!(
                adapter
                    .update(&editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );
            assert_eq!(
                paragraph_ids,
                (0..editor.document().paragraphs().len())
                    .map(|index| adapter.paragraph_node(index).unwrap())
                    .collect::<Vec<_>>()
            );
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
        assert_eq!(snapshot.label(), "Notes");
        assert!(snapshot.is_focused());
        assert_eq!(snapshot.selection(), editor.selection());
        assert_eq!(snapshot.revision(), editor.document().revision());
        assert!(snapshot.composition().is_none());
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

    #[test]
    fn actions_cancel_composition_and_report_preview_only_changes() {
        let mut editor = Editor::from_text("original");
        editor.select_all();
        let selection = editor.selection();
        editor.update_composition("candidate", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::SetSelection(selection)).unwrap();
        assert!(outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "original");

        editor.update_composition("candidate", None).unwrap();
        let outcome =
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("new")).unwrap();
        assert!(outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
        assert_eq!(editor.selection(), selection);
        assert!(!editor.can_undo());
    }
}
