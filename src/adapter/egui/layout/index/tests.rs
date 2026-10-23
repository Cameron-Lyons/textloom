use super::{Children, Entry, LEAF_CAPACITY, Node, ParagraphLayouts};
use crate::adapter::egui::PARAGRAPH_GAP;
use crate::adapter::egui::layout::{ParagraphCache, ParagraphLayout};
use crate::model::NumberedItem;
use crate::{Document, Paragraph};
use std::collections::HashSet;
use std::sync::Arc;

struct Fixture {
    galley: Arc<egui::Galley>,
    paragraph: Arc<Paragraph>,
}

impl Fixture {
    fn new() -> Self {
        let context = egui::Context::default();
        let mut galley = None;
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            galley = Some(ui.fonts_mut(|fonts| {
                fonts.layout_job(egui::text::LayoutJob::simple_singleline(
                    "index fixture".to_owned(),
                    egui::FontId::proportional(13.0),
                    egui::Color32::WHITE,
                ))
            }));
        });
        output.textures_delta.clear();
        Self {
            galley: galley.unwrap(),
            paragraph: Document::from_text("index fixture").paragraphs()[0].clone(),
        }
    }

    fn cache(&self, id: usize) -> ParagraphCache {
        let mut galley = (*self.galley).clone();
        galley.rect = egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(20.0 + (id % 17) as f32, 12.0 + (id % 11) as f32 * 0.125),
        );
        let marker = id.is_multiple_of(4).then(|| self.galley.clone());
        let indent = (id % 5) as f32 * 3.0;
        let quote_bar = (id % 4 == 1).then(|| {
            egui::Rect::from_min_size(egui::pos2(1.0, 0.0), egui::vec2(2.0, galley.rect.height()))
        });
        let marker_x = indent - marker.as_ref().map_or(0.0, |marker| marker.size().x) - 8.0;
        ParagraphCache {
            paragraph: self.paragraph.clone(),
            number: None,
            chars: id + 1,
            layout: ParagraphLayout {
                rect: egui::Rect::from_min_size(egui::pos2(indent, 0.0), galley.rect.size()),
                marker_position: egui::pos2(marker_x, 0.0),
                quote_bar,
                galley: Arc::new(galley),
                marker,
            },
        }
    }

    fn caches(&self, start: usize, len: usize) -> Vec<ParagraphCache> {
        (start..start + len).map(|id| self.cache(id)).collect()
    }
}

// The aggregates a node caches about its subtree.
#[derive(Debug, PartialEq)]
struct Summary {
    len: usize,
    height: f64,
    chars: usize,
    lists: usize,
    depth: u16,
}

fn assert_tree(node: &Node) -> Summary {
    let expected = match &node.children {
        Children::Leaf(entries) => {
            assert!(!entries.is_empty());
            assert!(entries.len() <= LEAF_CAPACITY);
            Summary {
                len: entries.len(),
                height: entries.iter().map(|entry| entry.height).sum(),
                chars: entries.iter().map(Entry::chars).sum(),
                lists: entries
                    .iter()
                    .filter(|entry| entry.layout().marker.is_some())
                    .count(),
                depth: 1,
            }
        }
        Children::Branch(left, right) => {
            let left = assert_tree(left);
            let right = assert_tree(right);
            assert!(
                left.depth.abs_diff(right.depth) <= 1,
                "unbalanced depths {} and {}",
                left.depth,
                right.depth
            );
            Summary {
                len: left.len + right.len,
                height: left.height + right.height,
                chars: left.chars + right.chars,
                lists: left.lists + right.lists,
                depth: left.depth.max(right.depth) + 1,
            }
        }
    };
    let actual = Summary {
        len: node.len,
        height: node.height,
        chars: node.chars,
        lists: node.lists,
        depth: node.depth,
    };
    assert_eq!(actual, expected);
    let expected_tail = match &node.children {
        Children::Leaf(entries) => {
            assert!(entries.iter().all(|entry| entry.layout().rect.top() == 0.0));
            let tail = entries.last().unwrap();
            (tail.height, tail.layout().rect.bottom())
        }
        Children::Branch(_, right) => (right.tail_height, right.tail_bottom),
    };
    assert_eq!((node.tail_height, node.tail_bottom), expected_tail);
    expected
}

