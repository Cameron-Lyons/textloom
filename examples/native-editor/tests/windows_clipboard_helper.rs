//! Native Windows regression for helper ownership, repeated copy and data lifetime.
//! Run explicitly in an isolated clipboard session with `--ignored`.
//! Cross-built tests can locate the host via `TEXTLOOM_CLIPBOARD_TEST_EXE`.

#![cfg(target_os = "windows")]

use std::io::{Cursor, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, thread};

use clipboard_win::{Clipboard, formats, raw};
use textloom::{Color, Editor, StylePatch};
use windows_win::Window;
use windows_win::raw::window::Builder;

/// Runs a real helper with the host thread blocked and no Win32 message pump.
/// Even a broken helper must be killed/reaped before the test fails.
fn run_helper(arguments: &[String], input: Vec<u8>) -> Vec<u8> {
    let executable = env::var_os("TEXTLOOM_CLIPBOARD_TEST_EXE")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_textloom-native-example").into());
    let mut child = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn clipboard helper");
    let mut stdin = child.stdin.take().expect("helper stdin pipe");
    let stdout = child.stdout.take().expect("helper stdout pipe");
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = (|| {
            stdin.write_all(&input)?;
            drop(stdin);
            let mut output = Vec::new();
            stdout.take(1024 * 1024).read_to_end(&mut output)?;
            Ok::<_, std::io::Error>(output)
        })();
        let _ = sender.send(result);
    });
    // Same deadline as the host. Reading to EOF also proves that publication
    // does not depend on keeping the helper alive after acknowledgement.
    let result = receiver.recv_timeout(Duration::from_millis(750));
    if result.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().expect("reap clipboard helper");
    worker.join().expect("join clipboard pipe worker");
    let output = result
        .expect("native clipboard helper exceeded its deadline")
        .expect("clipboard pipe exchange");
    assert!(status.success(), "clipboard helper exited with {status}");
    output
}

fn write_field(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

fn read_field(input: &mut Cursor<&[u8]>) -> Vec<u8> {
    let mut length = [0; 8];
    input
        .read_exact(&mut length)
        .expect("clipboard field length");
    let length = usize::try_from(u64::from_le_bytes(length)).expect("bounded field length");
    assert!(length <= 1024 * 1024);
    let mut bytes = vec![0; length];
    input.read_exact(&mut bytes).expect("clipboard field bytes");
    bytes
}

#[test]
#[ignore = "overwrites the system clipboard; use an isolated Windows session"]
fn repeated_copy_survives_helper_exit_while_host_window_does_not_pump() {
    let host = Window::from_builder(Builder::new().class_name("STATIC").parent_message()).unwrap();
    let mut editor = Editor::from_text("café 日本語 👩🏽\u{200d}💻\nsecond");
    editor.select_all();
    for sequence in 0..7_u64 {
        editor
            .apply_style(StylePatch {
                foreground: Some(Some(Color([sequence as u8, 100, 200, 255]))),
                background: Some(Some(Color::YELLOW)),
                bold: Some(true),
                ..Default::default()
            })
            .unwrap();
        let fragment = editor.selected_fragment();
        let plain = fragment.plain_text();
        let html = fragment.to_html();
        let payload = fragment.to_bytes();
        // The production helper protocol: TLCL v1, length, timestamp, sequence.
        let mut rich = b"TLCL\x01\0\0\0".to_vec();
        rich.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        rich.extend_from_slice(&1_u64.to_le_bytes());
        rich.extend_from_slice(&sequence.to_le_bytes());
        rich.extend_from_slice(&payload);
        let mut input = Vec::new();
        for field in [plain.as_bytes(), html.as_bytes(), &rich] {
            write_field(&mut input, field);
        }
        // Supply the old host handle deliberately. The helper must ignore it;
        // otherwise the second EmptyClipboard waits on this blocked thread.
        assert_eq!(
            run_helper(
                &[
                    "--native-clipboard-copy".into(),
                    (host.inner() as usize).to_string()
                ],
                input
            ),
            b"READY\n",
            "copy {sequence}"
        );
        assert!(
            raw::get_owner().is_none(),
            "helper owner window outlived READY/exit"
        );
        let output = run_helper(&["--native-clipboard-read".into()], Vec::new());
        assert_eq!(
            output[0], 1,
            "rich data lost after helper exit on copy {sequence}"
        );
        let mut input = Cursor::new(&output[1..]);
        assert_eq!(read_field(&mut input), plain.as_bytes());
        assert_eq!(read_field(&mut input), html.as_bytes());
        let captured = read_field(&mut input);
        assert!(captured.starts_with(&rich));
        assert!(captured[rich.len()..].iter().all(|byte| *byte == 0));
        assert_eq!(input.position() as usize, output.len() - 1);
    }

    // A foreign plain-only publication is an ordinary successful capture
    // with no TLFR, not a helper error. It can also outlive its owner window.
    {
        let clipboard = Clipboard::new_for(host.inner().cast()).unwrap();
        raw::empty().unwrap();
        raw::set_without_clear(formats::CF_UNICODETEXT, &[b'p', 0, 0, 0]).unwrap();
        drop(clipboard);
    }
    host.destroy();
    assert_eq!(
        run_helper(&["--native-clipboard-read".into()], Vec::new()),
        [0]
    );
}
