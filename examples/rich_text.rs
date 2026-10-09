//! Headless rich clipboard, native persistence, HTML export, and grouped replacement.
//! Run with `cargo run --example rich_text`; no GUI features are required.

use textloom::{
    Document, Editor, Fragment, ParagraphKind, Position, SearchOptions, Selection, StylePatch,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut editor = Editor::from_text("Textloom\nRust & <editors>\nRust apps");
    editor.set_paragraph_kind(ParagraphKind::Heading { level: 1 })?;
    editor.set_selection(Selection::new(Position::new(1, 0), editor.document().end()))?;
    editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 })?;
    editor.set_selection(Selection::new(Position::new(1, 0), Position::new(1, 4)))?;
    editor.apply_style(StylePatch {
        bold: Some(true),
        ..Default::default()
    })?;

    // A host clipboard can carry native bytes alongside plain text and HTML.
    editor.select_all();
    let clipboard = editor.selected_fragment();
    let bytes = clipboard.to_bytes();
    let restored = Document::from_bytes(&bytes)?;
    assert_eq!(Fragment::from_document(&restored), clipboard);
    let mut pasted = Editor::default();
    pasted.insert_fragment(&Fragment::from_document(&restored))?;
    assert_eq!(Fragment::from_document(pasted.document()), clipboard);

    // A batch replacement retains formatting and creates exactly one undo step.
    let previous_steps = pasted.undo_len();
    let count = pasted.replace_all("Rust", "native", SearchOptions::default())?;
    assert_eq!(count, 2);
    assert_eq!(pasted.undo_len(), previous_steps + 1);
    println!(
        "Replaced {count} matches in one undo step:\n{}",
        pasted.document().plain_text()
    );
    assert!(pasted.undo());
    assert_eq!(Fragment::from_document(pasted.document()), clipboard);

    // Text is escaped; generated tags preserve inline and paragraph formatting.
    let html = pasted.document().to_html();
    assert!(html.contains("&amp; &lt;editors&gt;"));
    println!("\nNative snapshot: {} bytes\nHTML: {html}", bytes.len());

    // Clear inline emphasis without changing the heading or list structure.
    pasted.select_all();
    pasted.clear_formatting()?;
    assert!(pasted.document().paragraphs().iter().all(|paragraph| {
        paragraph
            .spans()
            .iter()
            .all(|span| span.style == Default::default())
    }));
    assert!(pasted.undo());
    assert_eq!(Fragment::from_document(pasted.document()), clipboard);
    Ok(())
}
