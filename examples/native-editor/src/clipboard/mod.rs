//! Native clipboard transport belongs to this host, not the library.
//!
//! Native initialization and transfers run in killable children. Every pipe
//! worker is joined after its child is reaped. One retained owner is replaced
//! only after a transient publisher's new offer has been verified.

use std::{
    ffi::OsString,
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use textloom::{Fragment, ParagraphKind};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub const MIME: &str = "application/x-textloom-fragment";
pub const MAX_NATIVE_BYTES: usize = 64 * 1024 * 1024 + 32;
const MAX_TLFR_BYTES: usize = 64 * 1024 * 1024;
const ENVELOPE: &[u8; 8] = b"TLCL\x01\0\0\0";
const ENVELOPE_SIZE: usize = 32;
static PUBLICATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const READY: &[u8; 6] = b"READY\n";
const OPERATION_TIMEOUT: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeItem {
    pub plain: String,
    pub html: String,
    pub rich: Vec<u8>,
}

impl NativeItem {
    fn from_fragment(fragment: &Fragment) -> Result<Self, String> {
        // Preflight the documented TLFR size before allocating its output.
        let mut encoded_size = 13usize;
        let mut spans = 0usize;
        if fragment.paragraphs().len() > 1_000_000 {
            return Err("Clipboard fragment exceeds the paragraph limit".into());
        }
        for paragraph in fragment.paragraphs() {
            let kind = match paragraph.kind() {
                ParagraphKind::Body => 1,
                ParagraphKind::Heading { .. } | ParagraphKind::Bullet { .. } => 2,
                ParagraphKind::Ordered { .. } => 6,
            };
            encoded_size = encoded_size.saturating_add(kind + 16 + paragraph.text().len());
            spans = spans.saturating_add(paragraph.spans().len());
            for span in paragraph.spans() {
                encoded_size = encoded_size
                    .saturating_add(9 + usize::from(span.style.foreground.is_some()) * 4);
            }
        }
        if encoded_size > MAX_TLFR_BYTES || spans > 1_000_000 {
            return Err("Clipboard fragment exceeds the native transport limit".into());
        }
        let plain = fragment.plain_text();
        if plain.contains('\0') {
            return Err("Native text clipboard cannot represent embedded NUL".into());
        }
        let html = fragment.to_html();
        if plain.len() > MAX_NATIVE_BYTES || html.len() > MAX_NATIVE_BYTES {
            return Err("Clipboard representation exceeds the native transport limit".into());
        }
        let payload = fragment.to_bytes();
        let mut rich = Vec::with_capacity(ENVELOPE_SIZE + payload.len());
        rich.extend_from_slice(ENVELOPE);
        rich.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        // Read-back must distinguish a newly published offer from an older copy
        // of identical text/styles. This is an identity token, not a secret.
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Clipboard publication clock is unavailable")?
            .as_nanos() as u64;
        let sequence = PUBLICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ^ (u64::from(std::process::id()) << 32);
        rich.extend_from_slice(&timestamp.to_le_bytes());
        rich.extend_from_slice(&sequence.to_le_bytes());
        rich.extend_from_slice(&payload);
        Ok(Self { plain, html, rich })
    }

    fn envelope(&self) -> Result<&[u8], String> {
        if self.rich.len() > MAX_NATIVE_BYTES || self.rich.get(..8) != Some(ENVELOPE.as_slice()) {
            return Err("Clipboard fragment envelope was rejected".into());
        }
        let length_bytes: [u8; 8] = self
            .rich
            .get(8..16)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("Clipboard fragment envelope was truncated")?;
        let length = usize::try_from(u64::from_le_bytes(length_bytes))
            .ok()
            .filter(|length| *length <= MAX_TLFR_BYTES)
            .ok_or("Clipboard fragment exceeds the native transport limit")?;
        self.rich
            .get(..ENVELOPE_SIZE + length)
            .ok_or_else(|| "Clipboard fragment payload was truncated".into())
    }

    fn fragment(&self) -> Result<Fragment, String> {
        let payload = &self.envelope()?[ENVELOPE_SIZE..];
        // GlobalSize on Windows may include allocation padding. The envelope
        // selects the exact TLFR bytes; its decoder still rejects trailing bytes.
        let fragment =
            Fragment::from_bytes(payload).map_err(|_| "Clipboard fragment payload was rejected")?;
        if fragment.plain_text() != normalize_text(&self.plain) {
            return Err("Clipboard representations do not describe the same text".into());
        }
        Ok(fragment)
    }

    fn same_publication(&self, expected: &Self) -> bool {
        normalize_text(&self.plain) == normalize_text(&expected.plain)
            && self.html == expected.html
            && matches!((self.envelope(), expected.envelope()), (Ok(actual), Ok(expected)) if actual == expected)
    }
}

fn normalize_text(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_owned();
    }
    text.replace("\r\n", "\n").replace('\r', "\n")
}

