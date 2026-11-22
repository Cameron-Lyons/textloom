//! AccessKit roles and text properties shared by the `accesskit` bridge and the
//! egui adapter's bridge, using egui's copy of AccessKit when only `egui` is
//! enabled.

use std::ops::Range;

use crate::{InlineStyle, ParagraphKind};
#[cfg(feature = "accesskit")]
use accesskit::{Color, ListStyle, Node, Role, TextDecoration, TextDecorationStyle};
#[cfg(not(feature = "accesskit"))]
use egui::accesskit::{Color, ListStyle, Node, Role, TextDecoration, TextDecorationStyle};

/// A consecutive fragment of one list. Interruptions split its accessible
/// containers so reading order stays intact, while positions and size describe
/// the entire logical set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListGroup {
    pub(crate) paragraphs: Range<usize>,
    pub(crate) first_position: usize,
    pub(crate) size: usize,
}

pub(crate) fn list_groups(
    kinds: impl Iterator<Item = ParagraphKind>,
) -> impl Iterator<Item = ListGroup> {
    // At a given indent, bullets and numbered items can interrupt each other.
    struct Level {
        indent: u8,
        ordered: Option<usize>,
        bullet: Option<usize>,
    }
    let mut open: Vec<Level> = Vec::new();
    let mut sizes: Vec<usize> = Vec::new();
    // Logical set, consecutive source paragraphs, and zero-based first position.
    let mut groups: Vec<(usize, Range<usize>, usize)> = Vec::new();
    for (index, kind) in kinds.enumerate() {
        let (ordered, indent, restart) = match kind {
            ParagraphKind::Bullet { indent } => (false, indent, false),
            ParagraphKind::Ordered { indent, start } => (true, indent, start.is_some()),
            ParagraphKind::Heading { .. } => {
                open.clear();
                continue;
            }
            _ => continue,
        };
        while open.last().is_some_and(|level| level.indent > indent) {
            open.pop();
        }
        if open.last().is_none_or(|level| level.indent != indent) {
            open.push(Level {
                indent,
                ordered: None,
                bullet: None,
            });
        }
        let level = open.last_mut().expect("the current indent is open");
        let slot = if ordered {
            &mut level.ordered
        } else {
            &mut level.bullet
        };
        if restart {
            *slot = None;
        }
        let set = *slot.get_or_insert_with(|| {
            let id = sizes.len();
            sizes.push(0);
            id
        });
        if let Some((last_set, paragraphs, _)) = groups.last_mut()
            && *last_set == set
            && paragraphs.end == index
        {
            paragraphs.end += 1;
        } else {
            groups.push((set, index..index + 1, sizes[set]));
        }
        sizes[set] += 1;
    }
    groups
        .into_iter()
        .map(move |(set, paragraphs, first_position)| ListGroup {
            paragraphs,
            first_position,
            size: sizes[set],
        })
}

/// Sets the role of a paragraph node for its kind, with the heading level, or the
/// list level and marker style of a list item.
pub(crate) fn set_paragraph_semantics(node: &mut Node, kind: ParagraphKind) {
    node.set_role(match kind {
        ParagraphKind::Body | ParagraphKind::Quote => Role::Paragraph,
        ParagraphKind::Heading { .. } => Role::Heading,
        ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } => Role::ListItem,
    });
    match kind {
        ParagraphKind::Heading { level } => node.set_level(usize::from(level) - 1),
        ParagraphKind::Bullet { indent } => {
            node.set_level(usize::from(indent));
            node.set_list_style(ListStyle::Disc);
        }
        ParagraphKind::Ordered { indent, .. } => {
            node.set_level(usize::from(indent));
            node.set_list_style(ListStyle::Numeric);
        }
        ParagraphKind::Body | ParagraphKind::Quote => {}
    }
}

