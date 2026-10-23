//! Feeds keyboard shortcuts, IME composition, and paste events from winit into
//! an `Editor` through `WinitAdapter`, and services the clipboard requests the
//! adapter returns.
//!
//! The events are constructed by hand, so the example needs no window or event
//! loop: `cargo run --example winit_input --features winit`.

use std::error;

use textloom::Editor;
use textloom::adapter::winit::{ClipboardEvent, CommandModifier, WinitAdapter};
use winit::event::{ElementState, Ime};
use winit::keyboard::{Key, KeyCode, ModifiersState, PhysicalKey};

fn main() -> Result<(), Box<dyn error::Error>> {
    let mut editor = Editor::from_text("Select and edit me");
    let mut input = WinitAdapter::new().command_modifier(CommandModifier::Control);
    assert!(
        input.set_focused(true, &mut editor).changed,
        "a new adapter starts unfocused"
    );
    input.set_modifiers(ModifiersState::CONTROL);
    let select = input.handle_key(
        &Key::Character("a".into()),
        PhysicalKey::Code(KeyCode::KeyA),
        ElementState::Pressed,
        Some("a"),
        &mut editor,
    )?;
    assert!(
        select.handled && select.changed,
        "Control+A should select the whole document"
    );
    // A Cyrillic layout types "с" on the physical C key. Shortcuts use the
    // physical key when the typed character isn't ASCII, so this is Control+C.
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
    assert!(
        input.handle_ime(&Ime::Enabled, &mut editor)?.handled,
        "a focused, editable adapter should accept IME sessions"
    );
    let preedit = input.handle_ime(&Ime::Preedit("日本".into(), Some((0, 6))), &mut editor)?;
    assert!(preedit.changed, "the preedit text should be shown");
    assert_eq!(
        editor.document().plain_text(),
        "Select and edit me",
        "preedit text is presentation only and must not enter the document"
    );
    let commit = input.handle_ime(&Ime::Commit("日本語".into()), &mut editor)?;
    assert!(
        commit.changed,
        "committing the composition should edit the document"
    );
    assert_eq!(
        editor.document().plain_text(),
        "日本語",
        "the commit should replace the selected text"
    );
    assert!(editor.undo(), "the commit left a step to undo");
    println!("Undo restored: {}", editor.document().plain_text());

    assert!(
        input.set_read_only(true, &mut editor).changed,
        "the adapter was editable until now"
    );
    input.set_modifiers(ModifiersState::CONTROL);
    let cut = input.handle_key(
        &Key::Character("x".into()),
        PhysicalKey::Code(KeyCode::KeyX),
        ElementState::Pressed,
        Some("x"),
        &mut editor,
    )?;
    assert_eq!(
        cut.clipboard,
        Some(ClipboardEvent::Copy("Select and edit me".into())),
        "a read-only editor should turn Control+X into a copy"
    );
    assert!(
        !input.paste(&mut editor, "ignored")?.changed,
        "a read-only editor should ignore pastes"
    );
    assert_eq!(
        editor.document().plain_text(),
        "Select and edit me",
        "neither the read-only cut nor the paste should edit the document"
    );
    Ok(())
}
