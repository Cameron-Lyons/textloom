# Textloom

A reusable native rich-text editor for Rust. The document and editing commands work without a window, renderer, browser, or async runtime. Optional adapters provide an **egui widget**, **winit keyboard/IME input**, and **AccessKit accessibility**.

The default package has one dependency: `unicode-segmentation`. There is no unsafe code in this crate.

```toml
[dependencies]
textloom = "0.1"
# Enable only the integrations your application uses:
# textloom = { version = "0.1", features = ["egui"] }
```

```rust
use textloom::{Editor, ParagraphKind, Position, Selection, StylePatch};

let mut editor = Editor::from_text("Hello, native Rust!");
editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5))).unwrap();
editor.apply_style(StylePatch { bold: Some(true), ..Default::default() }).unwrap();
editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }).unwrap();
assert!(editor.undo()); // Undo list formatting.
assert!(editor.redo());
```

## Editing

- Bold, italic, underline, strikethrough, inline code, foreground colors, and six heading levels.
- Bulleted and numbered paragraphs with indentation and explicit starting numbers. Enter continues a list; Enter on an empty item exits it. Applying `Ordered { start, .. }` restarts numbering at the first selected item and renumbers the following items at the same indent. Structural edits update that continuation run; inline typing preserves explicit restarts. A body paragraph or a different indent ends the run.
- Directional selections, replacement, grapheme and Unicode word movement/deletion, paragraph/document navigation, and selection extension. `indent_list()` and `outdent_list()` format selected list items in one undo step; outdenting level zero returns to body text.
- Literal find/replace across paragraph breaks, optional Unicode lowercase matching and whole words, next/previous search with wrapping, and `replace_all()` as one undo step. Matches always contain complete graphemes. Case-insensitive search uses lowercase mappings, without Unicode normalization or full case folding.
- Bounded undo/redo that restores text, formatting, selection, and typing style. Adjacent typing coalesces into one undo step. `break_history_group()` starts a separate step; `HistoryLimits` controls retained entries and bytes.
- IME begin/update/commit/cancel. Preedit text is transient, replacement selections survive updates, and each commit is one undo step.
- LF paragraph breaks, with CRLF and lone CR normalized on import/insertion.

Positions are `(paragraph index, UTF-8 byte offset)`. Document selections must land at extended grapheme boundaries: combining marks, emoji sequences, and flags cannot be split. Invalid public input returns an error before mutation. IME preedit selections use native UTF-8 scalar boundaries because a composition can contain unfinished graphemes.

`Document::paragraphs()` exposes immutable shared paragraphs and normalized style spans. `plain_text()` and clipboard selection export omit visual list markers. `Paragraph` also maps grapheme/scalar indexes to UTF-8 bytes for custom GUI integrations.

## Rich clipboard and interchange

`selected_fragment()` captures text, styles, headings, and list metadata. `insert_fragment()` pastes them in one undo step, preserving surviving edge styles. Pasting at a paragraph start adopts source metadata; pasting into existing text retains the first destination paragraph kind. Explicit source list numbering survives rich paste. The GUI host owns clipboard MIME types and transport.

```rust
use textloom::{Document, Editor, SearchOptions};

let mut editor = Editor::from_text("Rust Rust");
editor.select_all();
let rich = editor.selected_fragment();
let mut destination = Editor::default();
destination.insert_fragment(&rich).unwrap();
assert_eq!(destination.replace_all("Rust", "native Rust", SearchOptions::default()).unwrap(), 2);
assert!(destination.undo()); // Both replacements undo together.

let bytes = destination.document().to_bytes();
let restored = Document::from_bytes(&bytes).unwrap();
let html = restored.to_html(); // Escaped text, semantic headings/styles, nested lists.
```

`Document` and `Fragment` support lossless `to_bytes()`/`from_bytes()` with a versioned, architecture-independent native format. Decoding validates UTF-8, grapheme-safe canonical spans, paragraph kinds, lengths, and trailing data; inputs are limited to 64 MiB and one million paragraphs/spans. HTML export escapes text and preserves whitespace, colors, formatting, headings, and nested lists with explicit numbering. Run `cargo run --example rich_text` for a complete core-only example. Embedded objects and HTML/Markdown parsing remain outside the package's scope.

## egui

Enable `features = ["egui"]` and add the widget from a native egui host:

```rust,ignore
ui.add(textloom::adapter::egui::RichTextEditor::new(&mut editor));
```

The widget draws rich paragraphs, selections, list markers, headings, and preedit text. It routes pointer and keyboard input through the same core and publishes the IME caret area through egui's platform output. Each independently stored editor should use a stable widget ID. For a scrolling editor, place the widget inside an egui `ScrollArea`.