fn assert_y_partitions(layouts: &ParagraphLayouts, flat: &[ParagraphLayout], point: f32) {
    assert_eq!(
        layouts.first_bottom_at_least(point),
        flat.partition_point(|layout| layout.rect.bottom() < point),
        "bottom partition at {point}"
    );
    assert_eq!(
        layouts.first_top_after(point),
        flat.partition_point(|layout| layout.rect.top() <= point),
        "top partition at {point}"
    );
    assert_eq!(
        layouts.paragraph_at_y(point),
        flat.partition_point(|layout| point > layout.rect.bottom() + PARAGRAPH_GAP / 2.0),
        "hit-test partition at {point}"
    );
}

fn assert_layout_sequence(actual: &[ParagraphLayout], expected: &[ParagraphLayout]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        assert_eq!(actual.rect, expected.rect);
        assert_eq!(actual.marker_position, expected.marker_position);
        assert_eq!(actual.quote_bar, expected.quote_bar);
        assert!(Arc::ptr_eq(&actual.galley, &expected.galley));
        match (&actual.marker, &expected.marker) {
            (Some(actual), Some(expected)) => assert!(Arc::ptr_eq(actual, expected)),
            (None, None) => (),
            _ => panic!("list marker changed during iteration"),
        }
    }
}

fn entry_counts(root: Option<&Node>) -> Vec<usize> {
    fn append(node: &Node, result: &mut Vec<usize>) {
        match &node.children {
            Children::Leaf(entries) => result.extend(entries.iter().map(Entry::chars)),
            Children::Branch(left, right) => {
                append(left, result);
                append(right, result);
            }
        }
    }
    let mut result = Vec::new();
    if let Some(node) = root {
        assert_tree(node);
        append(node, &mut result);
    }
    result
}

fn assert_layouts(layouts: &ParagraphLayouts, expected: &[ParagraphCache]) {
    assert_eq!(layouts.len(), expected.len());
    assert_eq!(
        entry_counts(layouts.root().map(Arc::as_ref)),
        expected
            .iter()
            .map(|cache| cache.chars + 1)
            .collect::<Vec<_>>()
    );
    let mut y = 0.0_f64;
    let mut chars = 0;
    let mut flat = Vec::with_capacity(expected.len());
    for (index, cache) in expected.iter().enumerate() {
        assert_eq!(layouts.char_offset(index), chars);
        chars += cache.chars + 1;
        let layout = layouts.get(index).unwrap();
        let offset = egui::vec2(0.0, y as f32);
        assert!(Arc::ptr_eq(&layout.galley, &cache.layout.galley));
        assert_eq!(layout.rect, cache.layout.rect.translate(offset));
        assert_eq!(
            layout.marker_position,
            cache.layout.marker_position + offset
        );
        assert_eq!(
            layout.quote_bar,
            cache.layout.quote_bar.map(|bar| bar.translate(offset))
        );
        match (&layout.marker, &cache.layout.marker) {
            (Some(actual), Some(expected)) => assert!(Arc::ptr_eq(actual, expected)),
            (None, None) => (),
            _ => panic!("list marker changed"),
        }
        y += f64::from(cache.layout.galley.size().y + PARAGRAPH_GAP);
        flat.push(layout);
    }
    assert_eq!(layouts.char_offset(expected.len()), chars);
    assert_eq!(layouts.char_offset(expected.len() + 10), chars);
    assert!(layouts.get(expected.len()).is_none());
    assert!(layouts.get(usize::MAX).is_none());
    assert_eq!(
        layouts.last().map(|layout| layout.rect),
        flat.last().map(|layout| layout.rect)
    );
    assert_eq!(
        layouts.content_height(),
        flat.last().map(|layout| layout.rect.bottom())
    );
    let iterated: Vec<_> = layouts.iter().collect();
    assert_eq!(iterated.len(), flat.len());
    for (actual, expected) in iterated.iter().zip(&flat) {
        assert_eq!(actual.rect, expected.rect);
        assert_eq!(actual.marker_position, expected.marker_position);
        assert_eq!(actual.quote_bar, expected.quote_bar);
        assert!(Arc::ptr_eq(&actual.galley, &expected.galley));
    }
    for point in [
        -1.0,
        0.0,
        y as f32 / 3.0,
        y as f32 / 2.0,
        y as f32,
        y as f32 + 1.0,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NAN,
    ] {
        assert_eq!(
            layouts.partition_point(|layout| layout.rect.bottom() < point),
            flat.partition_point(|layout| layout.rect.bottom() < point),
            "bottom predicate at {point}"
        );
        assert_eq!(
            layouts.partition_point(|layout| layout.rect.top() <= point),
            flat.partition_point(|layout| layout.rect.top() <= point),
            "top predicate at {point}"
        );
        assert_y_partitions(layouts, &flat, point);
    }
    for start in [0, expected.len() / 3, expected.len() / 2, expected.len()] {
        for end in [start, start + (expected.len() - start) / 2, expected.len()] {
            assert_eq!(
                layouts.contains_list(start..end),
                expected[start..end]
                    .iter()
                    .any(|cache| cache.layout.marker.is_some()),
                "paragraphs {start}..{end}"
            );
        }
    }
}

