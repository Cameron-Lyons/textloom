use std::{fmt::Write, sync::Arc};

use crate::{
    Color, Link, Paragraph, ParagraphKind,
    document::{LINE_SEPARATOR, Levels},
};

pub(crate) fn plain_text(paragraphs: &[Arc<Paragraph>]) -> String {
    let capacity = paragraphs
        .iter()
        .map(|paragraph| paragraph.text().len())
        .sum::<usize>()
        .saturating_add(paragraphs.len().saturating_sub(1));
    let mut output = String::with_capacity(capacity);
    for (index, paragraph) in paragraphs.iter().enumerate() {
        if index != 0 {
            output.push('\n');
        }
        push_plain(&mut output, paragraph.text());
    }
    output
}

pub(crate) fn push_plain(output: &mut String, text: &str) {
    let mut lines = text.split(LINE_SEPARATOR);
    output.push_str(lines.next().unwrap_or_default());
    for line in lines {
        output.push('\n');
        output.push_str(line);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Ordered,
}

struct List {
    indent: u8,
    kind: ListKind,
    next: u64,
}

pub(crate) fn html(paragraphs: &[Arc<Paragraph>], mut numbers: Levels) -> String {
    let mut output = String::from("<div style=\"white-space:pre-wrap\">");
    let mut lists: Vec<List> = Vec::new();
    let mut quoting = false;
    for paragraph in paragraphs {
        let number = numbers.push(paragraph.kind());
        let quote = paragraph.kind() == ParagraphKind::Quote;
        if quoting && !quote {
            output.push_str("</blockquote>");
        }
        let current = match paragraph.kind() {
            ParagraphKind::Bullet { indent } => Some((ListKind::Bullet, indent, 0)),
            ParagraphKind::Ordered { indent, .. } => Some((
                ListKind::Ordered,
                indent,
                number.expect("ordered items are numbered"),
            )),
            _ => None,
        };
        if let Some((kind, indent, start)) = current {
            while lists.last().is_some_and(|list| list.indent > indent) {
                close_list(&mut output, lists.pop().unwrap().kind);
            }
            if let Some(list) = lists.pop_if(|list| list.indent == indent && list.kind != kind) {
                close_list(&mut output, list.kind);
            } else if lists.last().is_some_and(|list| list.indent == indent) {
                output.push_str("</li>");
            }
            if lists
                .last()
                .is_none_or(|list| list.indent != indent || list.kind != kind)
            {
                match kind {
                    ListKind::Bullet => output.push_str("<ul>"),
                    ListKind::Ordered => {
                        write!(output, "<ol start=\"{start}\">").unwrap();
                    }
                }
                lists.push(List {
                    indent,
                    kind,
                    next: u64::from(start),
                });
            }
            let list = lists.last_mut().unwrap();
            if kind == ListKind::Ordered && list.next != u64::from(start) {
                write!(output, "<li value=\"{start}\">").unwrap();
            } else {
                output.push_str("<li>");
            }
            list.next = u64::from(start) + 1;
            inline(&mut output, paragraph);
        } else {
            while let Some(list) = lists.pop() {
                close_list(&mut output, list.kind);
            }
            if quote && !quoting {
                output.push_str("<blockquote>");
            }
            match paragraph.kind() {
                ParagraphKind::Heading { level } => {
                    write!(output, "<h{level}>").unwrap();
                    inline(&mut output, paragraph);
                    write!(output, "</h{level}>").unwrap();
                }
                _ => {
                    output.push_str("<p>");
                    inline(&mut output, paragraph);
                    output.push_str("</p>");
                }
            }
        }
        quoting = quote;
    }
    while let Some(list) = lists.pop() {
        close_list(&mut output, list.kind);
    }
    if quoting {
        output.push_str("</blockquote>");
    }
    output.push_str("</div>");
    output
}

fn close_list(output: &mut String, kind: ListKind) {
    output.push_str(match kind {
        ListKind::Bullet => "</li></ul>",
        ListKind::Ordered => "</li></ol>",
    });
}

fn inline(output: &mut String, paragraph: &Paragraph) {
    if paragraph.text().is_empty() {
        output.push_str("<br>");
    }
    let mut anchor: Option<&Link> = None;
    for span in paragraph.spans() {
        let link = span.link.as_ref().filter(|link| is_safe_href(link));
        if anchor != link {
            if anchor.is_some() {
                output.push_str("</a>");
            }
            if let Some(link) = link {
                output.push_str("<a href=\"");
                escape(output, link.as_str());
                output.push_str("\">");
            }
            anchor = link;
        }
        let style = span.style;
        if style.bold {
            output.push_str("<strong>");
        }
        if style.italic {
            output.push_str("<em>");
        }
        if style.code {
            output.push_str("<code>");
        }
        let colored = style.foreground.is_some() || style.background.is_some();
        if colored {
            output.push_str("<span style=\"");
            if let Some(color) = style.foreground {
                output.push_str("color:");
                css_color(output, color);
            }
            if let Some(fill) = style.background {
                if style.foreground.is_some() {
                    output.push(';');
                }
                output.push_str("background-color:");
                css_color(output, fill);
            }
            output.push_str("\">");
        }
        if style.underline {
            output.push_str("<u>");
        }
        if style.strikethrough {
            output.push_str("<s>");
        }
        escape(output, &paragraph.text()[span.range.clone()]);
        if style.strikethrough {
            output.push_str("</s>");
        }
        if style.underline {
            output.push_str("</u>");
        }
        if colored {
            output.push_str("</span>");
        }
        if style.code {
            output.push_str("</code>");
        }
        if style.italic {
            output.push_str("</em>");
        }
        if style.bold {
            output.push_str("</strong>");
        }
    }
    if anchor.is_some() {
        output.push_str("</a>");
    }
    if paragraph.text().ends_with(LINE_SEPARATOR) {
        output.push_str("<br>");
    }
}

fn is_safe_href(link: &Link) -> bool {
    let target = link.as_str().trim();
    !["javascript:", "vbscript:", "data:"].iter().any(|scheme| {
        target
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

fn css_color(output: &mut String, Color([red, green, blue, alpha]): Color) {
    if alpha == 255 {
        write!(output, "#{red:02x}{green:02x}{blue:02x}").unwrap();
    } else {
        let opacity = (u32::from(alpha) * 1_000_000).div_ceil(255);
        write!(output, "rgba({red},{green},{blue},0.{opacity:06})").unwrap();
    }
}

fn escape(output: &mut String, text: &str) {
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        let escaped = match byte {
            0 => "\u{fffd}",
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'\"' => "&quot;",
            b'\'' => "&#39;",
            0xe2 if text[index..].starts_with(LINE_SEPARATOR) => "<br>",
            _ => continue,
        };
        output.push_str(&text[start..index]);
        output.push_str(escaped);
        start = index + text[index..].chars().next().map_or(1, char::len_utf8);
    }
    output.push_str(&text[start..]);
}
