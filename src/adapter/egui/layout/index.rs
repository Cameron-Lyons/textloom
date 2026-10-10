//! Persistent paragraph geometry and prefix totals. Leaves share unchanged
//! layouts across frames; an edit copies only boundary leaves and tree paths.

#[cfg(test)]
mod tests;

use super::{PARAGRAPH_GAP, ParagraphCache, ParagraphLayout, ParagraphLayoutView};
use std::{
    ops::Range,
    sync::{Arc, Weak},
};

const LEAF_CAPACITY: usize = 32;

#[derive(Clone)]
struct Entry {
    layout: ParagraphLayout,
    height: f64,
    chars: usize,
}

impl Entry {
    fn from_cache(cache: &ParagraphCache) -> Self {
        let rect =
            egui::Rect::from_min_size(egui::Pos2::new(cache.indent, 0.0), cache.galley.size());
        Self {
            layout: ParagraphLayout {
                galley: cache.galley.clone(),
                rect,
                marker: cache.marker.clone(),
                marker_position: egui::Pos2::new(
                    cache.indent - cache.marker.as_ref().map_or(0.0, |m| m.size().x) - 8.0,
                    0.0,
                ),
            },
            height: f64::from(cache.galley.size().y + PARAGRAPH_GAP),
            chars: cache.chars + 1,
        }
    }

    fn at(&self, y: f64) -> ParagraphLayout {
        let mut layout = self.layout.clone();
        let offset = egui::vec2(0.0, y as f32);
        layout.rect = layout.rect.translate(offset);
        layout.marker_position += offset;
        layout
    }

    fn view(&self, y: f64) -> ParagraphLayoutView<'_> {
        let offset = egui::vec2(0.0, y as f32);
        ParagraphLayoutView {
            galley: &self.layout.galley,
            rect: self.layout.rect.translate(offset),
            marker: self.layout.marker.as_ref(),
            marker_position: self.layout.marker_position + offset,
        }
    }
}

enum Children {
    Leaf(Box<[Entry]>),
    Branch(Arc<Node>, Arc<Node>),
}

struct Node {
    children: Children,
    len: usize,
    height: f64,
    chars: usize,
    lists: usize,
    depth: u16,
    tail_height: f64,
    tail_top: f32,
    tail_bottom: f32,
}

fn leaf(entries: Vec<Entry>) -> Arc<Node> {
    debug_assert!(!entries.is_empty() && entries.len() <= LEAF_CAPACITY);
    let tail = entries.last().expect("nonempty layout leaf");
    Arc::new(Node {
        len: entries.len(),
        height: entries.iter().map(|entry| entry.height).sum(),
        chars: entries.iter().map(|entry| entry.chars).sum(),
        lists: entries
            .iter()
            .filter(|entry| entry.layout.marker.is_some())
            .count(),
        depth: 1,
        tail_height: tail.height,
        tail_top: tail.layout.rect.top(),
        tail_bottom: tail.layout.rect.bottom(),
        children: Children::Leaf(entries.into_boxed_slice()),
    })
}

fn branch(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    Arc::new(Node {
        len: left.len + right.len,
        height: left.height + right.height,
        chars: left.chars + right.chars,
        lists: left.lists + right.lists,
        depth: left.depth.max(right.depth) + 1,
        tail_height: right.tail_height,
        tail_top: right.tail_top,
        tail_bottom: right.tail_bottom,
        children: Children::Branch(left, right),
    })
}

fn balanced(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if left.depth > right.depth + 1 {
        let Children::Branch(ll, lr) = &left.children else {
            unreachable!()
        };
        if ll.depth >= lr.depth {
            branch(ll.clone(), branch(lr.clone(), right))
        } else {
            let Children::Branch(lrl, lrr) = &lr.children else {
                unreachable!()
            };
            branch(branch(ll.clone(), lrl.clone()), branch(lrr.clone(), right))
        }
    } else if right.depth > left.depth + 1 {
        let Children::Branch(rl, rr) = &right.children else {
            unreachable!()
        };
        if rr.depth >= rl.depth {
            branch(branch(left, rl.clone()), rr.clone())
        } else {
            let Children::Branch(rll, rlr) = &rl.children else {
                unreachable!()
            };
            branch(branch(left, rll.clone()), branch(rlr.clone(), rr.clone()))
        }
    } else {
        branch(left, right)
    }
}