fn capture_paste(
    input: &mut egui::RawInput,
    capture: impl FnOnce() -> Result<Option<NativeItem>, String>,
) -> Option<(String, Fragment)> {
    let mut pastes = input.events.iter_mut().filter_map(|event| match event {
        egui::Event::Paste(text) => Some(text),
        _ => None,
    });
    let plain = pastes.next()?;
    // Callback-only FIFO matching is ambiguous when earlier events are
    // suppressed. Multiple paste events retain their original plain fallback.
    if plain.is_empty() || pastes.next().is_some() {
        return None;
    }
    let item = capture().ok()??;
    let fragment = item.fragment().ok()?;
    let captured_plain = fragment.plain_text();
    if normalize_text(plain) != captured_plain {
        return None;
    }
    // Produce this host's event from one captured native item. A copied-text
    // cache could not distinguish equal text with different formatting.
    *plain = captured_plain;
    Some((plain.clone(), fragment))
}

fn write_item(mut output: impl Write, item: &NativeItem) -> Result<(), String> {
    for bytes in [item.plain.as_bytes(), item.html.as_bytes(), &item.rich] {
        if bytes.len() > MAX_NATIVE_BYTES {
            return Err("Clipboard representation exceeds the pipe limit".into());
        }
        output
            .write_all(&(bytes.len() as u64).to_le_bytes())
            .and_then(|()| output.write_all(bytes))
            .map_err(|_| "Clipboard helper pipe write failed")?;
    }
    output
        .flush()
        .map_err(|_| "Clipboard helper pipe flush failed".into())
}

fn read_item(mut input: impl Read) -> Result<NativeItem, String> {
    let mut fields = Vec::with_capacity(3);
    for _ in 0..3 {
        let mut length = [0; 8];
        input
            .read_exact(&mut length)
            .map_err(|_| "Clipboard helper pipe was truncated")?;
        let length = usize::try_from(u64::from_le_bytes(length))
            .ok()
            .filter(|length| *length <= MAX_NATIVE_BYTES)
            .ok_or("Clipboard helper advertised an oversized representation")?;
        let mut bytes = vec![0; length];
        input
            .read_exact(&mut bytes)
            .map_err(|_| "Clipboard helper pipe was truncated")?;
        fields.push(bytes);
    }
    let rich = fields.pop().unwrap();
    let html =
        String::from_utf8(fields.pop().unwrap()).map_err(|_| "Clipboard HTML is not UTF-8")?;
    let plain =
        String::from_utf8(fields.pop().unwrap()).map_err(|_| "Clipboard text is not UTF-8")?;
    Ok(NativeItem { plain, html, rich })
}

enum Reply {
    Ready,
    Item(Option<NativeItem>),
}

fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn exchange(
    mut child: Child,
    item: Option<NativeItem>,
    deadline: Instant,
) -> Result<(Reply, Child), String> {
    let Some(mut output) = child.stdout.take() else {
        stop_child(child);
        return Err("Clipboard helper stdout was unavailable".into());
    };
    let input = child.stdin.take();
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        let result = (|| {
            if let Some(item) = item {
                let input = input.ok_or("Clipboard helper stdin was unavailable")?;
                write_item(input, &item)?;
                let mut ready = [0; 6];
                output
                    .read_exact(&mut ready)
                    .map_err(|_| "Clipboard publication failed")?;
                if &ready != READY {
                    return Err("Clipboard helper returned an invalid acknowledgement".into());
                }
                Ok(Reply::Ready)
            } else {
                drop(input);
                let mut present = [0; 1];
                output
                    .read_exact(&mut present)
                    .map_err(|_| "Clipboard capture failed")?;
                match present[0] {
                    0 => Ok(Reply::Item(None)),
                    1 => read_item(output).map(|item| Reply::Item(Some(item))),
                    _ => Err("Clipboard helper returned an invalid response".into()),
                }
            }
        })();
        let _ = sender.send(result);
    });
    let result = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    match result {
        Ok(Ok(reply)) => {
            if worker.join().is_err() {
                stop_child(child);
                return Err("Clipboard pipe worker failed".into());
            }
            Ok((reply, child))
        }
        failure => {
            // Terminating the child closes its pipes, releasing blocked reads
            // and writes before join. Backends never spawn further processes.
            stop_child(child);
            let _ = worker.join();
            match failure {
                Ok(Err(error)) => Err(error),
                _ => Err("Native clipboard operation timed out".into()),
            }
        }
    }
}