/// Sets the text properties of a text run node for its inline style.
pub(crate) fn apply_text_style(node: &mut Node, style: InlineStyle) {
    let InlineStyle {
        bold,
        italic,
        underline,
        strikethrough,
        code,
        foreground,
        background,
    } = style;
    node.set_font_weight(if bold { 700.0 } else { 400.0 });
    if italic {
        node.set_italic();
    }
    if code {
        node.set_font_family("monospace");
    }
    let foreground = foreground.map(to_accesskit_color);
    if let Some(color) = foreground {
        node.set_foreground_color(color);
    }
    if let Some(color) = background {
        node.set_background_color(to_accesskit_color(color));
    }
    let decoration = TextDecoration {
        style: TextDecorationStyle::Solid,
        color: foreground.unwrap_or(Color {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 255,
        }),
    };
    if underline {
        node.set_underline(decoration);
    }
    if strikethrough {
        node.set_strikethrough(decoration);
    }
}

// `crate::Color` stays qualified because AccessKit's `Color` is imported.
fn to_accesskit_color(crate::Color([red, green, blue, alpha]): crate::Color) -> Color {
    Color {
        red,
        green,
        blue,
        alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_and_list_levels_use_accesskits_zero_based_indices() {
        for (kind, expected) in [
            (ParagraphKind::Heading { level: 1 }, 0),
            (ParagraphKind::Heading { level: 6 }, 5),
            (ParagraphKind::Bullet { indent: 0 }, 0),
            (
                ParagraphKind::Ordered {
                    indent: u8::MAX,
                    start: None,
                },
                255,
            ),
        ] {
            let mut node = Node::new(Role::Paragraph);
            set_paragraph_semantics(&mut node, kind);
            assert_eq!(node.level(), Some(expected));
        }
    }

    #[test]
    fn list_sets_respect_nesting_kind_restarts_and_headings() {
        let ordered = |indent| ParagraphKind::Ordered {
            indent,
            start: None,
        };
        let bullet = |indent| ParagraphKind::Bullet { indent };
        // Each item's position in its set and the set's size.
        let positions = |kinds: &[ParagraphKind]| -> Vec<Option<(usize, usize)>> {
            let mut positions = vec![None; kinds.len()];
            for group in list_groups(kinds.iter().copied()) {
                for (n, index) in group.paragraphs.enumerate() {
                    positions[index] = Some((group.first_position + n, group.size));
                }
            }
            positions
        };
        assert_eq!(
            positions(&[ordered(0), ordered(1), ordered(0)]),
            [Some((0, 2)), Some((0, 1)), Some((1, 2))]
        );
        assert_eq!(
            positions(&[
                ordered(0),
                ordered(2),
                ordered(1),
                ordered(1),
                ordered(0),
                ordered(1)
            ]),
            [
                Some((0, 2)),
                Some((0, 1)),
                Some((0, 2)),
                Some((1, 2)),
                Some((1, 2)),
                Some((0, 1)),
            ]
        );
        assert_eq!(
            positions(&[bullet(0), ordered(1), bullet(0), ordered(0), bullet(0)]),
            [
                Some((0, 3)),
                Some((0, 1)),
                Some((1, 3)),
                Some((0, 1)),
                Some((2, 3)),
            ]
        );
        let restart = ParagraphKind::Ordered {
            indent: 0,
            start: Some(5),
        };
        assert_eq!(
            positions(&[
                ordered(0),
                ParagraphKind::Body,
                ordered(0),
                restart,
                ordered(0)
            ]),
            [Some((0, 2)), None, Some((1, 2)), Some((0, 2)), Some((1, 2))]
        );
        assert_eq!(
            positions(&[
                bullet(0),
                bullet(1),
                ParagraphKind::Heading { level: 2 },
                bullet(1),
                ParagraphKind::Body,
                bullet(0),
            ]),
            [
                Some((0, 1)),
                Some((0, 1)),
                None,
                Some((0, 1)),
                None,
                Some((0, 1)),
            ]
        );
        let groups: Vec<_> = list_groups(
            [
                ordered(0),
                ordered(0),
                ordered(1),
                ordered(0),
                ParagraphKind::Body,
                ordered(0),
                restart,
                ordered(0),
            ]
            .into_iter(),
        )
        .map(|group| (group.paragraphs, group.first_position, group.size))
        .collect();
        assert_eq!(
            groups,
            [
                (0..2, 0, 4),
                (2..3, 0, 1),
                (3..4, 2, 4),
                (5..6, 3, 4),
                (6..8, 0, 2)
            ]
        );
    }
}
