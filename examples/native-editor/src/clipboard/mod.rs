use std::borrow::Cow;
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use textloom::{DecodeLimits, Fragment, ParagraphKind};

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

pub const SUPPORTED: bool = cfg!(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
));

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    use super::NativeItem;

    const UNSUPPORTED: &str = "Native rich clipboard is not supported on this platform";

    pub(super) fn publish(_item: &NativeItem, _owner: usize) -> Result<(), String> {
        Err(UNSUPPORTED.into())
    }

    pub(super) fn read() -> Result<Option<NativeItem>, String> {
        Err(UNSUPPORTED.into())
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub const MIME: &str = "application/x-textloom-fragment";
const TLFR_LIMITS: DecodeLimits = DecodeLimits::DEFAULT;
pub const MAX_NATIVE_BYTES: usize = TLFR_LIMITS.max_bytes + ENVELOPE_SIZE;
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

fn encoded_size(fragment: &Fragment) -> (usize, usize) {
    let mut size = 13usize;
    let mut spans = 0usize;
    for paragraph in fragment.paragraphs() {
        let kind = match paragraph.kind() {
            ParagraphKind::Body | ParagraphKind::Quote => 1,
            ParagraphKind::Heading { .. }
            | ParagraphKind::Bullet { .. }
            | ParagraphKind::Ordered { start: None, .. } => 2,
            _ => 6,
        };
        size = size.saturating_add(kind + 16 + paragraph.text().len());
        spans = spans.saturating_add(paragraph.spans().len());
        for span in paragraph.spans() {
            let colors = usize::from(span.style.foreground.is_some())
                + usize::from(span.style.background.is_some());
            let link = span.link.as_ref().map_or(0, |link| 8 + link.as_str().len());
            size = size.saturating_add(9 + colors * 4).saturating_add(link);
        }
    }
    (size, spans)
}

impl NativeItem {
    fn from_fragment(fragment: &Fragment) -> Result<Self, String> {
        if fragment.paragraphs().len() > TLFR_LIMITS.max_paragraphs {
            return Err("Clipboard fragment exceeds the paragraph limit".into());
        }
        let (encoded_size, spans) = encoded_size(fragment);
        if encoded_size > TLFR_LIMITS.max_bytes || spans > TLFR_LIMITS.max_spans {
            return Err("Clipboard fragment exceeds the native transport limit".into());
        }
        let plain = fragment.plain_text();
        if plain.contains('\0') {
            return Err("Native text clipboard cannot represent embedded NUL".into());
        }
        let html = native_html(fragment);
        if plain.len() > MAX_NATIVE_BYTES || html.len() > MAX_NATIVE_BYTES {
            return Err("Clipboard representation exceeds the native transport limit".into());
        }
        let payload = fragment.to_bytes();
        let mut rich = Vec::with_capacity(ENVELOPE_SIZE + payload.len());
        rich.extend_from_slice(ENVELOPE);
        rich.extend_from_slice(&(payload.len() as u64).to_le_bytes());
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
            .filter(|length| *length <= TLFR_LIMITS.max_bytes)
            .ok_or("Clipboard fragment exceeds the native transport limit")?;
        self.rich
            .get(..ENVELOPE_SIZE + length)
            .ok_or_else(|| "Clipboard fragment payload was truncated".into())
    }

    fn fragment(&self) -> Result<Fragment, String> {
        let payload = &self.envelope()?[ENVELOPE_SIZE..];
        let fragment = Fragment::from_bytes_with_limits(payload, TLFR_LIMITS)
            .map_err(|_| "Clipboard fragment payload was rejected")?;
        if fragment.plain_text() != normalize_text(&self.plain) {
            return Err("Clipboard representations do not describe the same text".into());
        }
        Ok(fragment)
    }

    fn same_publication(&self, expected: &Self) -> bool {
        (self.plain == expected.plain
            || normalize_text(&self.plain) == normalize_text(&expected.plain))
            && self.html == expected.html
            && matches!((self.envelope(), expected.envelope()), (Ok(actual), Ok(expected)) if actual == expected)
    }
}

fn native_html(fragment: &Fragment) -> String {
    let html = fragment.to_html();
    #[cfg(target_os = "macos")]
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>{html}</body></html>"
    );
    html
}

fn normalize_text(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
}

