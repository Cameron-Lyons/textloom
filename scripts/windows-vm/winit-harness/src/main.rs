//! A minimal winit host for checking textloom's winit adapter on Linux (Wayland,
//! X11) and Windows. `cargo winvm-checks` drives it in the Windows VM.
//!
//! One window, one `Editor`, one `WinitAdapter`, a fake monospace layout
//! (cells of 10x24 logical px from origin 20,20) painted with softbuffer.
//! Every forwarded window event, every `sync_ime` call, and every clipboard
//! action is printed as one JSON line on stdout (and, prefixed with "TLQA ",
//! on stderr so it interleaves with WAYLAND_DEBUG output).
//!
//! Flags: --caret-end     start with the caret at the end of the document.
//!        --anchor-start  report the IME area at the composition's start instead of
//!                        the preedit cursor (a host choice some editors make).
//!        --preallow-ime  call Window::set_ime_allowed(true) once right after creating
//!                        the window, before any focus event (diagnostic for X11 XIM).

use std::{
    io::Write as _,
    num::NonZeroU32,
    process::{Command, Stdio},
    rc::Rc,
    time::Instant,
};

use serde_json::{Value, json};
use textloom::{
    Editor, Movement, Position, Selection,
    adapter::winit::{ClipboardEvent, InputOutcome, WinitAdapter},
};
#[cfg(target_os = "linux")]
use winit::platform::wayland::ActiveEventLoopExtWayland;
use winit::{
    application::ApplicationHandler,
    dpi::{LogicalPosition, LogicalSize},
    event::{Ime, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    raw_window_handle::{HasWindowHandle, RawWindowHandle},
    window::{Window, WindowId},
};

const CELL_W: f64 = 10.0;
const CELL_H: f64 = 24.0;
const ORIGIN: (f64, f64) = (20.0, 20.0);
const FIXTURE: &str = "Первая строка hello\nsecond line";

#[derive(Clone, Copy, PartialEq)]
enum Cell {
    Text,
    Selected,
    Preedit,
    PreeditSelected,
}

struct Display {
    lines: Vec<Vec<Cell>>,
    caret: (usize, usize),
    composition_start: Option<(usize, usize)>,
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// The painted model: document paragraphs, with the composition's replacement
/// range shown as preedit cells, and the caret's (line, column).
fn display(editor: &Editor) -> Display {
    let paragraphs: Vec<&str> = editor
        .document()
        .paragraphs()
        .iter()
        .map(|p| p.text())
        .collect();
    if let Some(composition) = editor.composition() {
        let range = composition.replacement.range();
        let mut lines: Vec<Vec<Cell>> = paragraphs[..range.start.paragraph]
            .iter()
            .map(|text| vec![Cell::Text; chars(text)])
            .collect();
        let prefix = chars(&paragraphs[range.start.paragraph][..range.start.byte]);
        let suffix = chars(&paragraphs[range.end.paragraph][range.end.byte..]);
        let length = chars(&composition.text);
        let (start, end) = composition
            .selection
            .clone()
            .map_or((length, length), |selection| {
                (
                    chars(&composition.text[..selection.start]),
                    chars(&composition.text[..selection.end]),
                )
            });
        let mut line = vec![Cell::Text; prefix];
        line.extend((0..length).map(|k| {
            if (start..end).contains(&k) {
                Cell::PreeditSelected
            } else {
                Cell::Preedit
            }
        }));
        line.extend(vec![Cell::Text; suffix]);
        lines.push(line);
        lines.extend(
            paragraphs[range.end.paragraph + 1..]
                .iter()
                .map(|text| vec![Cell::Text; chars(text)]),
        );
        return Display {
            lines,
            caret: (range.start.paragraph, prefix + end),
            composition_start: Some((range.start.paragraph, prefix)),
        };
    }
    let selection = editor.selection();
    let range = selection.range();
    let lines = paragraphs
        .iter()
        .enumerate()
        .map(|(index, text)| {
            text.char_indices()
                .map(|(byte, _)| {
                    let position = Position::new(index, byte);
                    if position >= range.start && position < range.end {
                        Cell::Selected
                    } else {
                        Cell::Text
                    }
                })
                .collect()
        })
        .collect();
    let focus = selection.focus;
    let caret = (
        focus.paragraph,
        chars(&paragraphs[focus.paragraph][..focus.byte]),
    );
    Display {
        lines,
        caret,
        composition_start: None,
    }
}

fn caret_rect(display: &Display) -> [f64; 4] {
    cell_rect(display.caret)
}

fn cell_rect((line, column): (usize, usize)) -> [f64; 4] {
    [
        ORIGIN.0 + column as f64 * CELL_W,
        ORIGIN.1 + line as f64 * CELL_H,
        2.0,
        CELL_H,
    ]
}

fn char_offset(editor: &Editor, position: Position) -> usize {
    let paragraphs = editor.document().paragraphs();
    paragraphs[..position.paragraph]
        .iter()
        .map(|p| chars(p.text()) + 1)
        .sum::<usize>()
        + chars(&paragraphs[position.paragraph].text()[..position.byte])
}

fn state(editor: &Editor) -> Value {
    let Selection { anchor, focus } = editor.selection();
    json!({
        "doc": editor.document().plain_text(),
        "selection": [char_offset(editor, anchor), char_offset(editor, focus)],
        "composition": editor.composition().map(|c| json!({
            "text": c.text,
            "selection": c.selection.as_ref().map(|r| [r.start, r.end]),
            "replacement": [char_offset(editor, c.replacement.anchor), char_offset(editor, c.replacement.focus)],
        })),
    })
}

fn outcome_json(outcome: &InputOutcome) -> Value {
    let clipboard = outcome.clipboard.as_ref().map(|event| match event {
        ClipboardEvent::Copy(text) => json!({"copy": text}),
        ClipboardEvent::Cut(text) => json!({"cut": text}),
        ClipboardEvent::PasteRequest => json!("paste_request"),
        other => json!(format!("{other:?}")),
    });
    json!({"handled": outcome.handled, "changed": outcome.changed, "clipboard": clipboard})
}

fn describe(event: &WindowEvent) -> Value {
    match event {
        WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } => json!({
            "kind": "key",
            "logical": format!("{:?}", event.logical_key),
            "physical": format!("{:?}", event.physical_key),
            "state": format!("{:?}", event.state),
            "text": event.text.as_deref(),
            "repeat": event.repeat,
            "synthetic": is_synthetic,
        }),
        WindowEvent::Ime(Ime::Enabled) => json!({"kind": "ime", "ime": "Enabled"}),
        WindowEvent::Ime(Ime::Disabled) => json!({"kind": "ime", "ime": "Disabled"}),
        WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
            json!({"kind": "ime", "ime": "Preedit", "text": text, "cursor": cursor.map(|(a, b)| [a, b])})
        }
        WindowEvent::Ime(Ime::Commit(text)) => {
            json!({"kind": "ime", "ime": "Commit", "text": text})
        }
        WindowEvent::Focused(focused) => json!({"kind": "focus", "focused": focused}),
        WindowEvent::ModifiersChanged(modifiers) => {
            json!({"kind": "modifiers", "state": format!("{:?}", modifiers.state())})
        }
        other => json!({"kind": format!("{other:?}")}),
    }
}