#[test]
fn placeholder_defaults_allocate_no_generation_and_initialized_empty_versions_are_distinct() {
    let mut layouts = ParagraphLayouts::default();
    let placeholder = layouts.identity();
    assert!(layouts.version.is_none());
    assert!(placeholder.matches(&layouts));
    assert!(placeholder.matches(&ParagraphLayouts::default()));
    assert_layouts(&layouts, &[]);
    layouts.replace(0..0, &[]);
    assert!(layouts.version.is_none());
    assert!(placeholder.matches(&layouts));

    layouts.rebuild(&[]);
    let initialized = layouts.identity();
    let environment = layouts.environment_identity();
    assert!(layouts.version.is_some());
    assert!(!placeholder.matches(&layouts));
    assert!(!initialized.matches(&ParagraphLayouts::default()));
    assert_layouts(&layouts, &[]);
    layouts.replace(0..0, &[]);
    assert!(initialized.matches(&layouts));

    assert!(Arc::ptr_eq(&environment, &layouts.environment_identity()));

    let retained_empty = layouts.clone();
    layouts.rebuild(&[]);
    assert!(!initialized.matches(&layouts));
    assert!(initialized.matches(&retained_empty));
    assert!(!Arc::ptr_eq(&environment, &layouts.environment_identity()));
    assert_layouts(&layouts, &[]);

    let fixture = Fixture::new();
    let appended = fixture.caches(0, 2);
    layouts.replace(0..0, &appended);
    assert_layouts(&layouts, &appended);
    let populated = layouts.identity();
    layouts.replace(0..appended.len(), &[]);
    assert!(!populated.matches(&layouts));
    assert!(layouts.version.is_some());
    assert_layouts(&layouts, &[]);
}

#[test]
fn weak_generation_tokens_do_not_retain_geometry_or_environment_after_replacement() {
    let fixture = Fixture::new();
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&fixture.caches(0, 4));
    let identity = layouts.identity();
    let root = Arc::downgrade(layouts.root().unwrap());
    let environment = Arc::downgrade(&layouts.environment_identity());
    assert!(identity.matches(&layouts));

    layouts.replace(0..4, &fixture.caches(100, 4));
    assert!(!identity.matches(&layouts));
    assert!(identity.0.upgrade().is_none());
    assert!(root.upgrade().is_none());
    assert!(environment.upgrade().is_some());
    assert!(identity.ptr_eq(&identity.clone()));
    drop(layouts);
    assert!(environment.upgrade().is_none());
    assert!(!identity.matches(&ParagraphLayouts::default()));
}

#[test]
fn retained_geometry_snapshots_keep_their_generation_until_the_snapshot_is_dropped() {
    let fixture = Fixture::new();
    let original = fixture.caches(0, 4);
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&original);
    let identity = layouts.identity();
    let root = Arc::downgrade(layouts.root().unwrap());
    let retained = layouts.clone();
    assert_eq!(Arc::strong_count(layouts.version.as_ref().unwrap()), 2);
    assert_eq!(Arc::strong_count(layouts.root().unwrap()), 1);

    layouts.replace(0..4, &fixture.caches(100, 6));
    assert!(identity.matches(&retained));
    assert!(!identity.matches(&layouts));
    assert_layouts(&retained, &original);
    assert!(root.upgrade().is_some());
    drop(retained);
    assert!(identity.0.upgrade().is_none());
    assert!(root.upgrade().is_none());
}

