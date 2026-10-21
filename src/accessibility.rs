use crate::{Composition, Document, Editor, Error, Link, ParagraphKind, Position, Selection, Span};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessiblePosition {
    pub paragraph: usize,
    pub grapheme: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibleSelection {
    pub anchor: AccessiblePosition,
    pub focus: AccessiblePosition,
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug)]
pub struct AccessibleParagraph<'a> {
    pub index: usize,
    pub text: &'a str,
    pub kind: ParagraphKind,
    pub spans: &'a [Span],
}

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

    pub fn label(&self) -> &'a str {
        self.label
    }

    pub fn is_focused(&self) -> bool {
        self.focused
    }

    pub fn selection(&self) -> Selection {
        self.selection
    }

    pub fn composition(&self) -> Option<&'a Composition> {
        self.composition
    }

    pub fn revision(&self) -> u64 {
        self.revision
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
        grapheme: paragraph
            .grapheme_index(position.byte)
            .expect("editor maintains valid selection"),
    }
}

fn editor_position(document: &Document, position: AccessiblePosition) -> Result<Position, Error> {
    document
        .paragraph(position.paragraph)
        .and_then(|paragraph| paragraph.byte_from_grapheme(position.grapheme))
        .map(|byte| Position::new(position.paragraph, byte))
        .ok_or(Error::InvalidGraphemePosition {
            paragraph: position.paragraph,
            grapheme: position.grapheme,
        })
}

#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibleAction<'a> {
    Focus,
    SetSelection(Selection),
    ReplaceSelectedText(&'a str),
    ReplaceAllText(&'a str),
}

#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessibilityOutcome {
    pub handled: bool,
    pub focus_requested: bool,
    pub changed: bool,
    pub composition_canceled: bool,
    pub activated_link: Option<Link>,
}

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
                ..AccessibilityOutcome::default()
            });
        }
        AccessibleAction::SetSelection(selection) => editor.set_selection(selection)?,
        AccessibleAction::ReplaceSelectedText(text) => {
            editor.cancel_composition();
            editor.break_history_group();
            editor.insert_text(text)?;
            editor.break_history_group();
        }
        AccessibleAction::ReplaceAllText(text) => {
            editor.cancel_composition();
            editor.break_history_group();
            let style = editor.document().style_at(Position::default());
            editor.replace_range_with_style(
                Position::default()..editor.document().end(),
                text,
                style,
            )?;
            editor.break_history_group();
        }
    }
    let composing = editor.composition().is_some();
    Ok(AccessibilityOutcome {
        handled: true,
        focus_requested: false,
        changed: before
            != (
                editor.document().revision(),
                editor.selection(),
                editor.typing_style(),
                composing,
            ),
        composition_canceled: before.3 && !composing,
        activated_link: None,
    })
}

#[cfg(feature = "accesskit")]
mod accesskit_bridge {
    use super::{AccessibilityOutcome, AccessibleAction, apply_action};
    use crate::{Editor, InlineStyle, Link, Paragraph, ParagraphKind, Position, Selection};
    use accesskit::{
        Action, ActionData, ActionRequest, Node, NodeId, Role, TextPosition, TextSelection, Tree,
        TreeId, TreeUpdate,
    };
    use std::{
        collections::HashMap,
        fmt,
        iter::Peekable,
        ops::Range,
        sync::{Arc, OnceLock},
    };
    use unicode_segmentation::UnicodeSegmentation;

    const MAX_RUN_CHARACTERS: usize = u8::MAX as usize;
    const MAX_CHARACTER_BYTES: usize = u8::MAX as usize;
    const SOFT_BREAK: &str = "\u{2028}";

    #[non_exhaustive]
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum AccessKitError {
        Editor(crate::Error),
        NodeIdExhausted,
        InvalidTextPosition(TextPosition),
        InvalidActionData,
        StaleDocument,
    }