fn join(left: Arc<Node>, right: Arc<Node>) -> Arc<Node> {
    if let (Children::Leaf(a), Children::Leaf(b)) = (&left.children, &right.children)
        && a.len() + b.len() <= LEAF_CAPACITY
    {
        return leaf(a.iter().chain(b.iter()).cloned().collect());
    }
    if left.depth > right.depth + 1 {
        let Children::Branch(ll, lr) = &left.children else {
            unreachable!()
        };
        balanced(ll.clone(), join(lr.clone(), right))
    } else if right.depth > left.depth + 1 {
        let Children::Branch(rl, rr) = &right.children else {
            unreachable!()
        };
        balanced(join(left, rl.clone()), rr.clone())
    } else {
        branch(left, right)
    }
}

fn concat(left: Option<Arc<Node>>, right: Option<Arc<Node>>) -> Option<Arc<Node>> {
    match (left, right) {
        (Some(a), Some(b)) => Some(join(a, b)),
        (a, b) => a.or(b),
    }
}

fn split(node: Arc<Node>, at: usize) -> (Option<Arc<Node>>, Option<Arc<Node>>) {
    if at == 0 {
        return (None, Some(node));
    }
    if at == node.len {
        return (Some(node), None);
    }
    debug_assert!(at < node.len);
    match &node.children {
        Children::Leaf(entries) => (
            Some(leaf(entries[..at].to_vec())),
            Some(leaf(entries[at..].to_vec())),
        ),
        Children::Branch(left, right) => {
            if at < left.len {
                let (before, after) = split(left.clone(), at);
                (before, concat(after, Some(right.clone())))
            } else {
                let (before, after) = split(right.clone(), at - left.len);
                (concat(Some(left.clone()), before), after)
            }
        }
    }
}

fn build(entries: impl IntoIterator<Item = Entry>) -> Option<Arc<Node>> {
    fn from_leaves(leaves: &[Arc<Node>]) -> Arc<Node> {
        if leaves.len() == 1 {
            return leaves[0].clone();
        }
        let middle = leaves.len() / 2;
        branch(
            from_leaves(&leaves[..middle]),
            from_leaves(&leaves[middle..]),
        )
    }
    let mut entries = entries.into_iter();
    let mut leaves = Vec::new();
    loop {
        let chunk: Vec<_> = entries.by_ref().take(LEAF_CAPACITY).collect();
        if chunk.is_empty() {
            break;
        }
        leaves.push(leaf(chunk));
    }
    (!leaves.is_empty()).then(|| from_leaves(&leaves))
}

fn update(node: &Arc<Node>, at: usize, paragraphs: &[ParagraphCache]) -> Arc<Node> {
    match &node.children {
        Children::Leaf(entries) => {
            let mut entries = entries.to_vec();
            for (entry, cache) in entries[at..at + paragraphs.len()]
                .iter_mut()
                .zip(paragraphs)
            {
                *entry = Entry::from_cache(cache);
            }
            leaf(entries)
        }
        Children::Branch(left, right) => {
            let left_count = paragraphs.len().min(left.len.saturating_sub(at));
            let next_left = if left_count == 0 {
                left.clone()
            } else {
                update(left, at, &paragraphs[..left_count])
            };
            let next_right = if left_count == paragraphs.len() {
                right.clone()
            } else {
                update(
                    right,
                    at.saturating_sub(left.len),
                    &paragraphs[left_count..],
                )
            };
            branch(next_left, next_right)
        }
    }
}

/// A generation token that keeps an allocation identity without retaining its
/// geometry. A stale Weak allocation remains reserved, preventing pointer reuse
/// from making an old accessibility generation match a newer layout.
#[derive(Clone, Debug)]
pub(crate) struct LayoutIdentity(Weak<LayoutVersion>);

impl LayoutIdentity {
    pub(crate) fn matches(&self, layouts: &ParagraphLayouts) -> bool {
        layouts.version.as_ref().map_or_else(
            || self.0.ptr_eq(&Weak::new()),
            |version| std::ptr::eq(self.0.as_ptr(), Arc::as_ptr(version)),
        )
    }

    #[cfg(test)]
    pub(crate) fn ptr_eq(&self, other: &Self) -> bool {
        self.0.ptr_eq(&other.0)
    }
}

struct LayoutVersion {
    root: Option<Arc<Node>>,
    environment: Arc<()>,
}

/// An immutable geometry snapshot. Empty placeholder caches allocate nothing;
/// initialized snapshots clone one Arc and keep their complete generation.
#[derive(Clone, Default)]
pub(crate) struct ParagraphLayouts {
    version: Option<Arc<LayoutVersion>>,
}

