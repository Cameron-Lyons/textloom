//! The host side of the native rich clipboard: [`NativeClipboard`], the
//! [`NativeItem`] it exchanges with the helper processes, and the helpers'
//! entry point.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::env;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::process::{self, Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui;
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use textloom::adapter::egui::{RichClipboard, RichTextEditor};
use textloom::{Color, DecodeLimits, Editor, Fragment, ParagraphKind, StylePatch};

#[cfg(target_os = "linux")]
use super::linux;
use super::platform;

/// Whether this platform has a rich clipboard backend. Elsewhere the host
/// rejects `--rich-clipboard`.
pub const SUPPORTED: bool = cfg!(any(
    target_os = "linux",
    target_os = "macos",
    target_os = "windows"
));

/// The clipboard format name of the enveloped fragment on Linux and Windows.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub const MIME: &str = "application/x-textloom-fragment";
/// The decode limits for clipboard fragments.
const TLFR_LIMITS: DecodeLimits = DecodeLimits::DEFAULT;
/// The largest native representation accepted, in bytes: the largest TLFR
/// payload plus its envelope.
pub const MAX_NATIVE_BYTES: usize = TLFR_LIMITS.max_bytes + ENVELOPE_SIZE;
/// The envelope's first 8 bytes: a magic number and the envelope version.
const ENVELOPE: &[u8; 8] = b"TLCL\x01\0\0\0";
/// The envelope's size in bytes: [`ENVELOPE`], then the payload length, the
/// publication timestamp in nanoseconds, and a sequence number mixed with the
/// process ID, each a little-endian `u64`.
const ENVELOPE_SIZE: usize = 32;
/// Numbers this process's publications for the envelope's sequence number.
static PUBLICATION_SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// The copy helper's acknowledgement on stdout once it has published the item
/// (on Linux, once it is about to serve it).
const READY: &[u8; 6] = b"READY\n";
/// The deadline for one helper exchange, and separately for verifying a
/// publication by reading it back.
const OPERATION_TIMEOUT: Duration = Duration::from_millis(750);

/// The three clipboard representations of one rich copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeItem {
    /// Plain text, which matches the fragment's text up to line break
    /// normalization.
    pub plain: String,
    /// The HTML export, for other applications; it is never imported.
    pub html: String,
    /// The enveloped TLFR fragment. Native clipboards may append padding.
    pub rich: Vec<u8>,
}

/// The size of a fragment's TLFR encoding, as predicted by [`encoded_size`].
#[derive(Debug, PartialEq, Eq)]
struct EncodedSize {
    /// The length of `Fragment::to_bytes`, saturating at `usize::MAX`.
    bytes: usize,
    /// The number of spans across all paragraphs.
    spans: usize,
}

/// Predicts the size of `fragment.to_bytes()` without encoding, so that a
/// fragment over the transport limits is rejected before allocating it.
fn encoded_size(fragment: &Fragment) -> EncodedSize {
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
    EncodedSize { bytes: size, spans }
}