#[test]
fn y_partitions_match_rectangle_predicates_at_every_edge_and_gap_midpoint() {
    let fixture = Fixture::new();
    for len in [0, 1, 31, 32, 33, 65, 257, 1025] {
        for large_first_paragraph in [false, true] {
            let mut caches = fixture.caches(0, len);
            if large_first_paragraph && let Some(first) = caches.first_mut() {
                Arc::make_mut(&mut first.layout.galley).rect.max.y = 16_777_216.0;
                first.layout.rect.max.y = 16_777_216.0;
            }
            let mut layouts = ParagraphLayouts::default();
            layouts.rebuild(&caches);
            let mut y = 0.0_f64;
            let flat: Vec<_> = caches
                .iter()
                .map(|cache| {
                    let offset = egui::vec2(0.0, y as f32);
                    let layout = ParagraphLayout {
                        galley: cache.layout.galley.clone(),
                        rect: cache.layout.rect.translate(offset),
                        marker: cache.layout.marker.clone(),
                        marker_position: cache.layout.marker_position + offset,
                        quote_bar: cache.layout.quote_bar.map(|bar| bar.translate(offset)),
                    };
                    y += f64::from(cache.layout.galley.size().y + PARAGRAPH_GAP);
                    layout
                })
                .collect();
            for layout in &flat {
                for edge in [
                    layout.rect.top(),
                    layout.rect.bottom(),
                    layout.rect.bottom() + PARAGRAPH_GAP / 2.0,
                ] {
                    for point in [edge.next_down(), edge, edge.next_up()] {
                        assert_y_partitions(&layouts, &flat, point);
                    }
                }
            }
            for point in [f32::NEG_INFINITY, f32::INFINITY, f32::NAN, -0.0, 0.0] {
                assert_y_partitions(&layouts, &flat, point);
            }
        }
    }
}

#[test]
fn range_iteration_matches_flat_slices_across_leaf_boundaries_and_edits() {
    let fixture = Fixture::new();
    let mut caches = fixture.caches(0, 257);
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&caches);
    for replacement in [0, 1, 7, 33, 65] {
        let inserted = fixture.caches(1000 + replacement * 100, replacement);
        layouts.replace(31..65, &inserted);
        caches.splice(31..65, inserted);
        assert_layouts(&layouts, &caches);
        let flat: Vec<_> = (0..layouts.len())
            .map(|index| layouts.get(index).unwrap())
            .collect();
        let mut boundaries = vec![
            0,
            1,
            30,
            31,
            32,
            33,
            63,
            64,
            65,
            layouts.len() / 2,
            layouts.len(),
        ];
        boundaries.retain(|boundary| *boundary <= flat.len());
        boundaries.sort_unstable();
        boundaries.dedup();
        for &start in &boundaries {
            for &end in &boundaries {
                let actual: Vec<_> = layouts.iter_range(start..end).collect();
                let expected = if start <= end { &flat[start..end] } else { &[] };
                assert_layout_sequence(&actual, expected);
            }
            assert_layout_sequence(
                &layouts.iter_range(start..usize::MAX).collect::<Vec<_>>(),
                &flat[start..],
            );
        }
        assert!(
            layouts
                .iter_range(layouts.len() + 1..usize::MAX)
                .next()
                .is_none()
        );
    }
    let empty = ParagraphLayouts::default();
    assert!(empty.iter_range(0..usize::MAX).next().is_none());
}

#[test]
fn every_split_boundary_and_rejoin_matches_a_flat_sequence() {
    let fixture = Fixture::new();
    assert!(super::build(Vec::new()).is_none());
    assert!(super::concat(None, None).is_none());
    for len in [
        1, 2, 31, 32, 33, 63, 64, 65, 95, 127, 128, 129, 255, 256, 257, 513,
    ] {
        let caches = fixture.caches(0, len);
        let root = super::build(caches.iter().cloned().map(Entry::new)).unwrap();
        let counts: Vec<_> = caches.iter().map(|cache| cache.chars + 1).collect();
        for boundary in 0..=len {
            let (before, after) = super::split(root.clone(), boundary);
            assert_eq!(
                entry_counts(before.as_deref()),
                counts[..boundary],
                "len={len}, boundary={boundary}"
            );
            assert_eq!(
                entry_counts(after.as_deref()),
                counts[boundary..],
                "len={len}, boundary={boundary}"
            );
            if boundary == 0 {
                assert!(
                    Arc::ptr_eq(after.as_ref().unwrap(), &root),
                    "splitting at the start keeps the whole tree, len={len}"
                );
            } else if boundary == len {
                assert!(
                    Arc::ptr_eq(before.as_ref().unwrap(), &root),
                    "splitting at the end keeps the whole tree, len={len}"
                );
            }
            assert_eq!(
                entry_counts(super::concat(before, after).as_deref()),
                counts,
                "len={len}, boundary={boundary}"
            );
        }
        assert_eq!(entry_counts(Some(&root)), counts, "len={len}");
    }
}

