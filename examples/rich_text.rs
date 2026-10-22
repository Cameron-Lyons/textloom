//! Formats a document with `Editor`, round-trips it through the native
//! clipboard format, replaces text in a single undo step, and exports HTML.
//!
//! The example uses only the core crate, so it runs without optional features:
//! `cargo run --example rich_text`.

use std::error;

use textloom::{
    Document, Editor, Fragment, InlineStyle, ParagraphKind, Position, SearchOptions, Selection,
    StylePatch,
};

fn main() -> Result<(), Box<dyn error::Error>> {
    let mut editor = Editor::from_text("Textloom\nRust & <editors>\nRust apps");
    editor.set_paragraph_kind(ParagraphKind::Heading { level: 1 })?;
    editor.set_selection(Selection::new(Position::new(1, 0), editor.document().end()))?;
    editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })?;
    editor.set_selection(Selection::new(Position::new(1, 0), Position::new(1, 4)))?;
    editor.apply_style(StylePatch {
        bold: Some(true),
        ..StylePatch::default()
    })?;

    editor.select_all();
    let clipboard = editor.selected_fragment();
    let bytes = clipboard.to_bytes();
    let restored = Document::from_bytes(&bytes)?;
    assert_eq!(
        Fragment::from_document(&restored),
        clipboard,
        "the native format should round-trip text, styles, and paragraph kinds losslessly"
    );
    let mut pasted = Editor::default();
    pasted.insert_fragment(&Fragment::from_document(&restored))?;
    assert_eq!(
        Fragment::from_document(pasted.document()),
        clipboard,
        "pasting a fragment into an empty editor should reproduce it exactly"
    );

    let previous_steps = pasted.undo_len();
    let count = pasted.replace_all("Rust", "native", SearchOptions::default())?;
    assert_eq!(
        count, 2,
        "the second and third paragraphs each contain \"Rust\""
    );
    assert_eq!(
        pasted.undo_len(),
        previous_steps + 1,
        "`replace_all` should record all of its replacements as one undo step"
    );
    println!(
        "Replaced {count} matches in one undo step:\n{}",
        pasted.document().plain_text()
    );
    assert!(pasted.undo(), "the replacement left a step to undo");
    assert_eq!(
        Fragment::from_document(pasted.document()),
        clipboard,
        "one undo should revert every replacement"
    );

    let html = pasted.document().to_html();
    assert!(
        html.contains("&amp; &lt;editors&gt;"),
        "HTML export should escape markup characters in the text, got {html}"
    );
    println!("\nNative snapshot: {} bytes\nHTML: {html}", bytes.len());

    pasted.select_all();
    pasted.clear_formatting()?;
    let all_plain = pasted.document().paragraphs().iter().all(|paragraph| {
        paragraph
            .spans()
            .iter()
            .all(|span| span.style == InlineStyle::default())
    });
    assert!(
        all_plain,
        "clearing formatting over the whole document should remove every inline style"
    );
    assert!(pasted.undo(), "clearing formatting left a step to undo");
    assert_eq!(
        Fragment::from_document(pasted.document()),
        clipboard,
        "undo should restore the formatting that was cleared"
    );
    Ok(())
}