pub struct NativeClipboard {
    owner: usize,
    backend: Option<&'static str>,
    copy_owner: Option<Child>,
    staged: Option<(String, Fragment)>,
    last_error: Option<String>,
}

impl NativeClipboard {
    pub fn new(context: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let window = context
            .window_handle()
            .map_err(|_| "Native window handle is unavailable")?;
        let display = context
            .display_handle()
            .map_err(|_| "Native display handle is unavailable")?;
        let owner = match window.as_raw() {
            RawWindowHandle::Win32(handle) => handle.hwnd.get() as usize,
            _ => 0,
        };
        let backend = match display.as_raw() {
            RawDisplayHandle::Wayland(_) => Some("wayland"),
            RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => Some("x11"),
            _ => None,
        };
        Ok(Self {
            owner,
            backend,
            copy_owner: None,
            staged: None,
            last_error: None,
        })
    }

    fn command(&self, operation: &str) -> Result<Command, String> {
        let executable =
            std::env::current_exe().map_err(|_| "Clipboard helper executable is unavailable")?;
        let mut command = Command::new(executable);
        command.arg(operation);
        if operation == "--native-clipboard-copy" {
            command.arg(self.owner.to_string());
        }
        if let Some(backend) = self.backend {
            command.env("TEXTLOOM_CLIPBOARD_BACKEND", backend);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }

    fn capture_until(&self, deadline: Instant) -> Result<Option<NativeItem>, String> {
        let child = self
            .command("--native-clipboard-read")?
            .spawn()
            .map_err(|_| "Clipboard capture helper could not start")?;
        let (reply, child) = exchange(child, None, deadline)?;
        stop_child(child);
        match reply {
            Reply::Item(item) => Ok(item),
            Reply::Ready => Err("Clipboard capture returned an invalid reply".into()),
        }
    }

    fn capture(&self) -> Result<Option<NativeItem>, String> {
        self.capture_until(Instant::now() + OPERATION_TIMEOUT)
    }

    fn publish(&mut self, item: NativeItem) -> Result<(), String> {
        let deadline = Instant::now() + OPERATION_TIMEOUT;
        let child = self
            .command("--native-clipboard-copy")?
            .spawn()
            .map_err(|_| "Clipboard publication helper could not start")?;
        let (_, child) = exchange(child, Some(item.clone()), deadline)?;
        // Linux READY is provisional: read-back establishes that the offer was
        // actually published, and verifies all representations on every platform.
        let verify_deadline = Instant::now() + OPERATION_TIMEOUT;
        loop {
            if let Ok(Some(captured)) = self.capture_until(verify_deadline)
                && captured.same_publication(&item)
            {
                if let Some(previous) = self.copy_owner.take() {
                    stop_child(previous);
                }
                self.copy_owner = Some(child);
                return Ok(());
            }
            if Instant::now() >= verify_deadline {
                stop_child(child);
                return Err("Native clipboard publication could not be verified".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn stage_paste(&mut self, input: &mut egui::RawInput) {
        self.staged = capture_paste(input, || self.capture());
    }

    pub fn clear_staged_paste(&mut self) {
        self.staged = None;
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    fn fixture_frame(
        &mut self,
        context: &egui::Context,
        editor: &mut textloom::Editor,
        input: egui::RawInput,
    ) -> Result<(), String> {
        let id = egui::Id::new("native-clipboard-fixture");
        let mut failed = false;
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.memory_mut(|memory| memory.request_focus(id));
                let output = textloom::adapter::egui::RichTextEditor::new(editor)
                    .id(id)
                    .rich_clipboard(self)
                    .show(ui);
                failed |= !output.errors.is_empty() || !output.accessibility_errors.is_empty();
            });
        });
        // A successful native copy must retire the deferred plain overwrite.
        failed |= output.platform_output.commands.iter().any(|command| {
            matches!(
                command,
                egui::OutputCommand::CopyText(_) | egui::OutputCommand::CopyImage(_)
            )
        });
        output.drop_without_applying_deltas();
        self.clear_staged_paste();
        if let Some(error) = self.take_error() {
            return Err(error);
        }
        if failed {
            return Err("Native clipboard fixture widget failed".into());
        }
        Ok(())
    }

    pub fn self_test(&mut self) -> Result<(), String> {
        let mut editor = textloom::Editor::from_text("café 日本語👩🏽‍💻\nsecond");
        editor.select_all();
        editor
            .apply_style(textloom::StylePatch {
                bold: Some(true),
                italic: Some(true),
                foreground: Some(Some(textloom::Color([42, 100, 200, 255]))),
                ..Default::default()
            })
            .map_err(|_| "Clipboard fixture formatting failed")?;
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 1,
                start: 4,
            })
            .map_err(|_| "Clipboard fixture paragraph formatting failed")?;
        let fragment = editor.selected_fragment();
        let context = egui::Context::default();
        self.fixture_frame(
            &context,
            &mut editor,
            egui::RawInput {
                events: vec![egui::Event::Copy],
                ..Default::default()
            },
        )?;
        let captured = self
            .capture()?
            .ok_or("Native rich clipboard format was unavailable")?;
        if normalize_text(&captured.plain) != fragment.plain_text()
            || captured.html != fragment.to_html()
            || captured.fragment()? != fragment
        {
            return Err("Native clipboard rich/HTML/plain roundtrip failed".into());
        }
        let mut destination = textloom::Editor::from_text("original");
        destination.select_all();
        let mut input = egui::RawInput {
            events: vec![egui::Event::Paste(captured.plain)],
            ..Default::default()
        };
        self.stage_paste(&mut input);
        self.fixture_frame(&context, &mut destination, input)?;
        if destination.undo_len() != 1 {
            return Err("Native rich paste history failed".into());
        }
        if Fragment::from_document(destination.document()) != fragment
            || !destination.undo()
            || destination.document().plain_text() != "original"
            || !destination.redo()
            || Fragment::from_document(destination.document()) != fragment
        {
            return Err("Native rich clipboard undo/redo failed".into());
        }
        Ok(())
    }
}