#[test]
fn repeated_replacements_match_flat_geometry_prefixes_and_retained_snapshots() {
    let fixture = Fixture::new();
    let mut expected = fixture.caches(0, 257);
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&expected);
    let mut snapshots = Vec::new();
    let mut seed = 0xdea1_cafe_babe_1234_u64;
    let mut next_id = expected.len();
    for operation in 0..800 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        if operation % 80 == 0 {
            snapshots.push((layouts.clone(), expected.clone()));
        }
        let start = match operation % 5 {
            0 => 0,
            1 => expected.len(),
            _ => seed as usize % (expected.len() + 1),
        };
        let removed = if operation % 19 == 0 {
            expected.len() - start
        } else {
            ((seed >> 16) as usize % 67).min(expected.len() - start)
        };
        let mut inserted = (seed >> 32) as usize % 39;
        if expected.len() == removed || (removed == 0 && inserted == 0) {
            inserted = 1;
        }
        let replacement = fixture.caches(next_id, inserted);
        next_id += inserted;
        let identity = layouts.identity();
        let environment = layouts.environment_identity();
        layouts.replace(start..start + removed, &replacement);
        expected.splice(start..start + removed, replacement);
        assert!(!layouts.identity().ptr_eq(&identity));
        assert!(Arc::ptr_eq(&layouts.environment_identity(), &environment));
        assert_layouts(&layouts, &expected);
    }
    for (snapshot, oracle) in snapshots {
        assert_layouts(&snapshot, &oracle);
    }
}

fn leaf_addresses(root: &Arc<Node>) -> HashSet<*const Node> {
    fn collect(root: &Arc<Node>, leaves: &mut HashSet<*const Node>) {
        match &root.children {
            Children::Leaf(_) => {
                leaves.insert(Arc::as_ptr(root));
            }
            Children::Branch(left, right) => {
                collect(left, leaves);
                collect(right, leaves);
            }
        }
    }
    let mut leaves = HashSet::new();
    collect(root, &mut leaves);
    leaves
}

#[test]
fn local_changes_share_unchanged_tree_leaves_and_preserve_old_snapshot_geometry() {
    let fixture = Fixture::new();
    let original = fixture.caches(0, 2048);
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&original);
    let snapshot = layouts.clone();
    assert!(Arc::ptr_eq(
        layouts.root().unwrap(),
        snapshot.root().unwrap()
    ));
    let old_leaves = leaf_addresses(snapshot.root().unwrap());
    let replacement = fixture.caches(3000, 3);
    layouts.replace(1023..1025, &replacement);
    let new_leaves = leaf_addresses(layouts.root().unwrap());
    assert!(old_leaves.intersection(&new_leaves).count() >= old_leaves.len() - 4);
    let mut expected = original.clone();
    expected.splice(1023..1025, replacement);
    assert_layouts(&layouts, &expected);
    assert_layouts(&snapshot, &original);
    let environment = layouts.environment_identity();
    let identity = layouts.identity();
    layouts.rebuild(&expected);
    assert!(!Arc::ptr_eq(&layouts.environment_identity(), &environment));
    assert!(!layouts.identity().ptr_eq(&identity));
    assert_layouts(&layouts, &expected);
}

#[test]
fn equal_count_updates_share_every_untouched_leaf_across_tree_seams() {
    let fixture = Fixture::new();
    let original = fixture.caches(0, 2048);
    for range in [0..1, 31..32, 31..34, 1023..1026, 2047..2048, 0..2048] {
        let mut layouts = ParagraphLayouts::default();
        layouts.rebuild(&original);
        let snapshot = layouts.clone();
        let before = leaf_addresses(snapshot.root().unwrap());
        let replacement = fixture.caches(3000, range.len());
        layouts.replace(range.clone(), &replacement);
        let after = leaf_addresses(layouts.root().unwrap());
        let changed_leaves = (range.end - 1) / LEAF_CAPACITY - range.start / LEAF_CAPACITY + 1;
        assert_eq!(
            before.intersection(&after).count(),
            before.len() - changed_leaves,
            "replaced {range:?}"
        );
        let mut expected = original.clone();
        expected.splice(range, replacement);
        assert_layouts(&layouts, &expected);
        assert_layouts(&snapshot, &original);
    }
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&original);
    let snapshot = layouts.clone();
    layouts.replace(32..32, &[]);
    assert!(Arc::ptr_eq(
        layouts.root().unwrap(),
        snapshot.root().unwrap()
    ));
    assert!(layouts.identity().ptr_eq(&snapshot.identity()));
}