impl NativeItem {
    /// Builds the representations of `fragment` for a new publication, with a
    /// fresh timestamp and sequence number in its envelope.
    ///
    /// # Errors
    ///
    /// Fails if the fragment exceeds the TLFR decode limits, if its plain text
    /// contains NUL, which native text clipboards cannot represent, if a
    /// representation exceeds [`MAX_NATIVE_BYTES`], or if the system clock is
    /// before the Unix epoch.
    pub(super) fn from_fragment(fragment: &Fragment) -> Result<Self, String> {
        if fragment.paragraphs().len() > TLFR_LIMITS.max_paragraphs {
            return Err("Clipboard fragment exceeds the paragraph limit".into());
        }
        let size = encoded_size(fragment);
        if size.bytes > TLFR_LIMITS.max_bytes || size.spans > TLFR_LIMITS.max_spans {
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
        let sequence =
            PUBLICATION_SEQUENCE.fetch_add(1, Ordering::Relaxed) ^ (u64::from(process::id()) << 32);
        rich.extend_from_slice(&timestamp.to_le_bytes());
        rich.extend_from_slice(&sequence.to_le_bytes());
        rich.extend_from_slice(&payload);
        Ok(Self { plain, html, rich })
    }

    /// Returns the envelope and its payload, without any padding after them.
    ///
    /// # Errors
    ///
    /// Fails if the envelope is oversized, has the wrong magic number or
    /// version, or is truncated, or if its payload length exceeds the TLFR
    /// limit.
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

    /// Decodes the rich fragment.
    ///
    /// # Errors
    ///
    /// Fails if the envelope or its TLFR payload is invalid, or if the
    /// fragment's text differs from the plain representation after line break
    /// normalization.
    pub(super) fn fragment(&self) -> Result<Fragment, String> {
        let payload = &self.envelope()?[ENVELOPE_SIZE..];
        let fragment = Fragment::from_bytes_with_limits(payload, TLFR_LIMITS)
            .map_err(|_| "Clipboard fragment payload was rejected")?;
        if fragment.plain_text() != normalize_text(&self.plain) {
            return Err("Clipboard representations do not describe the same text".into());
        }
        Ok(fragment)
    }

    /// Whether `self`, read back from the clipboard, is the publication
    /// `expected`: the same HTML, the same envelope and payload (which
    /// identifies the publication), and the same plain text up to line break
    /// normalization.
    fn same_publication(&self, expected: &Self) -> bool {
        (self.plain == expected.plain
            || normalize_text(&self.plain) == normalize_text(&expected.plain))
            && self.html == expected.html
            && matches!((self.envelope(), expected.envelope()), (Ok(actual), Ok(expected)) if actual == expected)
    }
}

/// The HTML representation of `fragment`. On macOS it is a full document that
/// declares UTF-8, so that other applications decode it correctly.
fn native_html(fragment: &Fragment) -> String {
    let html = fragment.to_html();
    #[cfg(target_os = "macos")]
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"></head><body>{html}</body></html>"
    );
    html
}

