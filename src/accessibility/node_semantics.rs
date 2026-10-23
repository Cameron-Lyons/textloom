//! AccessKit roles and text properties shared by the `accesskit` bridge and the
//! egui adapter's bridge, using egui's copy of AccessKit when only `egui` is
//! enabled.

use crate::{InlineStyle, ParagraphKind};
#[cfg(feature = "accesskit")]
use accesskit::{Color, ListStyle, Node, Role, TextDecoration, TextDecorationStyle};
#[cfg(not(feature = "accesskit"))]
use egui::accesskit::{Color, ListStyle, Node, Role, TextDecoration, TextDecorationStyle};

/// Sets the role of a paragraph node for its kind, with the heading level, or the
/// list level and marker style of a list item.
pub(crate) fn set_paragraph_semantics(node: &mut Node, kind: ParagraphKind) {
    node.set_role(match kind {
        ParagraphKind::Body | ParagraphKind::Quote => Role::Paragraph,
        ParagraphKind::Heading { .. } => Role::Heading,
        ParagraphKind::Bullet { .. } | ParagraphKind::Ordered { .. } => Role::ListItem,
    });
    match kind {
        ParagraphKind::Heading { level } => node.set_level(usize::from(level)),
        ParagraphKind::Bullet { indent } => {
            node.set_level(usize::from(indent) + 1);
            node.set_list_style(ListStyle::Disc);
        }
        ParagraphKind::Ordered { indent, .. } => {
            node.set_level(usize::from(indent) + 1);
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
