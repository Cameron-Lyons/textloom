use std::sync::Arc;

use super::Paragraph;
use crate::ParagraphKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Level {
    indent: u8,
    next: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Levels {
    open: Vec<Level>,
}

impl Levels {
    pub(crate) fn continuation(&self, indent: u8) -> u32 {
        self.open_run(indent).unwrap_or(1)
    }

    pub(crate) fn open_run(&self, indent: u8) -> Option<u32> {
        self.open
            .iter()
            .rev()
            .find(|level| level.indent == indent)
            .map(|level| level.next)
    }

    pub(crate) fn push(&mut self, kind: ParagraphKind) -> Option<u32> {
        match kind {
            ParagraphKind::Ordered { indent, start } => {
                let number = start.unwrap_or_else(|| self.continuation(indent));
                self.close_deeper(indent);
                let next = number.saturating_add(1);
                match self.open.last_mut() {
                    Some(last) if last.indent == indent => last.next = next,
                    _ => self.open.push(Level { indent, next }),
                }
                Some(number)
            }
            ParagraphKind::Bullet { indent } => {
                self.close_deeper(indent);
                None
            }
            ParagraphKind::Heading { .. } => {
                self.open.clear();
                None
            }
            ParagraphKind::Body | ParagraphKind::Quote => None,
        }
    }

    fn close_deeper(&mut self, indent: u8) {
        while self.open.last().is_some_and(|open| open.indent > indent) {
            self.open.pop();
        }
    }

    pub(super) fn before(
        paragraphs: &[Arc<Paragraph>],
        index: usize,
        mut number: impl FnMut(usize) -> Option<u32>,
    ) -> Self {
        let mut open = Vec::new();
        let mut limit = u16::from(u8::MAX);
        for (index, paragraph) in paragraphs[..index].iter().enumerate().rev() {
            match paragraph.kind {
                ParagraphKind::Ordered { indent, .. } => {
                    if u16::from(indent) <= limit
                        && open
                            .last()
                            .is_none_or(|level: &Level| level.indent > indent)
                    {
                        let next = number(index)
                            .expect("ordered items are numbered")
                            .saturating_add(1);
                        open.push(Level { indent, next });
                        if indent == 0 {
                            break;
                        }
                    }
                    limit = limit.min(indent.into());
                }
                ParagraphKind::Bullet { indent } => limit = limit.min(indent.into()),
                ParagraphKind::Heading { .. } => break,
                ParagraphKind::Body | ParagraphKind::Quote => {}
            }
        }
        open.reverse();
        Self { open }
    }
}

pub(super) fn numbers(paragraphs: &[Arc<Paragraph>]) -> Arc<[(usize, u32)]> {
    let mut levels = Levels::default();
    paragraphs
        .iter()
        .enumerate()
        .filter_map(|(index, paragraph)| levels.push(paragraph.kind).map(|number| (index, number)))
        .collect()
}

pub(crate) fn same_effect(before: ParagraphKind, after: ParagraphKind) -> bool {
    before == after
        || matches!(
            (before, after),
            (
                ParagraphKind::Body | ParagraphKind::Quote,
                ParagraphKind::Body | ParagraphKind::Quote
            ) | (ParagraphKind::Heading { .. }, ParagraphKind::Heading { .. })
        )
}

pub(super) fn surviving_kind(
    removed: impl IntoIterator<Item = ParagraphKind>,
    kind: ParagraphKind,
) -> ParagraphKind {
    let ParagraphKind::Ordered {
        indent,
        start: None,
    } = kind
    else {
        return kind;
    };
    let mut start = None;
    for removed in removed {
        match removed {
            ParagraphKind::Ordered {
                indent: level,
                start: explicit,
            } if level == indent => start = explicit.or(start),
            ParagraphKind::Ordered { indent: level, .. }
            | ParagraphKind::Bullet { indent: level }
                if level < indent =>
            {
                start = None;
            }
            ParagraphKind::Heading { .. } => start = None,
            _ => {}
        }
    }
    ParagraphKind::Ordered { indent, start }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(indent: u8, start: Option<u32>) -> ParagraphKind {
        ParagraphKind::Ordered { indent, start }
    }

    fn numbered(kinds: &[ParagraphKind]) -> Vec<Option<u32>> {
        let mut levels = Levels::default();
        kinds.iter().map(|kind| levels.push(*kind)).collect()
    }

    fn paragraphs(kinds: &[ParagraphKind]) -> Vec<Arc<Paragraph>> {
        kinds
            .iter()
            .map(|kind| Arc::new(Paragraph::plain("item")).with_kind(*kind))
            .collect()
    }

    #[test]
    fn runs_continue_across_body_text_quotes_and_bullets_at_their_level() {
        let bullet = |indent| ParagraphKind::Bullet { indent };
        assert_eq!(
            numbered(&[
                ordered(0, None),
                ordered(1, None),
                bullet(2),
                ordered(1, None),
                ordered(0, None),
                ordered(1, None),
                bullet(0),
                ordered(0, None),
                ParagraphKind::Body,
                ParagraphKind::Quote,
                ordered(0, None),
                ordered(0, Some(7)),
                ordered(0, None),
                ParagraphKind::Heading { level: 1 },
                ordered(0, None),
                ordered(2, Some(u32::MAX)),
                ordered(2, None),
            ]),
            [
                Some(1),
                Some(1),
                None,
                Some(2),
                Some(2),
                Some(1),
                None,
                Some(3),
                None,
                None,
                Some(4),
                Some(7),
                Some(8),
                None,
                Some(1),
                Some(u32::MAX),
                Some(u32::MAX),
            ]
        );
    }

    #[test]
    fn shallower_items_and_headings_end_deeper_lists() {
        assert_eq!(
            numbered(&[
                ordered(1, None),
                ordered(1, None),
                ParagraphKind::Bullet { indent: 0 },
                ordered(1, None),
                ParagraphKind::Body,
                ordered(1, None),
                ParagraphKind::Heading { level: 2 },
                ordered(1, None),
            ]),
            [
                Some(1),
                Some(2),
                None,
                Some(1),
                None,
                Some(2),
                None,
                Some(1)
            ]
        );
    }

    #[test]
    fn levels_restored_from_preceding_items_match_a_forward_pass() {
        let mut seed = 0x1357_9bdf_2468_ace0u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..5000 {
            let len = (next() % 10) as usize;
            let kinds: Vec<_> = (0..len)
                .map(|_| match next() % 7 {
                    0 => ParagraphKind::Body,
                    1 => ParagraphKind::Quote,
                    2 => ParagraphKind::Heading { level: 1 },
                    3 => ParagraphKind::Bullet {
                        indent: (next() % 4) as u8,
                    },
                    _ => ordered(
                        (next() % 4) as u8,
                        (next() % 3 == 0).then(|| (next() % 5) as u32),
                    ),
                })
                .collect();
            let paragraphs = paragraphs(&kinds);
            let numbers = numbered(&kinds);
            for end in 0..=kinds.len() {
                let mut forward = Levels::default();
                for kind in &kinds[..end] {
                    forward.push(*kind);
                }
                let restored = Levels::before(&paragraphs, end, |index| numbers[index]);
                assert_eq!(restored, forward, "{kinds:?} before paragraph {end}");
            }
        }
    }

    #[test]
    fn a_surviving_item_keeps_the_start_of_the_list_it_continued() {
        let kept = surviving_kind(
            [ordered(0, Some(5)), ParagraphKind::Body, ordered(1, None)],
            ordered(0, None),
        );
        assert_eq!(kept, ordered(0, Some(5)));
        assert_eq!(
            surviving_kind([ordered(0, Some(5)), ordered(0, Some(9))], ordered(0, None)),
            ordered(0, Some(9))
        );
        for barrier in [
            ParagraphKind::Heading { level: 1 },
            ParagraphKind::Bullet { indent: 0 },
        ] {
            assert_eq!(
                surviving_kind([ordered(1, Some(5)), barrier], ordered(1, None)),
                ordered(1, None)
            );
        }
        assert_eq!(
            surviving_kind([ordered(0, Some(5))], ordered(0, Some(2))),
            ordered(0, Some(2))
        );
    }
}