fn capture_paste(
    events: &mut [egui::Event],
    capture: impl FnOnce() -> Result<Option<NativeItem>, String>,
) -> Option<(String, Fragment)> {
    let mut pastes = events.iter_mut().filter_map(|event| match event {
        egui::Event::Paste(text) => Some(text),
        _ => None,
    });
    let plain = pastes.next()?;
    if plain.is_empty() || pastes.next().is_some() {
        return None;
    }
    let item = capture().ok()??;
    let fragment = item.fragment().ok()?;
    let captured_plain = fragment.plain_text();
    if normalize_text(plain) != captured_plain {
        return None;
    }
    *plain = captured_plain;
    Some((plain.clone(), fragment))
}

#[derive(Default)]
struct PendingPastes(VecDeque<Option<(String, Fragment)>>);

impl PendingPastes {
    fn record(&mut self, events: &[egui::Event], mut captured: Option<(String, Fragment)>) {
        for event in events {
            if matches!(event, egui::Event::Paste(_)) {
                self.0.push_back(captured.take());
            }
        }
    }

    fn take_pass(&mut self, events: &[egui::Event]) -> Option<(String, Fragment)> {
        let mut pastes = events.iter().filter_map(|event| match event {
            egui::Event::Paste(text) => Some(text),
            _ => None,
        });
        let plain = pastes.next()?;
        let mut captured = self
            .0
            .pop_front()
            .flatten()
            .filter(|(text, _)| text == plain);
        for _ in pastes {
            self.0.pop_front();
            captured = None;
        }
        captured
    }
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
    let worker = thread::Builder::new().spawn(move || {
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
    wait_for_reply(child, worker, receiver, deadline)
}

fn wait_for_reply(
    child: Child,
    worker: std::io::Result<thread::JoinHandle<()>>,
    receiver: mpsc::Receiver<Result<Reply, String>>,
    deadline: Instant,
) -> Result<(Reply, Child), String> {
    let worker = match worker {
        Ok(worker) => worker,
        Err(_) => {
            stop_child(child);
            return Err("Clipboard pipe worker could not start".into());
        }
    };
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
    pending_pastes: PendingPastes,
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
            pending_pastes: PendingPastes::default(),
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
        #[cfg(target_os = "linux")]
        command.env(linux::PARENT_VARIABLE, std::process::id().to_string());
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

    pub fn capture_pastes(&mut self, input: &mut egui::RawInput, capture_native: bool) {
        let captured = capture_native
            .then(|| capture_paste(&mut input.events, || self.capture()))
            .flatten();
        // Keep a slot even for plain-only or native-field pastes. Every delivered
        // Paste advances the queue, including ones the document does not handle.
        self.pending_pastes.record(&input.events, captured);
    }

    pub fn stage_paste(&mut self, events: &[egui::Event], document_owns_input: bool) {
        self.staged = self
            .pending_pastes
            .take_pass(events)
            .filter(|_| document_owns_input);
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
        mut input: egui::RawInput,
    ) -> Result<(), String> {
        let id = egui::Id::unique("native-clipboard-fixture");
        let mut failed = false;
        let mut captured = capture_paste(&mut input.events, || self.capture());
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.memory_mut(|memory| memory.request_focus(id));
                self.staged = captured.take();
                let output = textloom::adapter::egui::RichTextEditor::new(editor)
                    .id(id)
                    .rich_clipboard(self)
                    .show(ui);
                failed |= !output.errors.is_empty() || !output.accessibility_errors.is_empty();
            });
        });
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
        let mut editor = textloom::Editor::from_text("café 日本語👩🏽\u{200d}💻\nsecond");
        editor.select_all();
        editor
            .apply_style(textloom::StylePatch {
                bold: Some(true),
                italic: Some(true),
                foreground: Some(Some(textloom::Color([42, 100, 200, 255]))),
                background: Some(Some(textloom::Color::YELLOW)),
                ..Default::default()
            })
            .map_err(|_| "Clipboard fixture formatting failed")?;
        editor
            .set_paragraph_kind(ParagraphKind::Ordered {
                indent: 1,
                start: Some(4),
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
            || captured.html != native_html(&fragment)
            || captured.fragment()? != fragment
        {
            return Err("Native clipboard rich/HTML/plain roundtrip failed".into());
        }
        let mut destination = textloom::Editor::from_text("original");
        destination.select_all();
        let input = egui::RawInput {
            events: vec![egui::Event::Paste(captured.plain)],
            ..Default::default()
        };
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
        #[cfg(target_os = "linux")]
        linux::exit_with_parent()?;
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
        NativeItem::from_fragment(&Fragment::from_text("café 日本語👩🏽\u{200d}💻\nsecond")).unwrap()
    }

    #[test]
    fn size_preflight_matches_the_encoding_of_every_paragraph_kind() {
        let mut editor = textloom::Editor::from_text("body\nheading\nbullet\nitem\nnext\nquote\n");
        for (paragraph, kind) in [
            (1, ParagraphKind::Heading { level: 2 }),
            (5, ParagraphKind::Quote),
            (2, ParagraphKind::Bullet { indent: 1 }),
            (
                3,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: Some(7),
                },
            ),
            (
                4,
                ParagraphKind::Ordered {
                    indent: 0,
                    start: None,
                },
            ),
        ] {
            editor
                .set_selection(textloom::Selection::caret(textloom::Position::new(
                    paragraph, 0,
                )))
                .unwrap();
            editor.set_paragraph_kind(kind).unwrap();
        }
        editor
            .set_selection(textloom::Selection::new(
                textloom::Position::new(0, 0),
                textloom::Position::new(3, 2),
            ))
            .unwrap();
        editor
            .apply_style(textloom::StylePatch {
                foreground: Some(Some(textloom::Color([1, 2, 3, 4]))),
                ..Default::default()
            })
            .unwrap();
        editor
            .set_selection(textloom::Selection::new(
                textloom::Position::new(1, 2),
                textloom::Position::new(2, 3),
            ))
            .unwrap();
        editor
            .set_link(Some(textloom::Link::new("https://example.com/é").unwrap()))
            .unwrap();
        let fragment = Fragment::from_document(editor.document());
        let spans = fragment
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.spans().len())
            .sum();
        assert_eq!(encoded_size(&fragment), (fragment.to_bytes().len(), spans));
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
        let captured = capture_paste(&mut input.events, || Ok(Some(native.clone()))).unwrap();
        assert_eq!(captured.1, native.fragment().unwrap());
        assert_eq!(input.events, vec![egui::Event::Paste(native.plain.clone())]);

        for alternative in ["different clipboard", "same text, native format absent"] {
            let events = vec![egui::Event::Paste(alternative.into())];
            input.events.clone_from(&events);
            assert!(capture_paste(&mut input.events, || Ok(Some(native.clone()))).is_none());
            assert_eq!(input.events, events);
        }
        input.events = vec![egui::Event::Paste(native.plain.clone())];
        let events = input.events.clone();
        assert!(capture_paste(&mut input.events, || Ok(None)).is_none());
        assert!(capture_paste(&mut input.events, || Err("unavailable".into())).is_none());
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
            assert!(
                capture_paste(&mut input.events, || panic!("ambiguous native capture")).is_none()
            );
            assert_eq!(input.events, events);
        }
    }

    #[test]
    fn deferred_paste_keeps_its_original_rich_snapshot_and_advances_field_slots() {
        struct Staged(Option<(String, Fragment)>);
        impl textloom::adapter::egui::RichClipboard for Staged {
            fn copy(&mut self, _fragment: &Fragment) -> bool {
                false
            }

            fn paste(&mut self, plain: &str) -> Option<Fragment> {
                let (captured, fragment) = self.0.take()?;
                assert_eq!(plain, captured);
                Some(fragment)
            }
        }

        for max_passes in [1, 2, 8] {
            for scenario in 0..3 {
                let context = egui::Context::default();
                textloom::adapter::egui::install_input_routing(&context);
                context.enable_accesskit();
                context.options_mut(|options| {
                    options.max_passes = std::num::NonZeroUsize::new(max_passes).unwrap();
                });
                let field_id = egui::Id::unique("clipboard-source-field");
                let document_id = egui::Id::unique("clipboard-target-document");
                context.memory_mut(|memory| memory.request_focus(field_id));
                let focus = |id: egui::Id| {
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Focus,
                        target_node: id.accesskit_id(),
                        target_tree: egui::accesskit::TreeId::ROOT,
                        data: None,
                    })
                };
                let mut field = String::new();
                let mut editor = textloom::Editor::from_text("original");
                editor.select_all();
                let native = item();
                let mut changed = textloom::Editor::from_text(&native.plain);
                changed.select_all();
                changed
                    .apply_style(textloom::StylePatch {
                        bold: Some(true),
                        ..Default::default()
                    })
                    .unwrap();
                let changed =
                    NativeItem::from_fragment(&Fragment::from_document(changed.document()))
                        .unwrap();
                assert_eq!(native.plain, changed.plain);
                assert_ne!(native.fragment().unwrap(), changed.fragment().unwrap());
                let mut available_clipboard = native.clone();
                let mut captures = 0;
                let mut clipboard = Staged(None);
                let mut pending = PendingPastes::default();
                let mut batches = VecDeque::new();
                let paste = egui::Event::Paste(native.plain.replace('\n', "\r\n"));
                match scenario {
                    0 => batches.push_back(vec![
                        egui::Event::Text("field".into()),
                        focus(document_id),
                        paste,
                    ]),
                    1 => {
                        batches
                            .push_back(vec![focus(field_id), egui::Event::Paste("field".into())]);
                        batches.push_back(vec![focus(document_id), paste]);
                    }
                    2 => {
                        batches
                            .push_back(vec![egui::Event::Text("field".into()), focus(document_id)]);
                        batches.push_back(vec![paste]);
                    }
                    _ => unreachable!(),
                }
                for _ in 0..6 {
                    let events = batches.pop_front().unwrap_or_default();
                    if scenario == 2
                        && max_passes == 1
                        && matches!(events.first(), Some(egui::Event::Paste(_)))
                    {
                        assert!(context.memory(|memory| memory.has_focus(field_id)));
                    }
                    let mut input = egui::RawInput {
                        focused: true,
                        events,
                        ..Default::default()
                    };
                    let captured = capture_paste(&mut input.events, || {
                        captures += 1;
                        Ok(Some(available_clipboard.clone()))
                    });
                    if captured.is_some() {
                        // A different publication with identical plain text arrives
                        // before deferred input is delivered. Never reread it.
                        available_clipboard = changed.clone();
                    }
                    pending.record(&input.events, captured);
                    let output = context.run_ui(input, |ui| {
                        let captured = ui.input(|input| pending.take_pass(&input.events));
                        clipboard.0 =
                            captured.filter(|_| ui.memory(|memory| memory.has_focus(document_id)));
                        ui.add(egui::TextEdit::singleline(&mut field).id(field_id));
                        let output = textloom::adapter::egui::RichTextEditor::new(&mut editor)
                            .id(document_id)
                            .rich_clipboard(&mut clipboard)
                            .show(ui);
                        assert!(output.errors.is_empty());
                        clipboard.0 = None;
                    });
                    output.drop_without_applying_deltas();
                }
                assert_eq!(captures, if scenario == 1 { 2 } else { 1 });
                assert!(pending.0.is_empty());
                assert_eq!(field, "field");
                assert_eq!(
                    Fragment::from_document(editor.document()),
                    native.fragment().unwrap()
                );
                assert_ne!(
                    Fragment::from_document(editor.document()),
                    available_clipboard.fragment().unwrap()
                );
                assert_eq!(editor.undo_len(), 1);
                assert!(editor.undo());
                assert_eq!(editor.document().plain_text(), "original");
            }
        }
    }

    #[test]
    fn ambiguous_paste_snapshots_remain_plain_across_separate_focus_passes() {
        let native = item();
        let mut events = vec![
            egui::Event::Paste("field".into()),
            egui::Event::Paste(native.plain.clone()),
        ];
        let mut pending = PendingPastes::default();
        let captured = capture_paste(&mut events, || panic!("ambiguous snapshot"));
        pending.record(&events, captured);
        assert!(pending.take_pass(&events[..1]).is_none());
        assert!(pending.take_pass(&[]).is_none());
        assert!(pending.take_pass(&events[1..]).is_none());
        let mut events = vec![egui::Event::Paste(native.plain.clone())];
        let captured = capture_paste(&mut events, || Ok(Some(native.clone())));
        pending.record(&events, captured);
        assert_eq!(
            pending.take_pass(&events).unwrap().1,
            native.fragment().unwrap()
        );
        assert!(pending.0.is_empty());
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

    #[cfg(target_os = "linux")]
    #[test]
    fn worker_start_failure_reaps_helper_and_preserves_plain_paste() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        let (_sender, receiver) = mpsc::channel();
        let events = vec![egui::Event::Paste("plain fallback".into())];
        let mut input = egui::RawInput {
            events: events.clone(),
            ..Default::default()
        };
        let staged = capture_paste(&mut input.events, || {
            let result = wait_for_reply(
                child,
                Err(std::io::ErrorKind::WouldBlock.into()),
                receiver,
                Instant::now() + OPERATION_TIMEOUT,
            );
            match result {
                Err(error) => {
                    assert_eq!(error, "Clipboard pipe worker could not start");
                    Err(error)
                }
                Ok((_, child)) => {
                    stop_child(child);
                    panic!("a failed worker cannot return a clipboard reply");
                }
            }
        });
        assert!(staged.is_none());
        assert_eq!(input.events, events);
        assert_eq!(
            rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD,
            "the helper must be terminated and reaped on worker startup failure"
        );
    }
}