`.read_only(true)` retains selection, copy, and accessibility while suppressing edits and IME. Ctrl/Alt+Backspace/Delete deletes Unicode words; Tab/Shift+Tab indents/outdents selected lists. Body-text Tab stays available for host focus traversal.

Run `cargo run --example egui_editor --features egui` for a display-independent integration example. A native window/renderer is supplied by your existing egui host; this crate does not select one for you. Register a bold font family for actual bold glyphs; egui's default fonts use strong color as a fallback.

## winit

Enable `features = ["winit"]`. `adapter::winit::WinitAdapter` maps logical keyboard events, modifiers, and native IME events to editor commands. The host provides text layout, pointer hit testing, clipboard access, and painting. The adapter returns whether an event was handled, whether presentation changed, and any clipboard request.

Call `set_focused` when the editor widget gains or loses focus, pass window events to `handle_window_event`, and call `sync_ime` with the caret's logical window position and size. Deliver clipboard text through `paste`. Avoid routing the same events through both egui and winit adapters. Run `cargo run --example winit_input --features winit` for a window-independent example of selection, clipboard requests, and IME replacement.

Opt into `WinitAdapter::new().rich_clipboard(true)` for `CopyRich`/`CutRich` fragment payloads, then deliver native rich clipboard content through `paste_fragment()`. Plain clipboard events remain the default. Word deletion and list Tab/Shift+Tab shortcuts use the same core commands as egui.

## Accessibility

`accessibility::AccessibilitySnapshot` exposes borrowed paragraph semantics and directional selection without an extra dependency. The optional `accesskit` feature provides a text input tree with styled runs, grapheme boundaries, selection, and validated accessibility actions. Connect its tree updates and action requests to your host's platform AccessKit adapter.

The host owns accessibility activation, focus, and layout geometry. Both adapters use AccessKit 0.24, matching egui's integration and avoiding a duplicate dependency version. Use a matching platform adapter for the direct tree. Text movement is logical; a renderer must provide visual movement across wrapped lines or bidirectional text. AccessKit integrations report an explicit error for graphemes larger than its upstream 255-byte character-length representation.

## Performance and validation

Text and formatting are shared between immutable paragraphs. Editing rebuilds only the affected paragraphs; undo keeps localized before/after deltas instead of copying the full document. Formatting reuses paragraph text. Single-paragraph typing merges history entries without retaining every intermediate string.

Unicode grapheme, scalar, and word indexes are built independently on first use and shared across formatting/history snapshots. Repeated navigation uses binary searches instead of rescanning long Unicode paragraphs; ASCII grapheme/scalar offsets need no indexes. `history_bytes()` estimates retained text and formatting, excluding shared navigation caches and allocator overhead.

A text edit costs time proportional to the affected paragraph bytes and runs. Inserting/removing paragraphs also shifts later paragraph references in the document's vector. Numbered-list edits may update the following continuation run. This is intended for application text fields and documents with reasonable paragraph sizes; it is not a rope for enormous single-line buffers. Explicit search temporarily flattens the document; replace-all builds each affected paragraph once. The egui widget retains galleys and layout geometry, invalidates them on content/font/DPI/appearance changes, and paints visible paragraphs. Idle cache validation takes constant time; editing still updates paragraph geometry across the document.

```sh
cargo test --locked --no-default-features
cargo test --locked --all-features
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo doc --locked --all-features --no-deps
cargo bench --bench editing
cargo bench --features egui --bench rendering
cargo package --locked --all-features
```

The dependency-free benchmark harnesses measure local insertion/undo, long Unicode navigation, batched replacement, and headless egui idle/edit frames. See [BENCHMARKS.md](BENCHMARKS.md) for measured results and workload details. Tests cover Unicode editing, rich interchange and malformed files, search/replacement, selection direction, style preservation, history limits, list continuation/indentation, composition lifecycle, native event routing, read-only mode, and accessibility action conversion. Native candidate windows and screen-reader behavior still need integration testing in each host/platform.

## CI

[GitHub Actions](https://github.com/Cameron-Lyons/textloom/actions/workflows/ci.yml) runs on pushes to `main`, pull requests, and manual dispatch:

- Ubuntu stable checks formatting, all-feature tests and doctests, Clippy for all targets, documentation with warnings denied, release benchmark smoke tests, the headless examples, and all-feature Cargo packaging.
- Rust 1.95 tests the core and compiles each optional feature independently, then the combined package.
- macOS and Windows compile all targets and run the library's native adapter tests.

Dependencies are locked, actions are pinned to commit SHAs, jobs have a 15-minute limit, and superseded runs are canceled. The workflow only reads repository contents and saves dependency caches on `main`. CI runs release benchmarks as smoke tests and records their output; timing thresholds are kept out of shared runners.

Rust 1.95 or newer. MIT licensed; see [LICENSE](LICENSE).
