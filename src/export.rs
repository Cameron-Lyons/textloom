use std::{fmt::Write, sync::Arc};

use crate::{Color, Paragraph, ParagraphKind};

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

pub(crate) fn html(paragraphs: &[Arc<Paragraph>]) -> String {
    let mut output = String::from("<div style=\"white-space:pre-wrap\">");
    let mut lists: Vec<List> = Vec::new();
    for paragraph in paragraphs {
        let current = match paragraph.kind() {
            ParagraphKind::Bullet { indent } => Some((ListKind::Bullet, indent, 0)),
            ParagraphKind::Ordered { indent, start } => Some((ListKind::Ordered, indent, start)),
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
    }
    while let Some(list) = lists.pop() {
        close_list(&mut output, list.kind);
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
    for span in paragraph.spans() {
        let style = span.style;
        if style.bold {
            output.push_str("<strong>");
        }
        if style.italic {
            output.push_str("<em>");
        }
        if style.underline {
            output.push_str("<u>");
        }
        if style.strikethrough {
            output.push_str("<s>");
        }
        if style.code {
            output.push_str("<code>");
        }
        if let Some(Color([red, green, blue, alpha])) = style.foreground {
            write!(
                output,
                "<span style=\"color:#{red:02x}{green:02x}{blue:02x}{alpha:02x}\">"
            )
            .unwrap();
        }
        escape(output, &paragraph.text()[span.range.clone()]);
        if style.foreground.is_some() {
            output.push_str("</span>");
        }
        if style.code {
            output.push_str("</code>");
        }
        if style.strikethrough {
            output.push_str("</s>");
        }
        if style.underline {
            output.push_str("</u>");
        }
        if style.italic {
            output.push_str("</em>");
        }
        if style.bold {
            output.push_str("</strong>");
        }
    }
}

fn escape(output: &mut String, text: &str) {
    let mut start = 0;
    for (index, byte) in text.bytes().enumerate() {
        let escaped = match byte {
            // A literal NULL is a parse error and is discarded in HTML body
            // text. Use its visible replacement, as for a NULL reference.
            0 => "\u{fffd}",
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'\"' => "&quot;",
            b'\'' => "&#39;",
            _ => continue,
        };
        output.push_str(&text[start..index]);
        output.push_str(escaped);
        start = index + 1;
    }
    output.push_str(&text[start..]);
}
