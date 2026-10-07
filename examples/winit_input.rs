//! Native input bridge without creating a window. In a windowed host, call
//! handle_window_event from ApplicationHandler::window_event and sync_ime each frame.
use textloom::{
    Editor,
    adapter::winit::{ClipboardEvent, CommandModifier, WinitAdapter},
};
use winit::{
    event::{ElementState, Ime},
    keyboard::{Key, ModifiersState},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut editor = Editor::from_text("Select and edit me");
    let mut input = WinitAdapter::new().command_modifier(CommandModifier::Control);
    input.set_focused(true, &mut editor);
    input.set_modifiers(ModifiersState::CONTROL);
    input.handle_key(
        &Key::Character("a".into()),
        ElementState::Pressed,
        None,
        &mut editor,
    )?;
    let copy = input.handle_key(
        &Key::Character("c".into()),
        ElementState::Pressed,
        None,
        &mut editor,
    )?;
    if let Some(ClipboardEvent::Copy(text)) = copy.clipboard {
        println!("Host clipboard write: {text}");
    }

    input.set_modifiers(ModifiersState::empty());
    input.handle_ime(&Ime::Enabled, &mut editor)?;
    input.handle_ime(&Ime::Preedit("日本".into(), Some((0, 6))), &mut editor)?;
    assert_eq!(editor.document().plain_text(), "Select and edit me");
    input.handle_ime(&Ime::Commit("日本語".into()), &mut editor)?;
    assert_eq!(editor.document().plain_text(), "日本語");
    assert!(editor.undo());
    println!("Undo restored: {}", editor.document().plain_text());
    Ok(())
}