impl ParagraphLayouts {
    fn root(&self) -> Option<&Arc<Node>> {
        self.version.as_ref()?.root.as_ref()
    }

    pub(crate) fn len(&self) -> usize {
        self.root().map_or(0, |node| node.len)
    }

    pub(crate) fn identity(&self) -> LayoutIdentity {
        LayoutIdentity(self.version.as_ref().map_or_else(Weak::new, Arc::downgrade))
    }

    pub(crate) fn environment_identity(&self) -> Arc<()> {
        self.version
            .as_ref()
            .expect("layout environment initialized by rebuild")
            .environment
            .clone()
    }

    pub(crate) fn get(&self, mut index: usize) -> Option<ParagraphLayout> {
        let mut node = self.root().map(Arc::as_ref)?;
        if index >= node.len {
            return None;
        }
        let mut y = 0.0;
        loop {
            match &node.children {
                Children::Leaf(entries) => {
                    y += entries[..index]
                        .iter()
                        .map(|entry| entry.height)
                        .sum::<f64>();
                    return Some(entries[index].at(y));
                }
                Children::Branch(left, right) => {
                    if index < left.len {
                        node = left;
                    } else {
                        index -= left.len;
                        y += left.height;
                        node = right;
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn last(&self) -> Option<ParagraphLayout> {
        self.get(self.len().checked_sub(1)?)
    }

    pub(crate) fn content_height(&self) -> Option<f32> {
        let root = self.root().map(Arc::as_ref)?;
        Some((root.height - root.tail_height) as f32 + root.tail_bottom)
    }

    #[cfg(test)]
    pub(crate) fn partition_point(
        &self,
        mut predicate: impl FnMut(&ParagraphLayout) -> bool,
    ) -> usize {
        let mut left = 0;
        let mut right = self.len();
        while left < right {
            let middle = left + (right - left) / 2;
            if predicate(&self.get(middle).expect("valid layout index")) {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        left
    }

    // Descend using subtree bounds rather than binary-searching materialized
    // rectangles. No layout/galley clones are needed to locate the viewport.
    fn partition_y(&self, y: f32, bottom: bool, inclusive: bool, extra: f32) -> usize {
        let Some(mut node) = self.root().map(Arc::as_ref) else {
            return 0;
        };
        let mut index = 0;
        let mut offset = 0.0;
        let before = |edge: f32| if inclusive { edge <= y } else { edge < y };
        loop {
            match &node.children {
                Children::Branch(left, right) => {
                    let last_y = (offset + left.height - left.tail_height) as f32;
                    let edge = last_y
                        + if bottom {
                            left.tail_bottom
                        } else {
                            left.tail_top
                        }
                        + extra;
                    if before(edge) {
                        index += left.len;
                        offset += left.height;
                        node = right;
                    } else {
                        node = left;
                    }
                }
                Children::Leaf(entries) => {
                    for entry in entries {
                        let edge = offset as f32
                            + if bottom {
                                entry.layout.rect.bottom()
                            } else {
                                entry.layout.rect.top()
                            }
                            + extra;
                        if !before(edge) {
                            return index;
                        }
                        index += 1;
                        offset += entry.height;
                    }
                    return index;
                }
            }
        }
    }

    pub(crate) fn first_bottom_at_least(&self, y: f32) -> usize {
        self.partition_y(y, true, false, 0.0)
    }

    pub(crate) fn first_top_after(&self, y: f32) -> usize {
        self.partition_y(y, false, true, 0.0)
    }

    pub(crate) fn paragraph_at_y(&self, y: f32) -> usize {
        self.partition_y(y, true, false, PARAGRAPH_GAP / 2.0)
    }

    fn prefix(&self, mut count: usize, lists: bool) -> usize {
        let Some(mut node) = self.root().map(Arc::as_ref) else {
            return 0;
        };
        let mut total = 0;
        count = count.min(node.len);
        loop {
            match &node.children {
                Children::Leaf(entries) => {
                    return total
                        + entries[..count]
                            .iter()
                            .map(|e| {
                                if lists {
                                    usize::from(e.layout.marker.is_some())
                                } else {
                                    e.chars
                                }
                            })
                            .sum::<usize>();
                }
                Children::Branch(left, right) => {
                    if count <= left.len {
                        node = left;
                    } else {
                        count -= left.len;
                        total += if lists { left.lists } else { left.chars };
                        node = right;
                    }
                }
            }
        }
    }

    pub(crate) fn char_offset(&self, paragraph: usize) -> usize {
        self.prefix(paragraph, false)
    }

    pub(crate) fn contains_list(&self, range: Range<usize>) -> bool {
        self.prefix(range.end, true) > self.prefix(range.start, true)
    }

    pub(super) fn rebuild(&mut self, paragraphs: &[ParagraphCache], environment_changed: bool) {
        let environment = if environment_changed {
            Arc::new(())
        } else {
            self.version
                .as_ref()
                .map_or_else(|| Arc::new(()), |version| version.environment.clone())
        };
        self.version = Some(Arc::new(LayoutVersion {
            root: build(paragraphs.iter().map(Entry::from_cache)),
            environment,
        }));
    }

    fn publish_root(&mut self, root: Option<Arc<Node>>) {
        let environment = self
            .version
            .as_ref()
            .map_or_else(|| Arc::new(()), |version| version.environment.clone());
        // Generations stay immutable even when no strong snapshot is retained.
        self.version = Some(Arc::new(LayoutVersion { root, environment }));
    }

    pub(super) fn replace(&mut self, range: Range<usize>, paragraphs: &[ParagraphCache]) {
        if range.len() == paragraphs.len() {
            if !range.is_empty() {
                let root = Some(update(
                    self.root().expect("initialized layouts"),
                    range.start,
                    paragraphs,
                ));
                self.publish_root(root);
            }
            return;
        }
        let (before, rest) = self
            .root()
            .map_or((None, None), |root| split(root.clone(), range.start));
        let (_, after) = if let Some(rest) = rest {
            split(rest, range.len())
        } else {
            (None, None)
        };
        let replacement = build(paragraphs.iter().map(Entry::from_cache));
        self.publish_root(concat(concat(before, replacement), after));
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = ParagraphLayout> + '_ {
        self.iter_range(0..self.len())
    }

    pub(crate) fn iter_range(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = ParagraphLayout> + '_ {
        self.entries_range(range).map(|(entry, y)| entry.at(y))
    }

    pub(crate) fn views_range(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = ParagraphLayoutView<'_>> + '_ {
        self.entries_range(range).map(|(entry, y)| entry.view(y))
    }

    fn entries_range(&self, range: Range<usize>) -> impl Iterator<Item = (&Entry, f64)> + '_ {
        let mut iterator = LayoutIter {
            stack: Vec::new(),
            leaf: [].iter(),
            y: 0.0,
        };
        let mut index = range.start;
        let mut node = self.root().map(Arc::as_ref);
        while let Some(current) = node {
            if index >= current.len {
                break;
            }
            match &current.children {
                Children::Leaf(entries) => {
                    iterator.y += entries[..index]
                        .iter()
                        .map(|entry| entry.height)
                        .sum::<f64>();
                    iterator.leaf = entries[index..].iter();
                    break;
                }
                Children::Branch(left, right) => {
                    if index < left.len {
                        iterator.stack.push(right);
                        node = Some(left);
                    } else {
                        index -= left.len;
                        iterator.y += left.height;
                        node = Some(right);
                    }
                }
            }
        }
        iterator.take(range.end.saturating_sub(range.start))
    }

    #[cfg(test)]
    pub(crate) fn from_test_layouts(layouts: Vec<ParagraphLayout>) -> Self {
        let mut y = 0.0;
        let entries: Vec<_> = layouts
            .into_iter()
            .map(|mut layout| {
                let height = f64::from(layout.galley.size().y + PARAGRAPH_GAP);
                layout.rect = layout.rect.translate(egui::vec2(0.0, -(y as f32)));
                layout.marker_position.y -= y as f32;
                y += height;
                Entry {
                    chars: layout.galley.end().index.0 + 1,
                    layout,
                    height,
                }
            })
            .collect();
        Self {
            version: Some(Arc::new(LayoutVersion {
                root: build(entries),
                environment: Arc::new(()),
            })),
        }
    }
}

struct LayoutIter<'a> {
    stack: Vec<&'a Node>,
    leaf: std::slice::Iter<'a, Entry>,
    y: f64,
}

impl<'a> Iterator for LayoutIter<'a> {
    type Item = (&'a Entry, f64);
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.leaf.next() {
                let y = self.y;
                self.y += entry.height;
                return Some((entry, y));
            }
            match &self.stack.pop()?.children {
                Children::Leaf(entries) => self.leaf = entries.iter(),
                Children::Branch(left, right) => {
                    self.stack.push(right);
                    self.stack.push(left);
                }
            }
        }
    }
}