    impl fmt::Display for AccessKitError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Editor(error) => error.fmt(f),
                Self::NodeIdExhausted => f.write_str("AccessKit node ID space exhausted"),
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
                Self::Editor(error) => std::error::Error::source(error),
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
        split: bool,
        line_break: bool,
        soft_break: bool,
    }

    impl CachedRun {
        fn lengths(&self) -> &[u8] {
            let lengths = self.node.character_lengths();
            &lengths[..lengths.len() - usize::from(self.line_break)]
        }

        fn text_lengths(&self) -> impl Iterator<Item = usize> + '_ {
            let lengths = self.lengths();
            let soft_break = self.soft_break.then(|| lengths.len() - 1);
            lengths.iter().enumerate().map(move |(index, length)| {
                if soft_break == Some(index) {
                    SOFT_BREAK.len()
                } else {
                    usize::from(*length)
                }
            })
        }

        fn character_index(&self, byte: usize) -> Option<usize> {
            let offset = byte.checked_sub(self.range.start)?;
            let lengths = self.lengths();
            if lengths.len() == self.range.len() {
                return (offset <= lengths.len()).then_some(offset);
            }
            let mut end = 0;
            for (index, length) in self.text_lengths().enumerate() {
                if end >= offset {
                    return (end == offset).then_some(index);
                }
                end += length;
            }
            (end == offset).then_some(lengths.len())
        }

        fn byte(&self, index: usize) -> Option<usize> {
            let lengths = self.lengths();
            lengths.get(..index)?;
            Some(
                self.range.start
                    + if lengths.len() == self.range.len() {
                        index
                    } else {
                        self.text_lengths().take(index).sum()
                    },
            )
        }
    }

    #[derive(Debug)]
    struct CachedLink {
        id: NodeId,
        node: Node,
        link: Link,
        runs: Range<usize>,
    }

    #[derive(Debug)]
    struct CachedParagraph {
        paragraph: Arc<Paragraph>,
        id: NodeId,
        node: Node,
        runs: Vec<CachedRun>,
        links: Vec<CachedLink>,
    }

    impl CachedParagraph {
        fn line_break(&self) -> bool {
            self.runs.last().is_some_and(|run| run.line_break)
        }

        fn push_nodes(&self, nodes: &mut Vec<(NodeId, Node)>) {
            nodes.push((self.id, self.node.clone()));
            nodes.extend(self.links.iter().map(|link| (link.id, link.node.clone())));
            nodes.extend(self.runs.iter().map(|run| (run.id, run.node.clone())));
        }
    }

    #[derive(Debug)]
    struct CachedQuote {
        id: NodeId,
        node: Node,
    }

    struct Staged<'a> {
        published: &'a [Arc<CachedParagraph>],
        range: Range<usize>,
        inserted: &'a [Arc<CachedParagraph>],
    }

    impl<'a> Staged<'a> {
        fn len(&self) -> usize {
            self.published.len() - self.range.len() + self.inserted.len()
        }

        fn get(&self, index: usize) -> Option<&'a CachedParagraph> {
            let paragraph = if index < self.range.start {
                self.published.get(index)
            } else if let Some(paragraph) = self.inserted.get(index - self.range.start) {
                Some(paragraph)
            } else {
                self.published
                    .get(index - self.range.start - self.inserted.len() + self.range.end)
            };
            paragraph.map(Arc::as_ref)
        }

        fn iter(&self) -> impl Iterator<Item = &'a Arc<CachedParagraph>> + use<'a> {
            let published = self.published;
            published[..self.range.start]
                .iter()
                .chain(self.inserted)
                .chain(&published[self.range.end..])
        }
    }

    #[derive(Debug)]
    pub struct AccessKitAdapter {
        root_id: NodeId,
        editor_id: NodeId,
        tree_id: TreeId,
        next_id: Option<u64>,
        paragraphs: Vec<Arc<CachedParagraph>>,
        quotes: Vec<CachedQuote>,
        run_locations: OnceLock<HashMap<NodeId, (usize, usize)>>,
        last_editor: Option<Node>,
        last_selection: Option<Selection>,
        document_identity: Option<Arc<()>>,
        initialized: bool,
        read_only: bool,
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
                quotes: Vec::new(),
                run_locations: OnceLock::new(),
                last_editor: None,
                last_selection: None,
                document_identity: None,
                initialized: false,
                read_only: false,
            })
        }

        pub fn with_tree_id(mut self, tree_id: TreeId) -> Self {
            self.tree_id = tree_id;
            self.invalidate();
            self
        }

        pub fn invalidate(&mut self) {
            self.initialized = false;
            self.last_editor = None;
        }

        pub fn root_node(&self) -> NodeId {
            self.root_id
        }
        pub fn editor_node(&self) -> NodeId {
            self.editor_id
        }

        pub fn is_read_only(&self) -> bool {
            self.read_only
        }

        pub fn set_read_only(
            &mut self,
            read_only: bool,
            editor: &mut Editor,
        ) -> AccessibilityOutcome {
            let changed = self.read_only != read_only;
            self.read_only = read_only;
            let canceled = read_only && editor.cancel_composition();
            AccessibilityOutcome {
                changed: changed || canceled,
                composition_canceled: canceled,
                ..AccessibilityOutcome::default()
            }
        }

        pub fn paragraph_node(&self, paragraph: usize) -> Option<NodeId> {
            self.paragraphs.get(paragraph).map(|paragraph| paragraph.id)
        }

        pub fn text_runs(
            &self,
            paragraph: usize,
        ) -> impl Iterator<Item = (NodeId, Range<usize>)> + '_ {
            self.paragraphs
                .get(paragraph)
                .into_iter()
                .flat_map(|paragraph| paragraph.runs.iter().map(|run| (run.id, run.range.clone())))
        }

        pub fn locate_link(&self, node: NodeId) -> Option<(usize, Range<usize>)> {
            self.paragraphs
                .iter()
                .enumerate()
                .find_map(|(index, paragraph)| {
                    let link = paragraph.links.iter().find(|link| link.id == node)?;
                    let runs = &paragraph.runs[link.runs.clone()];
                    Some((index, runs[0].range.start..runs[runs.len() - 1].range.end))
                })
        }

        pub fn locate_text_run(&self, run: NodeId) -> Option<(usize, Range<usize>)> {
            let &(paragraph, index) = self.run_locations().get(&run)?;
            Some((
                paragraph,
                self.paragraphs[paragraph].runs[index].range.clone(),
            ))
        }

        pub fn update(
            &mut self,
            editor: &Editor,
            label: &str,
            focused: bool,
        ) -> Result<TreeUpdate, AccessKitError> {
            let document_identity = editor.document().content_identity();
            let selection = editor.selection();
            let focus = if focused {
                self.editor_id
            } else {
                self.root_id
            };
            let same_document = self
                .document_identity
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, &document_identity));
            if self.initialized && same_document {
                let mut nodes = Vec::new();
                if self.last_selection != Some(selection)
                    || self.last_editor.as_ref().and_then(Node::label) != Some(label)
                    || self
                        .last_editor
                        .as_ref()
                        .is_some_and(|node| node.is_read_only() != self.read_only)
                {
                    let mut node = self.last_editor.clone().expect("initialized editor node");
                    node.set_label(label);
                    set_editable_actions(&mut node, self.read_only);
                    node.set_text_selection(TextSelection {
                        anchor: to_text_position(&self.paragraphs, selection.anchor)?,
                        focus: to_text_position(&self.paragraphs, selection.focus)?,
                    });
                    nodes.push((self.editor_id, node.clone()));
                    self.last_editor = Some(node);
                    self.last_selection = Some(selection);
                }
                return Ok(TreeUpdate {
                    nodes,
                    tree: None,
                    tree_id: self.tree_id,
                    focus,
                });
            }
            let source = editor.document().paragraphs();
            let initialized = self.initialized;
            let mut next_id = self.next_id;
            let mut nodes = Vec::new();
            let published = self.paragraphs.len();
            let change = self
                .document_identity
                .as_ref()
                .and_then(|identity| editor.document().change_since(identity))
                .filter(|change| {
                    change.range.end <= published
                        && published - change.range.len() + change.new_len == source.len()
                });
            let in_place = initialized
                && change.as_ref().is_some_and(|change| {
                    change.range.len() == change.new_len
                        && self.paragraphs[change.range.clone()]
                            .iter()
                            .zip(&source[change.range.clone()])
                            .all(|(cached, paragraph)| cached.paragraph.kind() == paragraph.kind())
                });
            let mut build = |index: usize| {
                let paragraph = Arc::new(build_paragraph(
                    Arc::clone(&source[index]),
                    index + 1 < source.len(),
                    in_place.then(|| self.paragraphs[index].id),
                    &mut next_id,
                )?);
                if initialized {
                    paragraph.push_nodes(&mut nodes);
                }
                Ok::<_, AccessKitError>(paragraph)
            };
            let (range, inserted) = if let Some(change) = change {
                let mut start = change.range.start;
                if start > 0 && (start < published) != (start < source.len()) {
                    start -= 1;
                }
                let inserted = (start..change.range.start + change.new_len)
                    .map(&mut build)
                    .collect::<Result<Vec<_>, _>>()?;
                (start..change.range.end, inserted)
            } else {
                let reusable = |cached: &CachedParagraph, index: usize| {
                    Arc::ptr_eq(&cached.paragraph, &source[index])
                        && cached.line_break() == (index + 1 < source.len())
                };
                let prefix = self
                    .paragraphs
                    .iter()
                    .zip(0..source.len())
                    .take_while(|(cached, index)| reusable(cached, *index))
                    .count();
                let suffix = self.paragraphs[prefix..]
                    .iter()
                    .rev()
                    .zip((prefix..source.len()).rev())
                    .take_while(|(cached, index)| reusable(cached, *index))
                    .count();
                let mut cached: HashMap<(usize, bool), Vec<&Arc<CachedParagraph>>> = HashMap::new();
                for paragraph in self.paragraphs[prefix..published - suffix].iter().rev() {
                    cached
                        .entry((
                            Arc::as_ptr(&paragraph.paragraph) as usize,
                            paragraph.line_break(),
                        ))
                        .or_default()
                        .push(paragraph);
                }
                let inserted = (prefix..source.len() - suffix)
                    .map(|index| {
                        let key = (
                            Arc::as_ptr(&source[index]) as usize,
                            index + 1 < source.len(),
                        );
                        match cached.get_mut(&key).and_then(Vec::pop) {
                            Some(existing) => Ok(Arc::clone(existing)),
                            None => build(index),
                        }
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                (prefix..published - suffix, inserted)
            };
            let staged = Staged {
                published: &self.paragraphs,
                range: range.clone(),
                inserted: &inserted,
            };
            if !initialized {
                for paragraph in staged.iter() {
                    paragraph.push_nodes(&mut nodes);
                }
            }
            let quotes = if in_place {
                None
            } else {
                Some(self.stage_quotes(&staged, &mut next_id)?)
            };
            for (quote, previous) in quotes.iter().flatten() {
                let previous = previous.and_then(|index| self.quotes.get(index));
                if !initialized || previous.is_none_or(|previous| previous.node != quote.node) {
                    nodes.push((quote.id, quote.node.clone()));
                }
            }
            let mut editor_node = if in_place {
                self.last_editor.clone().expect("initialized editor node")
            } else {
                let mut node = Node::new(Role::MultilineTextInput);
                node.add_action(Action::Focus);
                node.add_action(Action::SetTextSelection);
                node
            };
            editor_node.set_label(label);
            set_editable_actions(&mut editor_node, self.read_only);
            if let Some(quotes) = &quotes {
                let mut children = Vec::with_capacity(staged.len());
                let mut containers = quotes.iter().map(|(quote, _)| quote.id);
                let mut quoting = false;
                for paragraph in staged.iter() {
                    let quote = paragraph.paragraph.kind() == ParagraphKind::Quote;
                    if !quote {
                        children.push(paragraph.id);
                    } else if !quoting {
                        children.push(containers.next().expect("a container per quote"));
                    }
                    quoting = quote;
                }
                editor_node.set_children(children);
            }
            editor_node.set_text_selection(TextSelection {
                anchor: paragraph_text_position(
                    staged.get(selection.anchor.paragraph),
                    selection.anchor,
                )?,
                focus: paragraph_text_position(
                    staged.get(selection.focus.paragraph),
                    selection.focus,
                )?,
            });
            if self.last_editor.as_ref() != Some(&editor_node) {
                nodes.push((self.editor_id, editor_node.clone()));
            }
            let tree = (!initialized).then(|| {
                let mut root = Node::new(if self.tree_id == TreeId::ROOT {
                    Role::Window
                } else {
                    Role::GenericContainer
                });
                root.set_children(vec![self.editor_id]);
                nodes.push((self.root_id, root));
                let mut tree = Tree::new(self.root_id);
                tree.toolkit_name = Some("textloom".into());
                tree.toolkit_version = Some(env!("CARGO_PKG_VERSION").into());
                tree
            });
            self.next_id = next_id;
            if range.len() == inserted.len() {
                if let Some(locations) = self.run_locations.get_mut() {
                    for paragraph in &self.paragraphs[range.clone()] {
                        for run in &paragraph.runs {
                            locations.remove(&run.id);
                        }
                    }
                    for (offset, paragraph) in inserted.iter().enumerate() {
                        locations.extend(
                            paragraph.runs.iter().enumerate().map(|(run_index, run)| {
                                (run.id, (range.start + offset, run_index))
                            }),
                        );
                    }
                }
            } else {
                self.run_locations = OnceLock::new();
            }
            self.paragraphs.splice(range, inserted);
            if let Some(quotes) = quotes {
                self.quotes = quotes.into_iter().map(|(quote, _)| quote).collect();
            }
            self.last_editor = Some(editor_node);
            self.last_selection = Some(selection);
            self.document_identity = Some(document_identity);
            self.initialized = true;
            Ok(TreeUpdate {
                nodes,
                tree,
                tree_id: self.tree_id,
                focus,
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
            from_text_position(&self.paragraphs, self.run_locations(), position)
        }

        pub fn handle_action(
            &self,
            request: &ActionRequest,
            editor: &mut Editor,
        ) -> Result<AccessibilityOutcome, AccessKitError> {
            if request.target_tree != self.tree_id {
                return Ok(AccessibilityOutcome::default());
            }
            if request.target_node != self.editor_id {
                let link = (request.action == Action::Click)
                    .then(|| {
                        self.paragraphs
                            .iter()
                            .flat_map(|paragraph| &paragraph.links)
                            .find(|link| link.id == request.target_node)
                    })
                    .flatten();
                if link.is_some() {
                    self.validate_document(editor)?;
                }
                return Ok(AccessibilityOutcome {
                    handled: link.is_some(),
                    activated_link: link.map(|link| link.link.clone()),
                    ..AccessibilityOutcome::default()
                });
            }
            if request.action == Action::Focus {
                return Ok(apply_action(editor, AccessibleAction::Focus)?);
            }
            if self.read_only
                && matches!(
                    request.action,
                    Action::SetValue | Action::ReplaceSelectedText
                )
            {
                return Ok(AccessibilityOutcome {
                    handled: true,
                    ..AccessibilityOutcome::default()
                });
            }
            match request.action {
                Action::SetTextSelection => {
                    self.validate_document(editor)?;
                    let Some(ActionData::SetTextSelection(selection)) = &request.data else {
                        return Err(AccessKitError::InvalidActionData);
                    };
                    let locations = self.run_locations();
                    let selection = Selection::new(
                        from_text_position(&self.paragraphs, locations, selection.anchor)?,
                        from_text_position(&self.paragraphs, locations, selection.focus)?,
                    );
                    Ok(apply_action(
                        editor,
                        AccessibleAction::SetSelection(selection),
                    )?)
                }
                Action::SetValue | Action::ReplaceSelectedText => {
                    self.validate_document(editor)?;
                    let Some(ActionData::Value(value)) = &request.data else {
                        return Err(AccessKitError::InvalidActionData);
                    };
                    let action = if request.action == Action::SetValue {
                        AccessibleAction::ReplaceAllText(value)
                    } else {
                        AccessibleAction::ReplaceSelectedText(value)
                    };
                    Ok(apply_action(editor, action)?)
                }
                _ => Ok(AccessibilityOutcome::default()),
            }
        }

        fn stage_quotes(
            &self,
            staged: &Staged<'_>,
            next_id: &mut Option<u64>,
        ) -> Result<Vec<(CachedQuote, Option<usize>)>, AccessKitError> {
            let is_quote =
                |paragraph: &CachedParagraph| paragraph.paragraph.kind() == ParagraphKind::Quote;
            let mut published = HashMap::new();
            let mut containers = 0;
            let mut quoting = false;
            for paragraph in &self.paragraphs {
                let quote = is_quote(paragraph);
                if quote {
                    containers += usize::from(!quoting);
                    published.insert(paragraph.id, containers - 1);
                }
                quoting = quote;
            }
            let replaced = staged.range.start
                ..staged
                    .range
                    .end
                    .min(staged.range.start + staged.inserted.len());
            let previous = |index: usize, paragraph: &CachedParagraph| {
                let replaced = replaced
                    .contains(&index)
                    .then(|| self.paragraphs[index].as_ref());
                [Some(paragraph), replaced]
                    .into_iter()
                    .flatten()
                    .find_map(|paragraph| published.get(&paragraph.id).copied())
            };
            let mut claimed = vec![false; containers];
            let mut quotes = Vec::new();
            let mut paragraphs = staged.iter().enumerate().peekable();
            while let Some((index, paragraph)) = paragraphs.next() {
                if !is_quote(paragraph) {
                    continue;
                }
                let mut children = vec![paragraph.id];
                let mut reused = previous(index, paragraph).filter(|old| !claimed[*old]);
                while let Some((index, paragraph)) =
                    paragraphs.next_if(|(_, paragraph)| is_quote(paragraph))
                {
                    children.push(paragraph.id);
                    reused =
                        reused.or_else(|| previous(index, paragraph).filter(|old| !claimed[*old]));
                }
                let reused = reused.filter(|old| *old < self.quotes.len());
                let id = match reused {
                    Some(old) => {
                        claimed[old] = true;
                        self.quotes[old].id
                    }
                    None => allocate(next_id)?,
                };
                let mut node = Node::new(Role::Blockquote);
                node.set_children(children);
                quotes.push((CachedQuote { id, node }, reused));
            }
            Ok(quotes)
        }

        fn run_locations(&self) -> &HashMap<NodeId, (usize, usize)> {
            self.run_locations.get_or_init(|| {
                self.paragraphs
                    .iter()
                    .enumerate()
                    .flat_map(|(paragraph_index, paragraph)| {
                        paragraph
                            .runs
                            .iter()
                            .enumerate()
                            .map(move |(run_index, run)| (run.id, (paragraph_index, run_index)))
                    })
                    .collect()
            })
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

    fn set_editable_actions(node: &mut Node, read_only: bool) {
        if read_only {
            node.set_read_only();
            node.remove_action(Action::SetValue);
            node.remove_action(Action::ReplaceSelectedText);
        } else {
            node.clear_read_only();
            node.add_action(Action::SetValue);
            node.add_action(Action::ReplaceSelectedText);
        }
    }

    fn allocate(next_id: &mut Option<u64>) -> Result<NodeId, AccessKitError> {
        let id = next_id.ok_or(AccessKitError::NodeIdExhausted)?;
        *next_id = id.checked_add(1);
        Ok(NodeId(id))
    }

    fn build_paragraph(
        paragraph: Arc<Paragraph>,
        line_break: bool,
        id: Option<NodeId>,
        next_id: &mut Option<u64>,
    ) -> Result<CachedParagraph, AccessKitError> {
        let id = match id {
            Some(id) => id,
            None => allocate(next_id)?,
        };
        let mut node = Node::new(match paragraph.kind() {
            ParagraphKind::Body | ParagraphKind::Quote => Role::Paragraph,
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
            ParagraphKind::Body | ParagraphKind::Quote => {}
        }
        let pieces = split_runs(&paragraph, line_break);
        let ids = pieces
            .iter()
            .map(|_| allocate(next_id))
            .collect::<Result<Vec<_>, _>>()?;
        let mut links: Vec<CachedLink> = Vec::new();
        let mut children = Vec::with_capacity(ids.len());
        for (index, piece) in pieces.iter().enumerate() {
            let Some(link) = &piece.link else {
                children.push(ids[index]);
                continue;
            };
            match links.last_mut() {
                Some(cached) if cached.runs.end == index && cached.link == *link => {
                    cached.runs.end = index + 1;
                }
                _ => {
                    let id = allocate(next_id)?;
                    children.push(id);
                    links.push(CachedLink {
                        id,
                        node: Node::new(Role::Link),
                        link: link.clone(),
                        runs: index..index + 1,
                    });
                }
            }
        }
        for cached in &mut links {
            let range =
                pieces[cached.runs.start].range.start..pieces[cached.runs.end - 1].range.end;
            cached.node.set_url(cached.link.as_str());
            cached
                .node
                .set_label(paragraph.text()[range].replace(SOFT_BREAK, " "));
            cached.node.add_action(Action::Click);
            cached.node.set_children(ids[cached.runs.clone()].to_vec());
        }
        let text = paragraph.text();
        let mut words = paragraph.word_starts().into_iter().peekable();
        let last = pieces.len() - 1;
        let mut continues_line = false;
        let runs = pieces
            .into_iter()
            .enumerate()
            .map(|(index, piece)| {
                let line_break = line_break && index == last;
                let word_starts = run_word_starts(&mut words, &piece);
                let mut value = String::with_capacity(piece.range.len() + usize::from(line_break));
                if piece.soft_break {
                    value.push_str(&text[piece.range.start..piece.range.end - SOFT_BREAK.len()]);
                    value.push('\n');
                } else {
                    value.push_str(&text[piece.range.clone()]);
                }
                let mut lengths = piece.lengths;
                if line_break {
                    value.push('\n');
                    lengths.push(1);
                }
                let mut run_node = Node::new(Role::TextRun);
                run_node.set_value(value);
                run_node.set_character_lengths(lengths);
                if !word_starts.is_empty() {
                    run_node.set_word_starts(word_starts);
                }
                if continues_line {
                    run_node.set_previous_on_line(ids[index - 1]);
                }
                continues_line = !piece.soft_break;
                if continues_line && let Some(next) = ids.get(index + 1) {
                    run_node.set_next_on_line(*next);
                }
                apply_style(&mut run_node, piece.style);
                CachedRun {
                    id: ids[index],
                    node: run_node,
                    range: piece.range,
                    split: piece.split,
                    line_break,
                    soft_break: piece.soft_break,
                }
            })
            .collect();
        node.set_children(children);
        Ok(CachedParagraph {
            paragraph,
            id,
            node,
            runs,
            links,
        })
    }

    struct RunPiece {
        range: Range<usize>,
        style: InlineStyle,
        link: Option<Link>,
        lengths: Vec<u8>,
        split: bool,
        soft_break: bool,
    }

    impl RunPiece {
        fn new(start: usize, style: InlineStyle, link: Option<&Link>) -> Self {
            Self {
                range: start..start,
                style,
                link: link.cloned(),
                lengths: Vec::new(),
                split: false,
                soft_break: false,
            }
        }
    }

    fn split_runs(paragraph: &Paragraph, line_break: bool) -> Vec<RunPiece> {
        let text = paragraph.text();
        let spans = paragraph.spans();
        let limit = MAX_RUN_CHARACTERS - usize::from(line_break);
        let mut runs = Vec::new();
        if text.is_ascii() {
            for span in spans {
                for start in span.range.clone().step_by(limit) {
                    let end = span.range.end.min(start + limit);
                    runs.push(RunPiece {
                        lengths: vec![1; end - start],
                        range: start..end,
                        ..RunPiece::new(start, span.style, span.link.as_ref())
                    });
                }
            }
        } else {
            let mut span = 0;
            let mut run_span = None;
            for (byte, grapheme) in text.grapheme_indices(true) {
                while spans
                    .get(span)
                    .is_some_and(|current| current.range.end <= byte)
                {
                    span += 1;
                }
                let (style, link) = spans
                    .get(span)
                    .map_or((InlineStyle::default(), None), |span| {
                        (span.style, span.link.as_ref())
                    });
                let single = u8::try_from(grapheme.len()).ok();
                let count = if single.is_some() {
                    1
                } else {
                    character_lengths(grapheme).count()
                };
                if run_span != Some(span)
                    || runs.last().is_none_or(|run: &RunPiece| {
                        run.lengths.len() + count > limit || run.soft_break
                    })
                {
                    runs.push(RunPiece::new(byte, style, link));
                    run_span = Some(span);
                }
                if grapheme == SOFT_BREAK {
                    let run = runs.last_mut().expect("a run was created");
                    run.lengths.push(1);
                    run.range.end = byte + grapheme.len();
                    run.soft_break = true;
                    continue;
                }
                if let Some(length) = single {
                    let run = runs.last_mut().expect("a run was created");
                    run.lengths.push(length);
                    run.range.end = byte + grapheme.len();
                    continue;
                }
                let mut end = byte;
                for length in character_lengths(grapheme) {
                    if runs.last().is_some_and(|run| run.lengths.len() == limit) {
                        runs.push(RunPiece::new(end, style, link));
                    }
                    let run = runs.last_mut().expect("a run was created");
                    run.lengths.push(length);
                    end += usize::from(length);
                    run.range.end = end;
                    run.split = true;
                }
            }
        }
        if runs.last().is_none_or(|run| run.soft_break) {
            let style = spans
                .last()
                .map_or_else(InlineStyle::default, |span| span.style);
            runs.push(RunPiece::new(text.len(), style, None));
        }
        runs
    }

    fn character_lengths(grapheme: &str) -> impl Iterator<Item = u8> + '_ {
        let mut rest = grapheme;
        std::iter::from_fn(move || {
            let end = rest.floor_char_boundary(MAX_CHARACTER_BYTES);
            let length = u8::try_from(end).ok().filter(|length| *length > 0)?;
            rest = &rest[end..];
            Some(length)
        })
    }

    fn run_word_starts(
        words: &mut Peekable<impl Iterator<Item = usize>>,
        run: &RunPiece,
    ) -> Vec<u8> {
        let mut starts = Vec::new();
        let mut index = 0;
        let mut end = run.range.start;
        while let Some(word) = words.next_if(|word| *word < run.range.end) {
            while let Some(length) = run.lengths.get(index)
                && end + usize::from(*length) <= word
            {
                end += usize::from(*length);
                index += 1;
            }
            if let Ok(start) = u8::try_from(index)
                && starts.last() != Some(&start)
            {
                starts.push(start);
            }
        }
        starts
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
        if let Some(crate::Color([red, green, blue, alpha])) = style.background {
            node.set_background_color(accesskit::Color {
                red,
                green,
                blue,
                alpha,
            });
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
        paragraph_text_position(
            paragraphs.get(position.paragraph).map(Arc::as_ref),
            position,
        )
    }

    fn paragraph_text_position(
        paragraph: Option<&CachedParagraph>,
        position: Position,
    ) -> Result<TextPosition, AccessKitError> {
        paragraph
            .and_then(|paragraph| {
                let index = paragraph
                    .runs
                    .partition_point(|run| run.range.start <= position.byte)
                    .checked_sub(1)?;
                let run = &paragraph.runs[index];
                Some(TextPosition {
                    node: run.id,
                    character_index: run.character_index(position.byte)?,
                })
            })
            .ok_or(AccessKitError::Editor(crate::Error::InvalidPosition(
                position,
            )))
    }

    fn from_text_position(
        paragraphs: &[Arc<CachedParagraph>],
        run_locations: &HashMap<NodeId, (usize, usize)>,
        position: TextPosition,
    ) -> Result<Position, AccessKitError> {
        if let Some(&(index, run_index)) = run_locations.get(&position.node) {
            let paragraph = &paragraphs[index];
            let run = &paragraph.runs[run_index];
            if let Some(byte) = run.byte(position.character_index) {
                let byte = if run.split {
                    paragraph.paragraph.boundary_at_or_before(byte)
                } else {
                    byte
                };
                return Ok(Position::new(index, byte));
            }
            if run.line_break && position.character_index == run.lengths().len() + 1 {
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

        fn assert_positions_roundtrip(adapter: &AccessKitAdapter, editor: &Editor) {
            for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
                for grapheme in 0..=paragraph.grapheme_count() {
                    let position =
                        Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap());
                    let published = adapter.to_text_position(editor, position).unwrap();
                    assert_eq!(
                        adapter.from_text_position(editor, published).unwrap(),
                        position
                    );
                }
            }
        }

        fn quote_groups(adapter: &AccessKitAdapter) -> Vec<Vec<usize>> {
            adapter
                .quotes
                .iter()
                .map(|quote| {
                    quote
                        .node
                        .children()
                        .iter()
                        .map(|id| {
                            adapter
                                .paragraphs
                                .iter()
                                .position(|paragraph| paragraph.id == *id)
                                .unwrap()
                        })
                        .collect()
                })
                .collect()
        }

        fn assert_matches_fresh_tree(adapter: &AccessKitAdapter, editor: &Editor) {
            let mut fresh = AccessKitAdapter::default();
            fresh.update(editor, "Notes", true).unwrap();
            assert_eq!(quote_groups(adapter), quote_groups(&fresh));
            assert_eq!(
                adapter.last_editor.as_ref().unwrap().children().len(),
                fresh.last_editor.as_ref().unwrap().children().len()
            );
            assert_eq!(adapter.paragraphs.len(), fresh.paragraphs.len());
            for (cached, expected) in adapter.paragraphs.iter().zip(&fresh.paragraphs) {
                assert_eq!(cached.node.role(), expected.node.role());
                assert_eq!(cached.runs.len(), expected.runs.len());
                for (run, expected) in cached.runs.iter().zip(&expected.runs) {
                    assert_eq!(run.range, expected.range);
                    assert_eq!(run.line_break, expected.line_break);
                    assert_eq!(run.soft_break, expected.soft_break);
                    assert_eq!(run.node.value(), expected.node.value());
                    assert_eq!(
                        run.node.character_lengths(),
                        expected.node.character_lengths()
                    );
                    assert_eq!(run.node.word_starts(), expected.node.word_starts());
                }
            }
        }

        fn assert_runs_form_lines(adapter: &AccessKitAdapter) {
            for paragraph in &adapter.paragraphs {
                let text = paragraph.paragraph.text();
                let mut end = 0;
                for (index, run) in paragraph.runs.iter().enumerate() {
                    assert_eq!(run.range.start, end);
                    end = run.range.end;
                    assert!(run.node.character_lengths().len() <= MAX_RUN_CHARACTERS);
                    let bytes: usize = run.text_lengths().sum();
                    assert_eq!(bytes, run.range.len());
                    let value = run.node.value().unwrap();
                    let value = value
                        .strip_suffix('\n')
                        .filter(|_| run.line_break)
                        .unwrap_or(value);
                    assert_eq!(value, text[run.range.clone()].replace(SOFT_BREAK, "\n"));
                    assert_eq!(
                        run.soft_break,
                        text[run.range.clone()].ends_with(SOFT_BREAK)
                    );
                    let starts = run.node.word_starts();
                    assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
                    assert!(
                        starts
                            .iter()
                            .all(|start| usize::from(*start) < run.lengths().len())
                    );
                    let previous = index
                        .checked_sub(1)
                        .map(|previous| &paragraph.runs[previous]);
                    assert_eq!(
                        run.node.previous_on_line(),
                        previous.filter(|run| !run.soft_break).map(|run| run.id)
                    );
                    assert_eq!(
                        run.node.next_on_line(),
                        paragraph
                            .runs
                            .get(index + 1)
                            .filter(|_| !run.soft_break)
                            .map(|next| next.id)
                    );
                }
                assert_eq!(end, text.len());
                assert!(!paragraph.runs.last().unwrap().soft_break);
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
        fn set_value_replaces_the_document_and_undo_restores_selection() {
            let mut editor = Editor::from_text("aé\nhello");
            let selection = Selection::new(Position::new(1, 2), Position::new(0, 1));
            editor.set_selection(selection).unwrap();
            let mut adapter = AccessKitAdapter::default();
            let tree = adapter.update(&editor, "Notes", true).unwrap();
            let node = &tree
                .nodes
                .iter()
                .find(|(id, _)| *id == adapter.editor_node())
                .unwrap()
                .1;
            assert!(node.supports_action(Action::SetValue));
            assert_eq!(
                adapter.handle_action(&request(&adapter, Action::SetValue, None), &mut editor),
                Err(AccessKitError::InvalidActionData)
            );
            let replace = request(
                &adapter,
                Action::SetValue,
                Some(ActionData::Value("new\r\ntext".into())),
            );
            let outcome = adapter.handle_action(&replace, &mut editor).unwrap();
            assert!(outcome.handled && outcome.changed);
            assert_eq!(editor.document().plain_text(), "new\ntext");
            assert_eq!(
                editor.selection(),
                Selection::caret(editor.document().end())
            );
            assert_eq!(
                adapter.handle_action(&replace, &mut editor),
                Err(AccessKitError::StaleDocument)
            );
            assert!(editor.undo());
            assert_eq!(editor.document().plain_text(), "aé\nhello");
            assert_eq!(editor.selection(), selection);
            assert!(!editor.can_undo());
        }

        #[test]
        fn read_only_updates_capabilities_without_republishing_paragraphs() {
            let mut editor = Editor::from_text("original");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let paragraph_id = adapter.paragraph_node(0);
            editor.update_composition("candidate", None).unwrap();
            let outcome = adapter.set_read_only(true, &mut editor);
            assert!(outcome.changed && outcome.composition_canceled);
            assert!(adapter.is_read_only());
            assert!(editor.composition().is_none());
            let outcome = adapter.set_read_only(true, &mut editor);
            assert!(!outcome.changed && !outcome.composition_canceled);
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(update.nodes.len(), 1);
            let (id, node) = &update.nodes[0];
            assert_eq!(*id, adapter.editor_node());
            assert!(node.is_read_only());
            assert!(node.supports_action(Action::Focus));
            assert!(node.supports_action(Action::SetTextSelection));
            assert!(!node.supports_action(Action::SetValue));
            assert!(!node.supports_action(Action::ReplaceSelectedText));
            assert_eq!(adapter.paragraph_node(0), paragraph_id);

            let selection = TextSelection {
                anchor: adapter
                    .to_text_position(&editor, Position::new(0, 8))
                    .unwrap(),
                focus: adapter
                    .to_text_position(&editor, Position::new(0, 0))
                    .unwrap(),
            };
            let select = request(
                &adapter,
                Action::SetTextSelection,
                Some(ActionData::SetTextSelection(selection)),
            );
            assert!(adapter.handle_action(&select, &mut editor).unwrap().changed);
            let selection = editor.selection();
            for action in [Action::SetValue, Action::ReplaceSelectedText] {
                let replace = request(&adapter, action, Some(ActionData::Value("new".into())));
                let outcome = adapter.handle_action(&replace, &mut editor).unwrap();
                assert!(outcome.handled && !outcome.changed);
            }
            assert_eq!(editor.document().plain_text(), "original");
            assert_eq!(editor.selection(), selection);
            assert!(!editor.can_undo());
            assert!(adapter.set_read_only(false, &mut editor).changed);
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(update.nodes.len(), 1);
            let node = &update.nodes[0].1;
            assert!(!node.is_read_only());
            assert!(node.supports_action(Action::SetValue));
            assert!(node.supports_action(Action::ReplaceSelectedText));
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
            let runs: Vec<_> = adapter.text_runs(0).collect();
            assert_eq!(runs.len(), 3);
            assert_runs_form_lines(&adapter);
            assert!(
                tree.nodes
                    .iter()
                    .any(|(_, node)| node.font_weight() == Some(700.0))
            );
            assert_eq!(
                adapter
                    .to_text_position(&editor, Position::new(0, 1))
                    .unwrap(),
                TextPosition {
                    node: runs[1].0,
                    character_index: 0
                }
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
        fn filled_runs_publish_their_background_and_split_where_the_fill_changes() {
            let mut editor = Editor::from_text("no fill yellow red");
            for (range, fill) in [(8..14, crate::Color::YELLOW), (15..18, crate::Color::RED)] {
                editor
                    .set_selection(Selection::new(
                        Position::new(0, range.start),
                        Position::new(0, range.end),
                    ))
                    .unwrap();
                editor
                    .apply_style(crate::StylePatch {
                        background: Some(Some(fill)),
                        ..crate::StylePatch::default()
                    })
                    .unwrap();
            }
            let mut adapter = AccessKitAdapter::default();
            let tree = adapter.update(&editor, "Notes", true).unwrap();
            let runs: Vec<_> = adapter.text_runs(0).collect();
            assert_eq!(
                runs.iter()
                    .map(|(_, range)| range.clone())
                    .collect::<Vec<_>>(),
                [0..8, 8..14, 14..15, 15..18]
            );
            let opaque = |red, green, blue| accesskit::Color {
                red,
                green,
                blue,
                alpha: 255,
            };
            let backgrounds: Vec<_> = runs
                .iter()
                .map(|(id, _)| {
                    tree.nodes
                        .iter()
                        .find(|(node, _)| node == id)
                        .unwrap()
                        .1
                        .background_color()
                })
                .collect();
            assert_eq!(
                backgrounds,
                [
                    None,
                    Some(opaque(255, 255, 0)),
                    None,
                    Some(opaque(255, 0, 0))
                ]
            );
        }

        #[test]
        fn consecutive_and_skipped_edits_publish_current_runs_and_keep_untouched_ids() {
            let mut editor = Editor::from_text("first\nsecond\nthird\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let first = adapter.paragraph_node(0).unwrap();
            let last = adapter.paragraph_node(3).unwrap();
            for text in ["é", "👩‍💻"] {
                editor
                    .set_selection(Selection::caret(Position::new(1, 0)))
                    .unwrap();
                editor.insert_text(text).unwrap();
                adapter.update(&editor, "Notes", true).unwrap();
                assert_eq!(adapter.paragraph_node(0), Some(first));
                assert_eq!(adapter.paragraph_node(3), Some(last));
            }
            let obsolete = adapter
                .to_text_position(&editor, Position::new(1, 0))
                .unwrap();
            for paragraph in [1, 2] {
                editor
                    .set_selection(Selection::caret(Position::new(paragraph, 0)))
                    .unwrap();
                editor.insert_text("changed ").unwrap();
            }
            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(adapter.paragraph_node(0), Some(first));
            assert_eq!(adapter.paragraph_node(3), Some(last));
            assert_eq!(
                adapter.from_text_position(&editor, obsolete),
                Err(AccessKitError::InvalidTextPosition(obsolete))
            );
            for (index, paragraph) in editor.document().paragraphs().iter().enumerate() {
                for grapheme in 0..=paragraph.grapheme_count() {
                    let position =
                        Position::new(index, paragraph.byte_from_grapheme(grapheme).unwrap());
                    let published = adapter.to_text_position(&editor, position).unwrap();
                    assert_eq!(
                        adapter.from_text_position(&editor, published).unwrap(),
                        position
                    );
                }
            }
        }

        #[test]
        fn local_rich_paste_keeps_untouched_duplicate_occurrences_and_run_locations() {
            let mut editor = Editor::from_text("copy\n\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let storage = adapter.paragraphs.as_ptr();
            let before = adapter.paragraphs.clone();
            let removed = adapter
                .to_text_position(&editor, Position::new(1, 0))
                .unwrap();
            let fragment = editor
                .document()
                .fragment(Position::new(0, 0)..Position::new(0, 4))
                .unwrap();
            editor
                .set_selection(Selection::caret(Position::new(1, 0)))
                .unwrap();
            editor.insert_fragment(&fragment).unwrap();
            assert!(Arc::ptr_eq(
                &editor.document().paragraphs()[0],
                &editor.document().paragraphs()[1]
            ));

            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(update.nodes.len(), 3);
            assert_eq!(adapter.paragraphs.as_ptr(), storage);
            for index in [0, 2] {
                assert!(Arc::ptr_eq(&before[index], &adapter.paragraphs[index]));
            }
            assert_ne!(adapter.paragraph_node(0), adapter.paragraph_node(1));
            assert_eq!(
                adapter.from_text_position(&editor, removed),
                Err(AccessKitError::InvalidTextPosition(removed))
            );
            for index in 0..3 {
                let position = Position::new(index, 2);
                let published = adapter.to_text_position(&editor, position).unwrap();
                assert_eq!(
                    adapter.from_text_position(&editor, published).unwrap(),
                    position
                );
                let run = &adapter.paragraphs[index].runs[0];
                assert_eq!(run.line_break, index < 2);
                if run.line_break {
                    assert_eq!(
                        adapter
                            .from_text_position(
                                &editor,
                                TextPosition {
                                    node: run.id,
                                    character_index: run.lengths().len() + 1,
                                }
                            )
                            .unwrap(),
                        Position::new(index + 1, 0)
                    );
                }
            }
        }

        #[test]
        fn failed_updates_keep_the_published_tree_and_id_allocator() {
            let mut editor = Editor::from_text("first\nmiddle\nlast");
            let published_editor = Editor::new(editor.document().clone());
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let before = adapter.paragraphs.clone();
            let storage = adapter.paragraphs.as_ptr();
            let position = Position::new(1, 2);
            let published = adapter.to_text_position(&editor, position).unwrap();
            assert_eq!(
                adapter.from_text_position(&editor, published).unwrap(),
                position
            );
            let locations = adapter.run_locations.get().cloned();
            assert!(locations.is_some());
            let next_id = adapter.next_id;
            editor
                .set_selection(Selection::caret(Position::new(1, 3)))
                .unwrap();
            for structural in [false, true] {
                if structural {
                    editor.insert_paragraph().unwrap();
                } else {
                    editor.insert_text(&"supported".repeat(40)).unwrap();
                }
                adapter.next_id = Some(u64::MAX);
                assert_eq!(
                    adapter.update(&editor, "Failed", false),
                    Err(AccessKitError::NodeIdExhausted)
                );
                assert_eq!(adapter.next_id, Some(u64::MAX));
                assert_eq!(adapter.paragraphs.as_ptr(), storage);
                for (previous, current) in before.iter().zip(&adapter.paragraphs) {
                    assert!(Arc::ptr_eq(previous, current));
                }
                assert_eq!(adapter.run_locations.get(), locations.as_ref());
                assert_eq!(adapter.last_editor.as_ref().unwrap().label(), Some("Notes"));
                assert_eq!(
                    adapter
                        .from_text_position(&published_editor, published)
                        .unwrap(),
                    position
                );
                assert_eq!(
                    adapter.from_text_position(&editor, published),
                    Err(AccessKitError::StaleDocument)
                );
                assert!(editor.undo());
            }
            adapter.next_id = next_id;
            assert!(
                adapter
                    .update(&published_editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );
        }

        #[test]
        fn same_count_reordering_keeps_ids_and_moves_run_locations() {
            let editor = Editor::from_text("first\nsecond\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let first = adapter
                .to_text_position(&editor, Position::new(0, 2))
                .unwrap();
            let second = adapter
                .to_text_position(&editor, Position::new(1, 2))
                .unwrap();
            let before = adapter.paragraphs.clone();
            let source = editor.document().paragraphs();
            let fragment = crate::Fragment::from_paragraphs(vec![
                Arc::clone(&source[1]),
                Arc::clone(&source[0]),
                Arc::clone(&source[2]),
            ]);
            let reordered = Editor::new(crate::Document::from_fragment(&fragment));
            let update = adapter.update(&reordered, "Notes", true).unwrap();
            assert_eq!(update.nodes.len(), 1);
            assert!(Arc::ptr_eq(&before[0], &adapter.paragraphs[1]));
            assert!(Arc::ptr_eq(&before[1], &adapter.paragraphs[0]));
            assert!(Arc::ptr_eq(&before[2], &adapter.paragraphs[2]));
            assert_eq!(
                adapter.from_text_position(&reordered, first).unwrap(),
                Position::new(1, 2)
            );
            assert_eq!(
                adapter.from_text_position(&reordered, second).unwrap(),
                Position::new(0, 2)
            );
        }

        #[test]
        fn reinitializing_an_unchanged_tree_republishes_cached_nodes() {
            let editor = Editor::from_text("first\nlast");
            let mut adapter = AccessKitAdapter::default();
            let initial = adapter.update(&editor, "Notes", true).unwrap();
            let storage = adapter.paragraphs.as_ptr();
            let next_id = adapter.next_id;
            adapter = adapter.with_tree_id(TreeId::ROOT);
            let rebuilt = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(rebuilt.nodes, initial.nodes);
            assert!(rebuilt.tree.is_some());
            assert_eq!(adapter.next_id, next_id);
            assert_eq!(adapter.paragraphs.as_ptr(), storage);
            assert!(
                adapter
                    .update(&editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );

            adapter.invalidate();
            let reactivated = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(reactivated, initial);
            assert_eq!(adapter.next_id, next_id);
            assert!(
                adapter
                    .update(&editor, "Notes", true)
                    .unwrap()
                    .nodes
                    .is_empty()
            );

            let subtree = TreeId(accesskit::Uuid::from_u128(1));
            let mut grafted = AccessKitAdapter::default().with_tree_id(subtree);
            let update = grafted.update(&editor, "Notes", true).unwrap();
            assert_eq!(update.tree_id, subtree);
            let (_, root) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == grafted.root_node())
                .unwrap();
            assert_eq!(root.role(), Role::GenericContainer);
            assert_eq!(root.children(), &[grafted.editor_node()]);
        }

        #[test]
        fn text_positions_track_structural_edits_and_reject_old_or_unknown_runs() {
            let mut editor = Editor::from_text("first\naé👩‍💻\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let position = Position::new(1, 3);
            let published = adapter.to_text_position(&editor, position).unwrap();

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
        fn exhausted_node_ids_return_errors_without_panics() {
            assert!(matches!(
                AccessKitAdapter::new(NodeId(u64::MAX)),
                Err(AccessKitError::NodeIdExhausted)
            ));
            let mut exhausted = AccessKitAdapter::new(NodeId(u64::MAX - 1)).unwrap();
            for _ in 0..2 {
                assert_eq!(
                    exhausted.update(&Editor::from_text("a"), "Notes", true),
                    Err(AccessKitError::NodeIdExhausted)
                );
                assert!(exhausted.paragraphs.is_empty() && !exhausted.initialized);
            }
        }

        #[test]
        fn oversized_graphemes_publish_as_several_characters() {
            let mut editor = Editor::from_text("first\nmiddle\nlast");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            editor
                .set_selection(Selection::caret(Position::new(1, 3)))
                .unwrap();
            editor
                .insert_text(&format!("x{}", "\u{301}".repeat(200)))
                .unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            let (run, _) = adapter.text_runs(1).next().unwrap();
            let (_, node) = update.nodes.iter().find(|(id, _)| *id == run).unwrap();
            assert_eq!(node.character_lengths(), &[1, 1, 1, 255, 146, 1, 1, 1, 1]);
            assert_runs_form_lines(&adapter);
            assert_positions_roundtrip(&adapter, &editor);
            let character = |character_index| TextPosition {
                node: run,
                character_index,
            };
            assert_eq!(
                adapter.from_text_position(&editor, character(4)).unwrap(),
                Position::new(1, 3)
            );
            assert_eq!(
                adapter.from_text_position(&editor, character(5)).unwrap(),
                Position::new(1, 404)
            );
            editor
                .set_selection(Selection::caret(Position::new(0, 2)))
                .unwrap();
            assert_eq!(
                adapter.update(&editor, "Notes", true).unwrap().nodes.len(),
                1
            );

            let editor = Editor::from_text(&format!("a{}b", "\u{301}".repeat(40_000)));
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_runs_form_lines(&adapter);
            assert_positions_roundtrip(&adapter, &editor);
            let runs: Vec<_> = adapter.text_runs(0).collect();
            assert_eq!(runs.len(), 2);
            let inside = TextPosition {
                node: runs[1].0,
                character_index: 0,
            };
            assert!(runs[1].1.start > 0);
            assert_eq!(
                adapter.from_text_position(&editor, inside).unwrap(),
                Position::new(0, 0)
            );
        }

        #[test]
        fn soft_line_breaks_end_published_lines_and_roundtrip_positions() {
            let mut editor = Editor::from_text("ab\u{2028}cd\u{2028}\nx\u{2028}\u{2028}é");
            editor
                .set_selection(Selection::new(Position::new(1, 1), Position::new(1, 4)))
                .unwrap();
            editor
                .apply_style(crate::StylePatch {
                    bold: Some(true),
                    ..crate::StylePatch::default()
                })
                .unwrap();
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_runs_form_lines(&adapter);
            assert_positions_roundtrip(&adapter, &editor);
            let values = |adapter: &AccessKitAdapter, paragraph: usize| {
                adapter.paragraphs[paragraph]
                    .runs
                    .iter()
                    .map(|run| run.node.value().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            assert_eq!(values(&adapter, 0), ["ab\n", "cd\n", "\n"]);
            let runs: Vec<_> = adapter.text_runs(0).collect();
            let ranges: Vec<_> = runs.iter().map(|(_, range)| range.clone()).collect();
            assert_eq!(ranges, [0..5, 5..10, 10..10]);
            assert_eq!(values(&adapter, 1), ["x", "\n", "\n", "é"]);
            assert_eq!(
                adapter.paragraphs[1].runs[0].node.next_on_line(),
                Some(adapter.paragraphs[1].runs[1].id)
            );

            let encode = |byte| {
                let position = adapter
                    .to_text_position(&editor, Position::new(0, byte))
                    .unwrap();
                (position.node, position.character_index)
            };
            assert_eq!(encode(2), (runs[0].0, 2));
            assert_eq!(encode(5), (runs[1].0, 0));
            assert_eq!(encode(10), (runs[2].0, 0));
            let decode = |node, character_index| {
                adapter.from_text_position(
                    &editor,
                    TextPosition {
                        node,
                        character_index,
                    },
                )
            };
            assert_eq!(decode(runs[0].0, 3), Ok(Position::new(0, 5)));
            assert_eq!(decode(runs[2].0, 1), Ok(Position::new(1, 0)));
            assert!(decode(runs[0].0, 4).is_err());

            for edit in 0..4 {
                match edit {
                    0 => {
                        editor
                            .set_selection(Selection::caret(Position::new(0, 7)))
                            .unwrap();
                        editor.insert_line_break().unwrap();
                    }
                    1 => editor.insert_text("e").unwrap(),
                    2 => editor.delete_backward().unwrap(),
                    _ => editor.delete_backward().unwrap(),
                }
                adapter.update(&editor, "Notes", true).unwrap();
                assert_matches_fresh_tree(&adapter, &editor);
                assert_runs_form_lines(&adapter);
                assert_positions_roundtrip(&adapter, &editor);
            }
            assert_eq!(values(&adapter, 0), ["ab\n", "cd\n", "\n"]);
        }

        #[test]
        fn long_paragraphs_split_into_linked_runs_with_word_starts() {
            let ascii = "ab ".repeat(200);
            let unicode = "é ".repeat(300);
            let editor = Editor::from_text(&format!("{ascii}\n{unicode}"));
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_runs_form_lines(&adapter);
            assert_positions_roundtrip(&adapter, &editor);
            let ranges: Vec<_> = adapter.text_runs(0).map(|(_, range)| range).collect();
            assert_eq!(ranges, [0..254, 254..508, 508..600]);
            let counts: Vec<_> = adapter.paragraphs[1]
                .runs
                .iter()
                .map(|run| run.lengths().len())
                .collect();
            assert_eq!(counts, [255, 255, 90]);
            let starts = adapter.paragraphs[0].runs[1].node.word_starts();
            assert_eq!(starts.first(), Some(&1));
            assert_eq!(starts.last(), Some(&253));
            assert_eq!(starts.len(), 85);
        }

        #[test]
        fn quote_containers_publish_only_when_their_paragraphs_change() {
            let roles = |update: &TreeUpdate| -> Vec<Role> {
                update.nodes.iter().map(|(_, node)| node.role()).collect()
            };
            let mut editor = Editor::from_text("a\nb\nc\nd");
            editor
                .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
                .unwrap();
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            let mut adapter = AccessKitAdapter::default();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            let quote = adapter.quotes[0].id;
            let editor_node = update
                .nodes
                .iter()
                .find(|(id, _)| *id == adapter.editor_node())
                .unwrap();
            assert_eq!(
                editor_node.1.children(),
                [
                    adapter.paragraph_node(0).unwrap(),
                    quote,
                    adapter.paragraph_node(3).unwrap()
                ]
            );
            let (_, container) = update.nodes.iter().find(|(id, _)| *id == quote).unwrap();
            assert_eq!(container.role(), Role::Blockquote);
            assert_eq!(quote_groups(&adapter), [vec![1, 2]]);
            assert_eq!(adapter.paragraphs[1].node.role(), Role::Paragraph);

            editor
                .set_selection(Selection::caret(Position::new(3, 1)))
                .unwrap();
            editor.insert_text("x").unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(
                roles(&update),
                [Role::Paragraph, Role::TextRun, Role::MultilineTextInput]
            );
            editor
                .set_selection(Selection::caret(Position::new(2, 1)))
                .unwrap();
            editor.insert_text("x").unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(
                roles(&update),
                [Role::Paragraph, Role::TextRun, Role::MultilineTextInput]
            );
            assert_eq!(adapter.quotes[0].id, quote);
            assert_matches_fresh_tree(&adapter, &editor);
            editor.select_all();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(roles(&update), [Role::MultilineTextInput]);

            editor
                .set_selection(Selection::caret(Position::new(3, 0)))
                .unwrap();
            editor.insert_paragraph().unwrap();
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(quote_groups(&adapter), [vec![1, 2], vec![4]]);
            assert_eq!(adapter.quotes[0].id, quote);
            assert_ne!(adapter.quotes[1].id, quote);
            editor
                .set_selection(Selection::caret(Position::new(3, 0)))
                .unwrap();
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(quote_groups(&adapter), [vec![1, 2, 3, 4]]);
            assert_eq!(adapter.quotes[0].id, quote);
            assert_matches_fresh_tree(&adapter, &editor);
            assert_positions_roundtrip(&adapter, &editor);
            while editor.undo() {
                adapter.update(&editor, "Notes", true).unwrap();
                assert_matches_fresh_tree(&adapter, &editor);
            }
            assert!(adapter.quotes.is_empty());
        }

        #[test]
        fn in_place_edits_keep_paragraph_ids_and_the_editor_child_list() {
            let roles = |update: &TreeUpdate| -> Vec<Role> {
                update.nodes.iter().map(|(_, node)| node.role()).collect()
            };
            let mut editor = Editor::from_text("zero\none\ntwo\nthree");
            editor
                .set_selection(Selection::new(Position::new(1, 0), Position::new(2, 1)))
                .unwrap();
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_positions_roundtrip(&adapter, &editor);
            let ids: Vec<_> = (0..4)
                .map(|index| adapter.paragraph_node(index).unwrap())
                .collect();
            let children = adapter.last_editor.as_ref().unwrap().children().to_vec();
            let quote = adapter.quotes[0].id;
            let obsolete = adapter
                .to_text_position(&editor, Position::new(2, 1))
                .unwrap();

            editor
                .set_selection(Selection::new(Position::new(1, 1), Position::new(2, 1)))
                .unwrap();
            editor.insert_text("x\ny").unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(
                roles(&update),
                [
                    Role::Paragraph,
                    Role::TextRun,
                    Role::Paragraph,
                    Role::TextRun,
                    Role::MultilineTextInput
                ]
            );
            assert_eq!(update.nodes[0].0, ids[1]);
            assert_eq!(update.nodes[2].0, ids[2]);
            assert_eq!(
                adapter.from_text_position(&editor, obsolete),
                Err(AccessKitError::InvalidTextPosition(obsolete))
            );

            editor
                .set_selection(Selection::caret(Position::new(0, 0)))
                .unwrap();
            editor.insert_text("x").unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(
                roles(&update),
                [Role::Paragraph, Role::TextRun, Role::MultilineTextInput]
            );
            assert_eq!(update.nodes[0].0, ids[0]);
            let (_, editor_node) = update.nodes.last().unwrap();
            assert_eq!(editor_node.children(), children);
            assert_eq!(
                (0..4)
                    .map(|index| adapter.paragraph_node(index).unwrap())
                    .collect::<Vec<_>>(),
                ids
            );
            assert_eq!(quote_groups(&adapter), [vec![1, 2]]);
            assert_eq!(adapter.quotes[0].id, quote);
            assert_matches_fresh_tree(&adapter, &editor);
            assert_positions_roundtrip(&adapter, &editor);
            assert_runs_form_lines(&adapter);

            editor
                .set_selection(Selection::caret(Position::new(3, 0)))
                .unwrap();
            editor.set_paragraph_kind(ParagraphKind::Quote).unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_ne!(adapter.paragraph_node(3), Some(ids[3]));
            assert_eq!(quote_groups(&adapter), [vec![1, 2, 3]]);
            assert_eq!(adapter.quotes[0].id, quote);
            assert_matches_fresh_tree(&adapter, &editor);
            assert_positions_roundtrip(&adapter, &editor);
            while editor.undo() {
                adapter.update(&editor, "Notes", true).unwrap();
                assert_matches_fresh_tree(&adapter, &editor);
                assert_positions_roundtrip(&adapter, &editor);
            }
            assert_eq!(adapter.paragraph_node(0), Some(ids[0]));
        }

        #[test]
        fn structural_edits_rebuild_only_inserted_paragraphs() {
            let mut editor = Editor::from_text("zero\none\ntwo\nthree");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let ids: Vec<_> = (0..4)
                .map(|index| adapter.paragraph_node(index).unwrap())
                .collect();
            assert!(adapter.run_locations.get().is_none());
            assert_positions_roundtrip(&adapter, &editor);
            editor
                .set_selection(Selection::caret(Position::new(2, 1)))
                .unwrap();
            editor.insert_text("w").unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert!(adapter.run_locations.get().is_some());
            assert_positions_roundtrip(&adapter, &editor);

            editor
                .set_selection(Selection::caret(Position::new(1, 1)))
                .unwrap();
            editor.insert_paragraph().unwrap();
            let update = adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(update.nodes.len(), 5);
            assert_eq!(adapter.paragraph_node(0), Some(ids[0]));
            assert_eq!(adapter.paragraph_node(4), Some(ids[3]));
            assert!(adapter.run_locations.get().is_none());
            assert_matches_fresh_tree(&adapter, &editor);
            assert_positions_roundtrip(&adapter, &editor);

            editor
                .set_selection(Selection::caret(editor.document().end()))
                .unwrap();
            editor.insert_paragraph().unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_ne!(adapter.paragraph_node(4), Some(ids[3]));
            assert_matches_fresh_tree(&adapter, &editor);
            editor.delete_backward().unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_matches_fresh_tree(&adapter, &editor);

            while editor.undo() {
                adapter.update(&editor, "Notes", true).unwrap();
                assert_matches_fresh_tree(&adapter, &editor);
                assert_positions_roundtrip(&adapter, &editor);
            }
            assert_eq!(adapter.paragraph_node(0), Some(ids[0]));
            while editor.redo() {
                adapter.update(&editor, "Notes", true).unwrap();
                assert_matches_fresh_tree(&adapter, &editor);
                assert_positions_roundtrip(&adapter, &editor);
            }
            assert_runs_form_lines(&adapter);
        }

        #[test]
        fn several_edits_between_updates_match_fresh_trees() {
            fn random_position(
                editor: &Editor,
                random: &mut impl FnMut(usize) -> usize,
            ) -> Position {
                let paragraphs = editor.document().paragraphs();
                let index = random(paragraphs.len());
                let paragraph = &paragraphs[index];
                let byte = paragraph
                    .byte_from_grapheme(random(paragraph.grapheme_count() + 1))
                    .unwrap();
                Position::new(index, byte)
            }
            let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
            let mut random = move |limit: usize| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                (seed % limit as u64) as usize
            };
            let kinds = [
                ParagraphKind::Quote,
                ParagraphKind::Body,
                ParagraphKind::Bullet { indent: 0 },
            ];
            for _ in 0..40 {
                let mut editor = Editor::from_text("zero\none é\ntwo\nthree");
                let mut adapter = AccessKitAdapter::default();
                adapter.update(&editor, "Notes", true).unwrap();
                for _ in 0..60 {
                    for _ in 0..2 + random(3) {
                        let caret = random_position(&editor, &mut random);
                        let _ = editor.set_selection(Selection::caret(caret));
                        let _ = match random(12) {
                            0 => editor.insert_text(["x", "é", "ab cd", "x\ny"][random(4)]),
                            1 => editor.insert_paragraph(),
                            2 => editor.insert_line_break(),
                            3 => editor.delete_backward(),
                            4 => editor.delete_forward(),
                            5 => editor.set_paragraph_kind(kinds[random(3)]),
                            6 => {
                                editor.undo();
                                Ok(())
                            }
                            7 => {
                                editor.redo();
                                Ok(())
                            }
                            8 => editor.transaction(|editor| {
                                editor.insert_paragraph()?;
                                editor.insert_text("t")?;
                                editor.set_paragraph_kind(ParagraphKind::Quote)
                            }),
                            9 => {
                                let _ =
                                    editor.set_selection(Selection::caret(editor.document().end()));
                                editor.insert_paragraph()
                            }
                            10 => {
                                let paragraph = random(editor.document().paragraphs().len());
                                let end = editor.document().paragraphs()[paragraph].text().len();
                                let fragment = editor
                                    .document()
                                    .fragment(
                                        Position::new(paragraph, 0)..Position::new(paragraph, end),
                                    )
                                    .unwrap();
                                editor.insert_fragment(&fragment)
                            }
                            _ => {
                                let focus = random_position(&editor, &mut random);
                                editor.break_history_group();
                                let _ = editor.set_selection(Selection::new(caret, focus));
                                editor.delete_backward()
                            }
                        };
                    }
                    adapter.update(&editor, "Notes", true).unwrap();
                    assert_matches_fresh_tree(&adapter, &editor);
                    assert_positions_roundtrip(&adapter, &editor);
                    let mut ids = std::collections::HashSet::from([
                        adapter.root_node(),
                        adapter.editor_node(),
                    ]);
                    assert!(adapter.quotes.iter().all(|quote| ids.insert(quote.id)));
                    for (index, paragraph) in adapter.paragraphs.iter().enumerate() {
                        assert!(ids.insert(paragraph.id));
                        for run in &paragraph.runs {
                            assert!(ids.insert(run.id));
                            assert_eq!(
                                adapter.locate_text_run(run.id),
                                Some((index, run.range.clone()))
                            );
                        }
                    }
                }
            }
        }

        #[test]
        fn text_runs_can_be_located_for_host_scrolling() {
            let mut editor = Editor::from_text("first\nsecond");
            let mut adapter = AccessKitAdapter::default();
            adapter.update(&editor, "Notes", true).unwrap();
            let (run, range) = adapter.text_runs(1).next().unwrap();
            assert_eq!(adapter.locate_text_run(run), Some((1, range)));
            editor.insert_paragraph().unwrap();
            adapter.update(&editor, "Notes", true).unwrap();
            assert_eq!(adapter.locate_text_run(run), Some((2, 0..6)));
            assert_eq!(adapter.locate_text_run(adapter.editor_node()), None);
        }

        #[test]
        fn editor_errors_appear_once_in_error_chains() {
            let error = AccessKitError::from(crate::Error::InvalidRange);
            assert_eq!(error.to_string(), crate::Error::InvalidRange.to_string());
            assert!(std::error::Error::source(&error).is_none());
        }

        #[test]
        fn adapters_can_move_between_threads() {
            fn assert_send_sync<T: Send + Sync>() {}
            assert_send_sync::<AccessKitAdapter>();
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
    fn invalid_grapheme_positions_report_grapheme_coordinates() {
        let editor = Editor::from_text("h\u{e9}llo");
        let snapshot = AccessibilitySnapshot::new(&editor, "Notes", true);
        let caret = |paragraph, grapheme| {
            let position = AccessiblePosition {
                paragraph,
                grapheme,
            };
            AccessibleSelection {
                anchor: position,
                focus: position,
            }
        };
        assert_eq!(
            snapshot.selection_from_graphemes(caret(0, 6)),
            Err(Error::InvalidGraphemePosition {
                paragraph: 0,
                grapheme: 6
            })
        );
        assert_eq!(
            snapshot.selection_from_graphemes(caret(1, 0)),
            Err(Error::InvalidGraphemePosition {
                paragraph: 1,
                grapheme: 0
            })
        );
        assert_eq!(
            snapshot.selection_from_graphemes(caret(0, 5)),
            Ok(Selection::caret(Position::new(0, 6)))
        );
    }

    #[test]
    fn outcomes_report_canceled_composition() {
        let mut editor = Editor::from_text("Hello");
        editor.update_composition("にほん", None).unwrap();
        let outcome = apply_action(
            &mut editor,
            AccessibleAction::SetSelection(Selection::caret(Position::new(0, 1))),
        )
        .unwrap();
        assert!(outcome.changed && outcome.composition_canceled);
        let outcome = apply_action(
            &mut editor,
            AccessibleAction::SetSelection(Selection::caret(Position::new(0, 2))),
        )
        .unwrap();
        assert!(outcome.changed && !outcome.composition_canceled);

        editor.update_composition("か", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::Focus).unwrap();
        assert!(outcome.focus_requested && !outcome.composition_canceled);
        assert!(editor.composition().is_some());
        let outcome =
            apply_action(&mut editor, AccessibleAction::ReplaceSelectedText("x")).unwrap();
        assert!(outcome.composition_canceled);
        assert_eq!(editor.document().plain_text(), "Hexllo");
    }

    #[test]
    fn replacing_all_text_uses_the_document_start_style() {
        let mut editor = Editor::from_text("Title\nbody bold");
        editor
            .set_paragraph_kind(ParagraphKind::Heading { level: 1 })
            .unwrap();
        editor
            .set_selection(Selection::new(Position::new(1, 5), Position::new(1, 9)))
            .unwrap();
        editor
            .apply_style(crate::StylePatch {
                bold: Some(true),
                ..crate::StylePatch::default()
            })
            .unwrap();
        let selection = Selection::caret(Position::new(1, 9));
        editor.set_selection(selection).unwrap();
        let typing_style = editor.typing_style();
        assert!(typing_style.bold);
        apply_action(
            &mut editor,
            AccessibleAction::ReplaceAllText("brand new\ncontent"),
        )
        .unwrap();
        let paragraphs = editor.document().paragraphs();
        assert_eq!(paragraphs[0].kind(), ParagraphKind::Heading { level: 1 });
        assert_eq!(paragraphs[1].kind(), ParagraphKind::Body);
        assert!(
            paragraphs
                .iter()
                .flat_map(|paragraph| paragraph.spans())
                .all(|span| !span.style.bold)
        );
        assert!(!editor.typing_style().bold);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "Title\nbody bold");
        assert_eq!(editor.selection(), selection);
        assert_eq!(editor.typing_style(), typing_style);
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

    #[test]
    fn replacing_all_text_preserves_selection_and_typing_undo_boundaries() {
        let mut editor = Editor::from_text("original");
        editor.insert_text("typed ").unwrap();
        let selection = Selection::new(Position::new(0, 7), Position::new(0, 2));
        editor.set_selection(selection).unwrap();
        editor.update_composition("candidate", None).unwrap();
        let outcome = apply_action(&mut editor, AccessibleAction::ReplaceAllText("new")).unwrap();
        assert!(outcome.handled && outcome.changed);
        assert!(editor.composition().is_none());
        assert_eq!(editor.document().plain_text(), "new");
        editor.insert_text(" typing").unwrap();
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "new");
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "typed original");
        assert_eq!(editor.selection(), selection);
        assert!(editor.undo());
        assert_eq!(editor.document().plain_text(), "original");
    }
}