/// Converts CRLF and lone CR line breaks to LF.
fn normalize_text(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// A rich paste captured for one Paste event.
struct CapturedPaste {
    /// The Paste event's text after capture, which identifies the event when
    /// the document handles it.
    plain: String,
    /// The fragment to insert instead of the plain text.
    fragment: Fragment,
}

/// Captures the native item for the one Paste event in `events`, and replaces
/// that event's text with the fragment's text, which equals it up to line
/// break normalization.
///
/// Returns `None`, leaving `events` unchanged, when there is no Paste event or
/// more than one, when the Paste is empty, when `capture` finds no rich item,
/// when its fragment is invalid, or when its text does not match the Paste.
/// `capture` is only called for a single nonempty Paste. A helper operation
/// failure is returned separately so ordinary plain pastes are not QA errors.
fn capture_paste(
    events: &mut [egui::Event],
    capture: impl FnOnce() -> Result<Option<NativeItem>, String>,
) -> Result<Option<CapturedPaste>, String> {
    let mut pastes = events.iter_mut().filter_map(|event| match event {
        egui::Event::Paste(text) => Some(text),
        _ => None,
    });
    let Some(plain) = pastes.next() else {
        return Ok(None);
    };
    if plain.is_empty() || pastes.next().is_some() {
        return Ok(None);
    }
    let Some(item) = capture()? else {
        return Ok(None);
    };
    let Ok(fragment) = item.fragment() else {
        return Ok(None);
    };
    let captured_plain = fragment.plain_text();
    if normalize_text(plain) != captured_plain {
        return Ok(None);
    }
    *plain = captured_plain;
    Ok(Some(CapturedPaste {
        plain: plain.clone(),
        fragment,
    }))
}

/// One slot per Paste event delivered in raw input, in order, holding its
/// captured rich paste if there was one.
///
/// Input routing can split one raw input into several passes. Every Paste
/// takes its slot when it reaches a pass, whoever handles it, so a later Paste
/// never takes an earlier one's capture.
#[derive(Default)]
struct PendingPastes(VecDeque<Option<CapturedPaste>>);

impl PendingPastes {
    /// Adds a slot for each Paste in `events`, the raw input of one frame. Only
    /// the first slot gets `captured`, as [`capture_paste`] only captures a
    /// lone Paste.
    fn record(&mut self, events: &[egui::Event], mut captured: Option<CapturedPaste>) {
        for event in events {
            if matches!(event, egui::Event::Paste(_)) {
                self.0.push_back(captured.take());
            }
        }
    }

    /// Takes the slots of the Paste events in one pass's `events`. Returns the
    /// capture of a lone Paste if its text matches, or `None`.
    fn take_pass(&mut self, events: &[egui::Event]) -> Option<CapturedPaste> {
        let mut pastes = events.iter().filter_map(|event| match event {
            egui::Event::Paste(text) => Some(text),
            _ => None,
        });
        let plain = pastes.next()?;
        let mut captured = self
            .0
            .pop_front()
            .flatten()
            .filter(|captured| captured.plain == *plain);
        for _ in pastes {
            self.0.pop_front();
            captured = None;
        }
        captured
    }
}

/// Writes `item` to a helper pipe: its plain, HTML, and rich representations,
/// each as a little-endian `u64` length followed by that many bytes.
///
/// # Errors
///
/// Fails if a representation exceeds [`MAX_NATIVE_BYTES`] or the pipe write
/// fails.
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

/// Reads an item that [`write_item`] wrote.
///
/// # Errors
///
/// Fails if the pipe ends early, if a length exceeds [`MAX_NATIVE_BYTES`]
/// (checked before allocating), or if the plain text or HTML is not UTF-8.
fn read_item(mut input: impl Read) -> Result<NativeItem, String> {
    let plain = read_field(&mut input)?;
    let html = read_field(&mut input)?;
    let rich = read_field(&mut input)?;
    let html = String::from_utf8(html).map_err(|_| "Clipboard HTML is not UTF-8")?;
    let plain = String::from_utf8(plain).map_err(|_| "Clipboard text is not UTF-8")?;
    Ok(NativeItem { plain, html, rich })
}

/// Reads one length-prefixed representation for [`read_item`].
fn read_field(input: &mut impl Read) -> Result<Vec<u8>, String> {
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
    Ok(bytes)
}

/// A helper's successful answer.
enum Reply {
    /// The copy helper acknowledged its item with [`READY`].
    Ready,
    /// The read helper's capture: `None` when the clipboard holds no rich item.
    Item(Option<NativeItem>),
}

/// Kills `child`, if it is still running, and reaps it.
fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Sends `item` to a copy helper and waits for its acknowledgement, or, with no
/// item, waits for a read helper's capture. Pipe I/O runs on a worker thread so
/// that the wait ends at `deadline`.
///
/// Returns the reply together with the helper, which may still be running.
///
/// # Errors
///
/// Fails if the helper's pipes are unavailable, the exchange fails or misses
/// `deadline`, or the reply is invalid. The helper is then stopped and reaped.
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

/// Waits until `deadline` for the reply that `worker` sends through
/// `receiver`, and joins it. On any failure, stops and reaps `child` before
/// returning the error.
fn wait_for_reply(
    child: Child,
    worker: io::Result<JoinHandle<()>>,
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

/// The host's rich clipboard, which the document widget uses through
/// [`RichClipboard`].
#[derive(Default)]
pub struct NativeClipboard {
    /// The Linux display backend of the window, `"wayland"` or `"x11"`, for
    /// the helpers to use; `None` lets them choose from the environment.
    backend: Option<&'static str>,
    /// The helper of the latest verified copy. On Linux it serves the
    /// clipboard until replaced, so it is stopped only then or on drop.
    copy_owner: Option<Child>,
    pending_pastes: PendingPastes,
    /// The rich paste for the current pass, if the document owns its input.
    staged: Option<CapturedPaste>,
    /// Failed rich copy/capture operations awaiting host status and QA reporting.
    errors: VecDeque<String>,
}

impl NativeClipboard {
    /// Creates the clipboard for the window that `context` belongs to.
    ///
    /// # Errors
    ///
    /// Fails if the native display handle is unavailable.
    pub fn try_new(context: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let display = context
            .display_handle()
            .map_err(|_| "Native display handle is unavailable")?;
        let backend = match display.as_raw() {
            RawDisplayHandle::Wayland(_) => Some("wayland"),
            RawDisplayHandle::Xlib(_) | RawDisplayHandle::Xcb(_) => Some("x11"),
            _ => None,
        };
        Ok(Self {
            backend,
            copy_owner: None,
            pending_pastes: PendingPastes::default(),
            staged: None,
            errors: VecDeque::new(),
        })
    }

    /// Prepares a helper process for `operation`, one of the flags that
    /// [`helper`] accepts, with piped stdin and stdout.
    fn command(&self, operation: &str) -> Result<Command, String> {
        let executable =
            env::current_exe().map_err(|_| "Clipboard helper executable is unavailable")?;
        let mut command = Command::new(executable);
        command.arg(operation);
        if operation == "--native-clipboard-copy" {
            // Retain the helper protocol's legacy owner argument, but never
            // lend the blocking UI thread's HWND to a helper.
            command.arg("0");
        }
        if let Some(backend) = self.backend {
            command.env("TEXTLOOM_CLIPBOARD_BACKEND", backend);
        }
        #[cfg(target_os = "linux")]
        command.env(linux::PARENT_VARIABLE, process::id().to_string());
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(command)
    }

    /// Reads the clipboard's rich item through a read helper, by `deadline`.
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

    /// Reads the clipboard's rich item within [`OPERATION_TIMEOUT`].
    fn capture(&self) -> Result<Option<NativeItem>, String> {
        self.capture_until(Instant::now() + OPERATION_TIMEOUT)
    }

    /// Publishes `item` through a copy helper, then reads the clipboard until
    /// it holds this publication. The verified helper replaces the previous
    /// clipboard owner, which is stopped.
    ///
    /// # Errors
    ///
    /// Fails if the copy helper fails or times out, or if reading back does
    /// not find this publication within [`OPERATION_TIMEOUT`].
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

    /// Captures the native item for a lone Paste in `input` if `capture_native`
    /// is true, and records a slot for every Paste. Call it from the raw input
    /// hook, before egui routes the input.
    pub fn capture_pastes(&mut self, input: &mut egui::RawInput, capture_native: bool) {
        let captured = if capture_native {
            let result = capture_paste(&mut input.events, || self.capture());
            self.paste_result(result)
        } else {
            None
        };
        // Keep a slot even for plain-only or native-field pastes. Every delivered
        // Paste advances the queue, including ones the document does not handle.
        self.pending_pastes.record(&input.events, captured);
    }

    /// Takes the slots of the Paste events in one pass's `events` and stages
    /// their capture for the document, if `document_owns_input`.
    pub fn stage_paste(&mut self, events: &[egui::Event], document_owns_input: bool) {
        self.staged = self
            .pending_pastes
            .take_pass(events)
            .filter(|_| document_owns_input);
    }

    /// Discards a staged paste that the document did not use in this pass.
    pub fn clear_staged_paste(&mut self) {
        self.staged = None;
    }

    /// Records a failed native capture while retaining the plain Paste event.
    fn paste_result(
        &mut self,
        result: Result<Option<CapturedPaste>, String>,
    ) -> Option<CapturedPaste> {
        match result {
            Ok(captured) => captured,
            Err(error) => {
                self.errors
                    .push_back(format!("{error}; pasted plain text instead"));
                None
            }
        }
    }

    /// Takes one copy/capture error and its plain-text fallback status. The
    /// host drains these once per pass and counts each in its QA observations.
    pub fn take_error(&mut self) -> Option<String> {
        self.errors.pop_front()
    }

    /// Runs one frame of a document widget for `editor` with `input`, using this
    /// clipboard, for [`Self::self_test`].
    ///
    /// # Errors
    ///
    /// Fails if the widget reports an error, falls back to egui's own copy, or
    /// a rich copy fails.
    fn fixture_frame(
        &mut self,
        context: &egui::Context,
        editor: &mut Editor,
        mut input: egui::RawInput,
    ) -> Result<(), String> {
        let id = egui::Id::unique("native-clipboard-fixture");
        let mut failed = false;
        let mut captured = capture_paste(&mut input.events, || self.capture())?;
        let output = context.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.memory_mut(|memory| memory.request_focus(id));
                self.staged = captured.take();
                let output = RichTextEditor::new(editor)
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

    /// Copies a formatted fixture through the system clipboard, checks every
    /// representation read back, then pastes it into another document and
    /// checks the result and one-step undo/redo. This overwrites the system
    /// clipboard.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first step that failed.
    pub fn self_test(&mut self) -> Result<(), String> {
        let mut editor = Editor::from_text("café 日本語👩🏽\u{200d}💻\nsecond");
        editor.select_all();
        editor
            .apply_style(StylePatch {
                bold: Some(true),
                italic: Some(true),
                foreground: Some(Some(Color([42, 100, 200, 255]))),
                background: Some(Some(Color::YELLOW)),
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
        // Repeated copies exercise owner replacement while the host thread
        // blocks in the helper exchange. A single copy missed the Windows
        // WM_DESTROYCLIPBOARD deadlock on the next publication.
        for _ in 0..7 {
            self.fixture_frame(
                &context,
                &mut editor,
                egui::RawInput {
                    events: vec![egui::Event::Copy],
                    ..Default::default()
                },
            )?;
        }
        let captured = self
            .capture()?
            .ok_or("Native rich clipboard format was unavailable")?;
        if normalize_text(&captured.plain) != fragment.plain_text()
            || captured.html != native_html(&fragment)
            || captured.fragment()? != fragment
        {
            return Err("Native clipboard rich/HTML/plain roundtrip failed".into());
        }
        let mut destination = Editor::from_text("original");
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

impl RichClipboard for NativeClipboard {
    fn copy(&mut self, fragment: &Fragment) -> bool {
        match NativeItem::from_fragment(fragment).and_then(|item| self.publish(item)) {
            Ok(()) => true,
            Err(error) => {
                self.errors
                    .push_back(format!("{error}; copied plain text instead"));
                false
            }
        }
    }

    fn paste(&mut self, plain_text: &str) -> Option<Fragment> {
        let captured = self.staged.take()?;
        (captured.plain == plain_text).then_some(captured.fragment)
    }
}

impl Drop for NativeClipboard {
    fn drop(&mut self) {
        if let Some(child) = self.copy_owner.take() {
            stop_child(child);
        }
    }
}

/// Runs a clipboard helper if `arguments`, the host's arguments without the
/// program name, request one; returns `None` otherwise.
///
/// `--native-clipboard-copy OWNER` reads a [`NativeItem`] from stdin,
/// publishes it (`OWNER` is a legacy argument, ignored), and writes
/// [`READY`]; on Linux it then serves the clipboard until another application
/// takes it over or the host stops it. `--native-clipboard-read` writes one
/// byte, 1 if the clipboard holds a rich item and 0 if not, followed by the
/// item. On Linux, both exit with the host process.
pub fn helper(arguments: &[OsString]) -> Option<Result<(), String>> {
    let operation = arguments.first()?.to_str()?;
    if operation != "--native-clipboard-copy" && operation != "--native-clipboard-read" {
        return None;
    }
    Some((|| {
        #[cfg(target_os = "linux")]
        linux::exit_with_parent()?;
        if operation == "--native-clipboard-copy" {
            let owner: usize = arguments
                .get(1)
                .and_then(|value| value.to_str())
                .and_then(|value| value.parse().ok())
                .filter(|_| arguments.len() == 2)
                .ok_or("Clipboard helper owner was invalid")?;
            let item = read_item(io::stdin().lock())?;
            item.fragment()?;
            platform::publish(&item, owner)?;
            #[cfg(not(target_os = "linux"))]
            {
                io::stdout()
                    .write_all(READY)
                    .map_err(|_| "Clipboard acknowledgement failed")?;
                io::stdout()
                    .flush()
                    .map_err(|_| "Clipboard acknowledgement failed")?;
            }
        } else {
            if arguments.len() != 1 {
                return Err("Clipboard capture helper arguments were invalid".into());
            }
            let item = platform::read()?;
            let mut output = io::stdout().lock();
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
    use std::num::NonZeroUsize;

    use textloom::{Link, Position, Selection};

    use super::*;

    fn item() -> NativeItem {
        NativeItem::from_fragment(&Fragment::from_text("café 日本語👩🏽\u{200d}💻\nsecond")).unwrap()
    }

    #[test]
    fn size_preflight_matches_the_encoding_of_every_paragraph_kind() {
        let mut editor = Editor::from_text("body\nheading\nbullet\nitem\nnext\nquote\n");
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
                .set_selection(Selection::caret(Position::new(paragraph, 0)))
                .unwrap();
            editor.set_paragraph_kind(kind).unwrap();
        }
        editor
            .set_selection(Selection::new(Position::new(0, 0), Position::new(3, 2)))
            .unwrap();
        editor
            .apply_style(StylePatch {
                foreground: Some(Some(Color([1, 2, 3, 4]))),
                ..Default::default()
            })
            .unwrap();
        editor
            .set_selection(Selection::new(Position::new(1, 2), Position::new(2, 3)))
            .unwrap();
        editor
            .set_link(Some(Link::try_new("https://example.com/é").unwrap()))
            .unwrap();
        let fragment = Fragment::from_document(editor.document());
        let spans: usize = fragment
            .paragraphs()
            .iter()
            .map(|paragraph| paragraph.spans().len())
            .sum();
        assert_eq!(
            encoded_size(&fragment),
            EncodedSize {
                bytes: fragment.to_bytes().len(),
                spans,
            }
        );
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
        let captured = capture_paste(&mut input.events, || Ok(Some(native.clone())))
            .unwrap()
            .unwrap();
        assert_eq!(captured.fragment, native.fragment().unwrap());
        assert_eq!(input.events, vec![egui::Event::Paste(native.plain.clone())]);

        for alternative in ["different clipboard", "same text, native format absent"] {
            let events = vec![egui::Event::Paste(alternative.into())];
            input.events.clone_from(&events);
            assert!(
                capture_paste(&mut input.events, || Ok(Some(native.clone())))
                    .unwrap()
                    .is_none(),
                "{alternative}"
            );
            assert_eq!(input.events, events);
        }
        input.events = vec![egui::Event::Paste(native.plain.clone())];
        let events = input.events.clone();
        let mut clipboard = NativeClipboard::default();
        let absent = capture_paste(&mut input.events, || Ok(None));
        assert!(clipboard.paste_result(absent).is_none());
        assert!(clipboard.take_error().is_none());
        let failed = capture_paste(&mut input.events, || Err("unavailable".into()));
        assert!(clipboard.paste_result(failed).is_none());
        assert_eq!(
            clipboard.take_error().as_deref(),
            Some("unavailable; pasted plain text instead")
        );
        assert!(clipboard.take_error().is_none());
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
                capture_paste(&mut input.events, || panic!("ambiguous native capture"))
                    .unwrap()
                    .is_none(),
                "{events:?}"
            );
            assert_eq!(input.events, events);
        }
    }

    #[test]
    fn deferred_paste_keeps_its_original_rich_snapshot_and_advances_field_slots() {
        struct Staged(Option<CapturedPaste>);
        impl RichClipboard for Staged {
            fn copy(&mut self, _fragment: &Fragment) -> bool {
                false
            }

            fn paste(&mut self, plain: &str) -> Option<Fragment> {
                let captured = self.0.take()?;
                assert_eq!(plain, captured.plain);
                Some(captured.fragment)
            }
        }

        for max_passes in [1, 2, 8] {
            for scenario in 0..3 {
                let case = format!("max_passes={max_passes} scenario={scenario}");
                let context = egui::Context::default();
                textloom::adapter::egui::install_input_routing(&context);
                context.enable_accesskit();
                context.options_mut(|options| {
                    options.max_passes = NonZeroUsize::new(max_passes).unwrap();
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
                let mut editor = Editor::from_text("original");
                editor.select_all();
                let native = item();
                let mut changed = Editor::from_text(&native.plain);
                changed.select_all();
                changed
                    .apply_style(StylePatch {
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
                    })
                    .unwrap();
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
                        let output = RichTextEditor::new(&mut editor)
                            .id(document_id)
                            .rich_clipboard(&mut clipboard)
                            .show(ui);
                        assert!(output.errors.is_empty(), "{case}");
                        clipboard.0 = None;
                    });
                    output.drop_without_applying_deltas();
                }
                assert_eq!(captures, if scenario == 1 { 2 } else { 1 }, "{case}");
                assert!(pending.0.is_empty(), "{case}");
                assert_eq!(field, "field", "{case}");
                assert_eq!(
                    Fragment::from_document(editor.document()),
                    native.fragment().unwrap(),
                    "{case}"
                );
                assert_ne!(
                    Fragment::from_document(editor.document()),
                    available_clipboard.fragment().unwrap(),
                    "{case}"
                );
                assert_eq!(editor.undo_len(), 1, "{case}");
                assert!(editor.undo(), "{case}");
                assert_eq!(editor.document().plain_text(), "original", "{case}");
            }
        }
    }

    #[test]
    fn canceled_plain_reply_advances_its_capture_before_an_identical_rich_paste() {
        struct Staged(Option<CapturedPaste>);
        impl RichClipboard for Staged {
            fn paste(&mut self, plain: &str) -> Option<Fragment> {
                let captured = self.0.take()?;
                assert_eq!(plain, captured.plain);
                Some(captured.fragment)
            }
        }
        for max_passes in [1, 2, 8] {
            for captured_stale in [false, true] {
                let context = egui::Context::default();
                textloom::adapter::egui::install_input_routing(&context);
                context.enable_accesskit();
                context.options_mut(|options| {
                    options.max_passes = NonZeroUsize::new(max_passes).unwrap()
                });
                let document_id = egui::Id::unique("canceled-paste-document");
                let field_id = egui::Id::unique("canceled-paste-field");
                context.memory_mut(|memory| memory.request_focus(document_id));
                let focus = |id: egui::Id| {
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Focus,
                        target_node: id.accesskit_id(),
                        target_tree: egui::accesskit::TreeId::ROOT,
                        data: None,
                    })
                };
                let mut source = Editor::from_text("same plain text");
                source.select_all();
                source
                    .apply_style(StylePatch {
                        bold: Some(true),
                        ..Default::default()
                    })
                    .unwrap();
                let fresh = source.selected_fragment();
                let mut pending = PendingPastes::default();
                let mut clipboard = Staged(None);
                let mut editor = Editor::from_text("original");
                editor.select_all();
                let mut field = String::from("native selection");
                let mut frame = |events: Vec<egui::Event>, captured| {
                    pending.record(&events, captured);
                    context
                        .run_ui(
                            egui::RawInput {
                                events,
                                ..Default::default()
                            },
                            |ui| {
                                clipboard.0 = ui.input(|input| pending.take_pass(&input.events));
                                ui.add(egui::TextEdit::singleline(&mut field).id(field_id));
                                let output = RichTextEditor::new(&mut editor)
                                    .id(document_id)
                                    .rich_clipboard(&mut clipboard)
                                    .show(ui);
                                assert!(output.errors.is_empty());
                                clipboard.0 = None;
                            },
                        )
                        .drop_without_applying_deltas();
                };
                frame(vec![], None);
                frame(vec![], None);
                frame(
                    vec![egui::Event::Key {
                        key: egui::Key::V,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::CTRL
                            | egui::Modifiers::COMMAND
                            | egui::Modifiers::SHIFT,
                    }],
                    None,
                );
                frame(
                    vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)],
                    None,
                );
                // The second raw batch captures a reply, then routing moves focus
                // before delivering it. The canceled event must still take its slot.
                frame(
                    vec![
                        focus(field_id),
                        egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::CTRL | egui::Modifiers::COMMAND,
                        },
                        egui::Event::Paste(fresh.plain_text()),
                    ],
                    captured_stale.then(|| CapturedPaste {
                        plain: fresh.plain_text(),
                        fragment: Fragment::from_text(&fresh.plain_text()),
                    }),
                );
                for _ in 0..3 {
                    frame(vec![], None);
                }
                // Same text, different formatting: using the old capture (or its
                // None placeholder) would lose bold on this ordinary rich paste.
                frame(
                    vec![focus(document_id), egui::Event::Paste(fresh.plain_text())],
                    Some(CapturedPaste {
                        plain: fresh.plain_text(),
                        fragment: fresh.clone(),
                    }),
                );
                for _ in 0..3 {
                    frame(vec![], None);
                }
                assert!(
                    pending.0.is_empty(),
                    "passes={max_passes}, stale={captured_stale}"
                );
                assert_eq!(field, "native selection");
                assert_eq!(Fragment::from_document(editor.document()), fresh);
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
        let captured = capture_paste(&mut events, || panic!("ambiguous snapshot")).unwrap();
        pending.record(&events, captured);
        assert!(pending.take_pass(&events[..1]).is_none());
        assert!(pending.take_pass(&[]).is_none());
        assert!(pending.take_pass(&events[1..]).is_none());
        let mut events = vec![egui::Event::Paste(native.plain.clone())];
        let captured = capture_paste(&mut events, || Ok(Some(native.clone()))).unwrap();
        pending.record(&events, captured);
        assert_eq!(
            pending.take_pass(&events).unwrap().fragment,
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
            assert!(bad.fragment().is_err(), "truncated to {end} bytes");
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
        use rustix::io::Errno;
        use rustix::process::{Pid, WaitOptions};

        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = Pid::from_raw(child.id() as i32).unwrap();
        let (_sender, receiver) = mpsc::channel();
        let events = vec![egui::Event::Paste("plain fallback".into())];
        let mut input = egui::RawInput {
            events: events.clone(),
            ..Default::default()
        };
        let staged = capture_paste(&mut input.events, || {
            let result = wait_for_reply(
                child,
                Err(io::ErrorKind::WouldBlock.into()),
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
        let mut clipboard = NativeClipboard::default();
        assert!(clipboard.paste_result(staged).is_none());
        assert_eq!(
            clipboard.take_error().as_deref(),
            Some("Clipboard pipe worker could not start; pasted plain text instead")
        );
        assert_eq!(input.events, events);
        assert_eq!(
            rustix::process::waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            Errno::CHILD,
            "the helper must be terminated and reaped on worker startup failure"
        );
    }
}