struct App {
    start: Instant,
    seq: u64,
    frame: u64,
    caret_end: bool,
    anchor_start: bool,
    wayland: bool,
    editor: Editor,
    adapter: WinitAdapter,
    window: Option<Rc<Window>>,
    surface: Option<softbuffer::Surface<Rc<Window>, Rc<Window>>>,
    last_area: Option<[f64; 4]>,
}

impl App {
    fn emit(&mut self, mut value: Value) {
        self.seq += 1;
        value["seq"] = json!(self.seq);
        value["ms"] = json!(self.start.elapsed().as_millis() as u64);
        let line = value.to_string();
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
        eprintln!("TLQA {line}");
    }

    fn write_clipboard(&mut self, action: &str, text: &str) {
        let (program, args): (&str, &[&str]) = if cfg!(windows) {
            // PowerShell reads the text from the environment, avoiding console code pages.
            (
                "powershell.exe",
                &[
                    "-NoProfile",
                    "-Command",
                    "Set-Clipboard -Value $env:TLQA_CLIPBOARD",
                ],
            )
        } else if self.wayland {
            ("wl-copy", &[])
        } else {
            (
                "xclip",
                &["-selection", "clipboard", "-i", "-t", "UTF8_STRING"],
            )
        };
        let spawned = Command::new(program)
            .args(args)
            .env("TLQA_CLIPBOARD", text)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let ok = match spawned {
            Ok(mut child) => {
                let written = child
                    .stdin
                    .take()
                    .map(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
                written == Some(true) && child.wait().is_ok_and(|status| status.success())
            }
            Err(_) => false,
        };
        self.emit(json!({"event": "clipboard_write", "action": action, "program": program, "text": text, "ok": ok}));
    }

    fn paste(&mut self) {
        let command: &[&str] = if cfg!(windows) {
            &[
                "powershell.exe",
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; [Console]::Out.Write((Get-Clipboard -Raw))",
            ]
        } else if self.wayland {
            &["timeout", "5", "wl-paste", "--no-newline"]
        } else {
            &[
                "timeout",
                "5",
                "xclip",
                "-selection",
                "clipboard",
                "-o",
                "-t",
                "UTF8_STRING",
            ]
        };
        let output = Command::new(command[0])
            .args(&command[1..])
            .stdin(Stdio::null())
            .output();
        let text = output
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let Some(text) = text else {
            self.emit(
                json!({"event": "paste", "clipboard_text": null, "error": "clipboard read failed"}),
            );
            return;
        };
        let result = self.adapter.paste(&mut self.editor, &text);
        let mut value =
            json!({"event": "paste", "clipboard_text": text, "state": state(&self.editor)});
        match result {
            Ok(outcome) => value["outcome"] = outcome_json(&outcome),
            Err(error) => value["error"] = json!(format!("{error:?}")),
        }
        self.emit(value);
    }

    fn redraw(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        self.frame += 1;
        let model = display(&self.editor);
        let rect = match model.composition_start {
            Some(start) if self.anchor_start => cell_rect(start),
            _ => caret_rect(&model),
        };
        let changed = self.last_area != Some(rect);
        self.adapter.sync_ime(
            &window,
            &self.editor,
            LogicalPosition::new(rect[0], rect[1]),
            LogicalSize::new(rect[2], rect[3]),
        );
        self.last_area = Some(rect);
        let value = json!({
            "event": "sync_ime", "frame": self.frame, "rect": rect, "changed": changed,
            "painted_caret": caret_rect(&model),
            "adapter_focused": self.adapter.is_focused(), "ime_enabled": self.adapter.ime_enabled(),
            "window_has_focus": window.has_focus(),
        });
        self.emit(value);
        self.paint(&window, &model);
    }

    fn paint(&mut self, window: &Window, model: &Display) {
        let size = window.inner_size();
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        let Some(surface) = self.surface.as_mut() else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        let Ok(mut buffer) = surface.buffer_mut() else {
            return;
        };
        buffer.fill(0xFFFFFF);
        let scale = window.scale_factor();
        let (w, h) = (size.width as i64, size.height as i64);
        let mut fill = |x: f64, y: f64, rw: f64, rh: f64, color: u32| {
            let (x0, y0) = ((x * scale).round() as i64, (y * scale).round() as i64);
            let (x1, y1) = (
                ((x + rw) * scale).round() as i64,
                ((y + rh) * scale).round() as i64,
            );
            for py in y0.max(0)..y1.min(h) {
                for px in x0.max(0)..x1.min(w) {
                    buffer[(py * w + px) as usize] = color;
                }
            }
        };
        for (line, cells) in model.lines.iter().enumerate() {
            for (column, cell) in cells.iter().enumerate() {
                let (x, y) = (
                    ORIGIN.0 + column as f64 * CELL_W,
                    ORIGIN.1 + line as f64 * CELL_H,
                );
                match cell {
                    Cell::Selected => fill(x, y, CELL_W, CELL_H, 0xADD8E6),
                    Cell::PreeditSelected => fill(x, y, CELL_W, CELL_H, 0xFFE0B0),
                    _ => {}
                }
                let preedit = matches!(cell, Cell::Preedit | Cell::PreeditSelected);
                fill(
                    x + 2.0,
                    y + 6.0,
                    6.0,
                    12.0,
                    if preedit { 0xD02090 } else { 0x303030 },
                );
                if preedit {
                    fill(x, y + 21.0, CELL_W, 2.0, 0xD02090);
                }
            }
        }
        let rect = caret_rect(model);
        fill(rect[0], rect[1], rect[2], rect[3], 0x000000);
        let _ = buffer.present();
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        #[cfg(target_os = "linux")]
        {
            self.wayland = event_loop.is_wayland();
        }
        let attributes = Window::default_attributes()
            .with_title("tlqa-winit")
            .with_inner_size(LogicalSize::new(800.0, 400.0));
        let window = Rc::new(event_loop.create_window(attributes).expect("create window"));
        if std::env::args().any(|arg| arg == "--preallow-ime") {
            window.set_ime_allowed(true);
        }
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        self.surface =
            Some(softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface"));
        if self.caret_end {
            self.editor
                .move_cursor(Movement::DocumentEnd, false)
                .expect("caret to end");
        }
        let focus = self.adapter.set_focused(true, &mut self.editor);
        #[allow(clippy::unnecessary_cast)] // Xlib's c_ulong is 32 bits on Windows.
        let x11_window = match window.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Xlib(handle)) => Some(handle.window as u64),
            Ok(RawWindowHandle::Xcb(handle)) => Some(handle.window.get() as u64),
            _ => None,
        };
        let value = json!({
            "event": "ready", "backend": if cfg!(windows) { "windows" } else if self.wayland { "wayland" } else { "x11" },
            "pid": std::process::id(), "x11_window": x11_window, "scale": window.scale_factor(),
            "set_focused": outcome_json(&focus), "state": state(&self.editor),
        });
        self.window = Some(window.clone());
        self.emit(value);
        window.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(size) => {
                let value = json!({"event": "resized", "physical": [size.width, size.height]});
                self.emit(value);
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { .. }
            | WindowEvent::Ime(_)
            | WindowEvent::Focused(_)
            | WindowEvent::ModifiersChanged(_) => {
                let result = self.adapter.handle_window_event(&event, &mut self.editor);
                let mut value = json!({"event": "window_event", "input": describe(&event), "state": state(&self.editor)});
                let clipboard = match &result {
                    Ok(outcome) => {
                        value["outcome"] = outcome_json(outcome);
                        outcome.clipboard.clone()
                    }
                    Err(error) => {
                        value["error"] = json!(format!("{error:?}"));
                        None
                    }
                };
                self.emit(value);
                match clipboard {
                    Some(ClipboardEvent::Copy(text)) => self.write_clipboard("copy", &text),
                    Some(ClipboardEvent::Cut(text)) => self.write_clipboard("cut", &text),
                    Some(ClipboardEvent::PasteRequest) => self.paste(),
                    _ => {}
                }
                // Repaint after every forwarded event, as an immediate-mode host would,
                // so sync_ime runs on the next frame.
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn main() {
    let caret_end = std::env::args().any(|arg| arg == "--caret-end");
    let anchor_start = std::env::args().any(|arg| arg == "--anchor-start");
    let event_loop = EventLoop::new().expect("event loop");
    let mut app = App {
        start: Instant::now(),
        seq: 0,
        frame: 0,
        caret_end,
        anchor_start,
        wayland: false,
        editor: Editor::from_text(FIXTURE),
        adapter: WinitAdapter::new(),
        window: None,
        surface: None,
        last_area: None,
    };
    event_loop.run_app(&mut app).expect("run app");
}
