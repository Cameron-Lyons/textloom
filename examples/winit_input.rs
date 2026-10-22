use textloom::Editor;
use textloom::adapter::winit::{ClipboardEvent, CommandModifier, WinitAdapter};
use winit::event::{ElementState, Ime};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut editor = Editor::from_text("Select and edit me");
    let mut input = WinitAdapter::new().command_modifier(CommandModifier::Control);
    assert!(input.set_focused(true, &mut editor).changed);
    input.set_modifiers(ModifiersState::CONTROL);
    let select = input.handle_key(
        &Key::Character("a".into()),
        PhysicalKey::Code(KeyCode::KeyA),
        ElementState::Pressed,
        Some("a"),
        &mut editor,
    )?;
    assert!(select.handled && select.changed);
    let copy = input.handle_key(
        &Key::Character("с".into()),
        PhysicalKey::Code(KeyCode::KeyC),
        ElementState::Pressed,
        Some("с"),
        &mut editor,
    )?;
    if let Some(ClipboardEvent::Copy(text)) = copy.clipboard {
        println!("Host clipboard write: {text}");
    }

    input.set_modifiers(ModifiersState::empty());
    assert!(input.handle_ime(&Ime::Enabled, &mut editor)?.handled);
    let preedit = input.handle_ime(&Ime::Preedit("日本".into(), Some((0, 6))), &mut editor)?;
    assert!(preedit.changed);
    assert_eq!(editor.document().plain_text(), "Select and edit me");
    let commit = input.handle_ime(&Ime::Commit("日本語".into()), &mut editor)?;
    assert!(commit.changed);
    assert_eq!(editor.document().plain_text(), "日本語");
    assert!(editor.undo());
    println!("Undo restored: {}", editor.document().plain_text());

    assert!(input.set_read_only(true, &mut editor).changed);
    input.set_modifiers(ModifiersState::CONTROL);
    let copy = input.handle_key(
        &Key::Character("x".into()),
        PhysicalKey::Code(KeyCode::KeyX),
        ElementState::Pressed,
        Some("x"),
        &mut editor,
    )?;
    assert_eq!(
        copy.clipboard,
        Some(ClipboardEvent::Copy("Select and edit me".into()))
    );
    assert!(!input.paste(&mut editor, "ignored")?.changed);
    assert_eq!(editor.document().plain_text(), "Select and edit me");
    Ok(())
}