#[test]
fn renumbering_rebuilds_only_the_leaves_whose_numbers_change() {
    let fixture = Fixture::new();
    let mut original = fixture.caches(0, 2048);
    for (index, cache) in original.iter_mut().enumerate() {
        cache.number = Some(index as u32);
    }
    let mut layouts = ParagraphLayouts::default();
    layouts.rebuild(&original);
    let snapshot = layouts.clone();
    let current: Vec<_> = (0..2048)
        .step_by(7)
        .map(|index| NumberedItem {
            paragraph: index,
            number: index as u32,
        })
        .collect();
    layouts.renumber(&current, |_, _, _| unreachable!("numbers are current"));
    layouts.renumber(&[], |_, _, _| unreachable!("nothing to renumber"));
    assert!(layouts.identity().ptr_eq(&snapshot.identity()));

    let mut expected = original.clone();
    let mut created = Vec::new();
    layouts.renumber(
        &[(3, 3), (5, 50), (6, 60), (900, 900), (1500, 7)]
            .map(|(paragraph, number)| NumberedItem { paragraph, number }),
        |index, cache, number| {
            created.push(index);
            assert!(Arc::ptr_eq(
                &cache.layout.galley,
                &original[index].layout.galley
            ));
            assert_eq!(cache.number, Some(index as u32));
            let mut replacement = fixture.cache(3000 + index);
            replacement.number = Some(number);
            expected[index] = replacement.clone();
            replacement
        },
    );
    assert_eq!(created, [5, 6, 1500]);
    let before = leaf_addresses(snapshot.root().unwrap());
    let after = leaf_addresses(layouts.root().unwrap());
    assert_eq!(before.intersection(&after).count(), before.len() - 2);
    assert_layouts(&layouts, &expected);
    assert_layouts(&snapshot, &original);
    for (actual, expected) in layouts.caches(0..2048).zip(&expected) {
        assert!(Arc::ptr_eq(&actual.paragraph, &expected.paragraph));
        assert!(Arc::ptr_eq(&actual.layout.galley, &expected.layout.galley));
        assert_eq!(actual.number, expected.number);
        assert_eq!(actual.chars, expected.chars);
        assert_eq!(actual.layout.rect, expected.layout.rect);
        assert_eq!(
            actual.layout.marker_position,
            expected.layout.marker_position
        );
        assert_eq!(actual.layout.quote_bar, expected.layout.quote_bar);
    }
    assert_eq!(layouts.caches(2040..2100).count(), 8);
}

#[test]
fn shared_edges_match_a_flat_comparison_across_leaf_seams() {
    let fixture = Fixture::new();
    let paragraph = || Arc::new((*fixture.paragraph).clone());
    let mut caches = fixture.caches(0, 300);
    for cache in &mut caches {
        cache.paragraph = paragraph();
    }
    let cached: Vec<_> = caches.iter().map(|cache| cache.paragraph.clone()).collect();
    let mut layouts = ParagraphLayouts::default();
    assert_eq!(layouts.shared_edges(&cached), (0, 0));
    layouts.rebuild(&caches);
    for start in [0, 1, 31, 32, 33, 150, 255, 256, 299, 300] {
        for removed in [0, 1, 2, 40, 300] {
            for inserted in [0, 1, 3] {
                let end = (start + removed).min(cached.len());
                let mut source = cached.clone();
                source.splice(start..end, (0..inserted).map(|_| paragraph()));
                let prefix = cached
                    .iter()
                    .zip(&source)
                    .take_while(|(cached, source)| Arc::ptr_eq(cached, source))
                    .count();
                let suffix = cached[prefix..]
                    .iter()
                    .rev()
                    .zip(source[prefix..].iter().rev())
                    .take_while(|(cached, source)| Arc::ptr_eq(cached, source))
                    .count();
                assert_eq!(
                    layouts.shared_edges(&source),
                    (prefix, suffix),
                    "{start}, {removed}, {inserted}"
                );
            }
        }
    }
}