impl textloom::adapter::egui::RichClipboard for NativeClipboard {
    fn copy(&mut self, fragment: &Fragment) -> bool {
        match NativeItem::from_fragment(fragment).and_then(|item| self.publish(item)) {
            Ok(()) => true,
            Err(error) => {
                self.last_error = Some(error);
                false
            }
        }
    }

    fn paste(&mut self, plain_text: &str) -> Option<Fragment> {
        let (plain, fragment) = self.staged.take()?;
        (plain == plain_text).then_some(fragment)
    }
}

impl Drop for NativeClipboard {
    fn drop(&mut self) {
        if let Some(child) = self.copy_owner.take() {
            stop_child(child);
        }
    }
}

pub fn helper(arguments: &[OsString]) -> Option<Result<(), String>> {
    let operation = arguments.first()?.to_str()?;
    if operation != "--native-clipboard-copy" && operation != "--native-clipboard-read" {
        return None;
    }
    Some((|| {
        if operation == "--native-clipboard-copy" {
            let owner = arguments
                .get(1)
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse().ok())
                .filter(|_| arguments.len() == 2)
                .ok_or("Clipboard helper owner was invalid")?;
            let item = read_item(std::io::stdin().lock())?;
            item.fragment()?;
            platform::publish(&item, owner)?;
            #[cfg(not(target_os = "linux"))]
            {
                std::io::stdout()
                    .write_all(READY)
                    .map_err(|_| "Clipboard acknowledgement failed")?;
                std::io::stdout()
                    .flush()
                    .map_err(|_| "Clipboard acknowledgement failed")?;
            }
        } else {
            if arguments.len() != 1 {
                return Err("Clipboard capture helper arguments were invalid".into());
            }
            let item = platform::read()?;
            let mut output = std::io::stdout().lock();
            output
                .write_all(&[u8::from(item.is_some())])
                .map_err(|_| "Clipboard response failed")?;
            if let Some(item) = item {
                write_item(&mut output, &item)?;
            }
            output.flush().map_err(|_| "Clipboard response failed")?;
        }
        Ok(())
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> NativeItem {
        NativeItem::from_fragment(&Fragment::from_text("café 日本語👩🏽‍💻\nsecond")).unwrap()
    }

    #[test]
    fn envelope_uses_exact_tlfr_slice_and_accepts_native_allocation_padding() {
        let mut item = item();
        let original = item.clone();
        let expected = item.fragment().unwrap();
        item.rich.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(item.fragment().unwrap(), expected);
        assert!(item.same_publication(&original));
        let mut invalid = item.clone();
        let length = u64::from_le_bytes(invalid.rich[8..16].try_into().unwrap()) + 1;
        invalid.rich[8..16].copy_from_slice(&length.to_le_bytes());
        assert!(
            invalid.fragment().is_err(),
            "padding inside TLFR must be rejected"
        );
    }

    #[test]
    fn publication_verification_distinguishes_repeated_copies_and_all_formats() {
        let original = item();
        let repeated = item();
        assert_eq!(repeated.fragment().unwrap(), original.fragment().unwrap());
        assert!(!repeated.same_publication(&original));
        let mut normalized = original.clone();
        normalized.plain = normalized.plain.replace('\n', "\r\n");
        assert!(normalized.same_publication(&original));
        normalized.html.push('x');
        assert!(!normalized.same_publication(&original));
        let mut changed = original.clone();
        changed.rich[ENVELOPE_SIZE] ^= 1;
        assert!(!changed.same_publication(&original));
    }

    #[test]
    fn paste_captures_one_native_item_and_preserves_plain_fallback() {
        let native = item();
        let mut input = egui::RawInput {
            events: vec![egui::Event::Paste(native.plain.replace('\n', "\r\n"))],
            ..Default::default()
        };
        let captured = capture_paste(&mut input, || Ok(Some(native.clone()))).unwrap();
        assert_eq!(captured.1, native.fragment().unwrap());
        assert_eq!(input.events, vec![egui::Event::Paste(native.plain.clone())]);

        for alternative in ["different clipboard", "same text, native format absent"] {
            let events = vec![egui::Event::Paste(alternative.into())];
            input.events.clone_from(&events);
            assert!(capture_paste(&mut input, || Ok(Some(native.clone()))).is_none());
            assert_eq!(input.events, events);
        }
        input.events = vec![egui::Event::Paste(native.plain.clone())];
        let events = input.events.clone();
        assert!(capture_paste(&mut input, || Ok(None)).is_none());
        assert!(capture_paste(&mut input, || Err("unavailable".into())).is_none());
        assert_eq!(input.events, events);
    }

    #[test]
    fn ambiguous_or_empty_pastes_do_not_read_the_native_clipboard() {
        for events in [
            vec![],
            vec![egui::Event::Paste(String::new())],
            vec![
                egui::Event::Paste("a".into()),
                egui::Event::Paste("b".into()),
            ],
        ] {
            let mut input = egui::RawInput {
                events: events.clone(),
                ..Default::default()
            };
            assert!(capture_paste(&mut input, || panic!("ambiguous native capture")).is_none());
            assert_eq!(input.events, events);
        }
    }

    #[test]
    fn envelope_rejects_truncation_lengths_and_mismatched_plain_text() {
        let item = item();
        for end in [0, 7, 15, item.rich.len() - 1] {
            let mut bad = item.clone();
            bad.rich.truncate(end);
            assert!(bad.fragment().is_err());
        }
        let mut bad = item.clone();
        bad.rich[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(bad.fragment().is_err());
        let mut bad = item;
        bad.plain.push('x');
        assert!(bad.fragment().is_err());
        assert!(NativeItem::from_fragment(&Fragment::from_text("a\0b")).is_err());
    }

    #[test]
    fn pipe_protocol_bounds_lengths_and_preserves_unicode_and_styles() {
        let original = item();
        let mut bytes = Vec::new();
        write_item(&mut bytes, &original).unwrap();
        assert_eq!(read_item(bytes.as_slice()).unwrap(), original);
        assert!(read_item(&bytes[..bytes.len() - 1]).is_err());
        assert!(read_item(u64::MAX.to_le_bytes().as_slice()).is_err());
        let bad_utf8 = [1_u64.to_le_bytes().as_slice(), &[0xff], &[0; 16]].concat();
        assert!(read_item(bad_utf8.as_slice()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn native_deadline_terminates_and_reaps_stalled_pipe_worker() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let started = Instant::now();
        let result = exchange(child, None, started + Duration::from_millis(25));
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "stalled child or pipe worker survived"
        );
    }
}
