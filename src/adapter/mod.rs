//! GUI adapters keep platform events and rendering outside the editing core.
//!
//! Custom adapters can render [`crate::Document::paragraphs`], hit-test to a
//! [`crate::Position`], and route commands through [`crate::Editor`].
#[cfg(feature = "egui")]
pub mod egui;
#[cfg(feature = "winit")]
pub mod winit;

#[cfg(any(feature = "egui", feature = "winit"))]
fn delete_to_paragraph_start(editor: &mut crate::Editor) -> Result<(), crate::Error> {
    let selection = editor.selection();
    if !selection.is_caret() {
        return editor.delete_backward();
    }
    if selection.focus.byte == 0 {
        return Ok(());
    }
    editor.replace_range(
        crate::Position::new(selection.focus.paragraph, 0)..selection.focus,
        "",
    )
}
