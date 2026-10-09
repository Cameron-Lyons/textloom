# Textloom

A reusable native rich-text editor for Rust. The document and editing commands work without a window, renderer, browser, or async runtime. Optional adapters provide an **egui widget**, **winit keyboard/IME input**, and **AccessKit accessibility**.

The default package has one dependency: `unicode-segmentation`. There is no unsafe code in this crate.

```toml
[dependencies]
textloom = "1.0"
# Enable only the integrations your application uses:
# textloom = { version = "1.0", features = ["egui"] }
```

```rust
use textloom::{Editor, ParagraphKind, Position, Selection, StylePatch};

let mut editor = Editor::from_text("Hello, native Rust!");
editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5))).unwrap();
editor.apply_style(StylePatch { bold: Some(true), ..Default::default() }).unwrap();
assert_eq!(editor.selection_style().bold, Some(true));
editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }).unwrap();
assert!(editor.undo()); // Undo list formatting.
assert!(editor.redo());
```

## Editing

- Bold, italic, underline, strikethrough, inline code, foreground colors, and six heading levels.
- `clear_formatting()` resets selected inline styles and typing style in one undo step, preserving headings and lists. At a caret it resets only future typing.
- `selection_style()` reports uniform or mixed attributes independently for formatting toolbars, including the pending typing style at a caret.
- Bulleted and numbered paragraphs with indentation and explicit starting numbers. Enter continues a list; Enter on an empty item exits it. Applying `Ordered { start, .. }` restarts numbering at the first selected item and renumbers the following items at the same indent. Structural edits update that continuation run; inline typing preserves explicit restarts. A body paragraph or a different indent ends the run.
- Directional selections, replacement, grapheme and Unicode word movement/deletion, paragraph/document navigation, and selection extension. `indent_list()` and `outdent_list()` format selected list items in one undo step; outdenting level zero returns to body text.
- Literal find/replace across paragraph breaks, optional Unicode lowercase matching and whole words, next/previous search with wrapping, and `replace_all()` as one undo step. Matches always contain complete graphemes. Case-insensitive search uses lowercase mappings, without Unicode normalization or full case folding.
- Bounded undo/redo that restores text, formatting, selection, and typing style. Adjacent typing coalesces into one undo step. `break_history_group()` starts a separate step; `HistoryLimits` controls retained entries and bytes.
- IME begin/update/commit/cancel. Preedit text is transient, replacement selections survive updates, and each commit is one undo step.
- LF paragraph breaks, with CRLF and lone CR normalized on import/insertion.

Positions are `(paragraph index, UTF-8 byte offset)`. Document selections must land at extended grapheme boundaries: combining marks, emoji sequences, and flags cannot be split. Invalid public input returns an error before mutation. IME preedit selections use native UTF-8 scalar boundaries because a composition can contain unfinished graphemes.

`Document::paragraphs()` exposes immutable shared paragraphs and normalized style spans. `plain_text()` and clipboard selection export omit visual list markers. `Paragraph` also maps grapheme/scalar indexes to UTF-8 bytes for custom GUI integrations.

`SelectionStyle` uses `Some(true)` for uniformly enabled emphasis, `Some(false)` for uniformly disabled emphasis, and `None` for mixed values. Its foreground field uses `None` for mixed colors, `Some(None)` for the renderer's default throughout, and `Some(Some(color))` for one explicit color throughout. The summary ignores paragraph separators and empty paragraphs; a selection containing only separators uses the pending typing style. Reading it does not allocate or change selection, history, or IME state.

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

`Document` and `Fragment` support lossless `to_bytes()`/`from_bytes()` with a versioned, architecture-independent native format. Decoding validates UTF-8, grapheme-safe canonical spans, paragraph kinds, lengths, and trailing data; inputs are limited to 64 MiB and one million paragraphs/spans. In-memory documents may exceed these limits, so an oversized encoded snapshot will be rejected on import. HTML export escapes text and preserves whitespace, colors, formatting, headings, and nested lists with explicit numbering; NUL becomes U+FFFD because HTML cannot represent NUL text. Run `cargo run --example rich_text` for a complete core-only example. Embedded objects and HTML/Markdown parsing remain outside the package's scope.

## egui

Enable `features = ["egui"]` and add the widget from a native egui host:

```rust,ignore
ui.add(textloom::adapter::egui::RichTextEditor::new(&mut editor));
```

The widget draws rich paragraphs, selections, list markers, headings, and preedit text. It routes pointer and keyboard input through the same core and publishes the IME caret area through egui's platform output. It checks layout character counts and retries inconsistent font shaping to keep rendered text and cursor positions aligned. Each independently stored editor should use a stable widget ID. For a scrolling editor, place the widget inside an egui `ScrollArea`.

Pointer clicks retain the chosen visual row at wrap boundaries, and selection highlighting extends a hard paragraph break only after the final wrapped row. Focused caret and preedit changes scroll into view even when the widget starts outside the viewport. Keyboard and accessibility actions follow their input batch order; accessibility actions based on content changed by an earlier event are rejected as stale.

Both input adapters support Ctrl/Command+B, I, and U for bold, italic, and underline. A mixed selection becomes uniformly emphasized; a uniformly emphasized selection becomes plain for that attribute. Ctrl/Command+0 returns selected paragraphs to body text, and Ctrl/Command+1–6 applies headings. Command+Backspace deletes to the paragraph start, or deletes the selected text, in one undo step. Losing widget or window focus ends the current typing undo group.

`.read_only(true)` retains selection, copy, and accessibility while suppressing edits and IME. Ctrl/Alt+Backspace/Delete deletes Unicode words; Tab/Shift+Tab indents/outdents selected lists. Body-text Tab stays available for host focus traversal.

Attach `.rich_clipboard(&mut host_clipboard)` to use an implementation of
`adapter::egui::RichClipboard`. Its `copy(&Fragment)` callback can publish TLFR,
HTML, and plain text using the fragment's export methods. Return `true` after
publishing a plain-text alternative alongside the rich formats: this suppresses
egui's plain copy command and retires older queued text/image copies that would
overwrite the newer clipboard item. Return `false` for
egui's plain fallback. `paste(&str)` receives the paste event's plain alternative;
return a decoded `Fragment` for one undoable rich replacement, or `None` to paste
that plain text. Rich data must belong to that paste event; capture native rich
and plain representations together when producing the event. Equal text alone
cannot identify clipboard items with different formatting. Native access and
transport errors remain the host's responsibility.

The callbacks follow input order and the widget's focus, disabled, read-only,
and IME rules. Read-only cut copies without deleting; empty paste events preserve
selection and history. A nonempty preedit blocks paste and cut, while an empty
preedit is canceled before a nonempty paste. Plain clipboard behavior remains
the default and does not capture rich fragments. Egui hosts may omit a paste
event when native plain text is unavailable, so rich writers also need that fallback.

Run `cargo run --example egui_editor --features egui` for a display-independent integration example. A native window/renderer is supplied by your existing egui host; this crate does not select one for you. Register a bold font family for actual bold glyphs; egui's default fonts use strong color as a fallback.

For a complete native window, run the repository's standalone eframe host:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

The host demonstrates mixed selection state, formatting, lists, find/replace,
snapshots, and read-only/disabled modes with a system plain-text clipboard and
native IME caret area. Native rich MIME transport still requires a host backend;
the example does not implement one. Its separate Cargo workspace and lockfile
remain outside the published library archive. `--read-only` starts a selectable
viewer; `--disabled` disables document focus and interaction. The toolbar can
toggle both modes, and document/search fields have accessibility labels.
`--font PATH` and `--bold-font PATH` register application fonts; `--smoke-test`
requests a graceful close after 20 native frames. Pass these arguments after
`--` in the command above.

`--qa-report PATH` writes a JSON report on graceful exit, protecting existing
files from overwrite. It records platform, scale, event/error counts, observed
editor transitions, final-state metrics, and an in-memory TLFR round trip without
document, clipboard, search, or preedit text. It always records manual signoff as
unrecorded. Reports and smoke runs provide observations; manual native checks
still need separate steps and outcomes. See the [native example guide](https://github.com/Cameron-Lyons/textloom/blob/main/examples/native-editor/README.md)
for report revision labeling, font examples, and host checks.

## winit

Enable `features = ["winit"]`. `adapter::winit::WinitAdapter` maps logical keyboard events, modifiers, and native IME events to editor commands. The host provides text layout, pointer hit testing, clipboard access, and painting. The adapter returns whether an event was handled, whether presentation changed, and any clipboard request.

Call `set_focused` when the editor widget gains or loses focus, pass window events to `handle_window_event`, and call `sync_ime` with the caret's logical window position and size. Deliver clipboard text through `paste`. Avoid routing the same events through both egui and winit adapters. Run `cargo run --example winit_input --features winit` for a window-independent example of selection, clipboard requests, and IME replacement.

Opt into `WinitAdapter::new().rich_clipboard(true)` for `CopyRich`/`CutRich` fragment payloads, then deliver native rich clipboard content through `paste_fragment()`. Plain clipboard events remain the default. Word deletion and list Tab/Shift+Tab shortcuts use the same core commands as egui.

Call `set_read_only(true, &mut editor)` for a selectable viewer: navigation and copy remain available, cut copies, and keyboard edits, paste, and IME are suppressed. Enabling read-only mode cancels active preedit; call `sync_ime` to update the native window.

## Accessibility

`accessibility::AccessibilitySnapshot` exposes borrowed paragraph semantics and directional selection without an extra dependency. The optional `accesskit` feature provides a text input tree with styled runs, grapheme boundaries, selection, and validated accessibility actions. Connect its tree updates and action requests to your host's platform AccessKit adapter.

The host owns accessibility activation, focus, and layout geometry. Both adapters use AccessKit 0.24, matching egui's integration and avoiding a duplicate dependency version. Use a matching platform adapter for the direct tree. Text movement is logical; a renderer must provide visual movement across wrapped lines or bidirectional text. AccessKit integrations report an explicit error for graphemes larger than its upstream 255-byte character-length representation.

The direct `AccessKitAdapter` supports selection replacement and full-value replacement as one undo step. Its `set_read_only(true, &mut editor)` advertises read-only capabilities and suppresses replacement actions while retaining focus and selection. Publish `update()` after changing the mode, and keep it aligned with your input adapter.

## Performance and validation

Text and formatting are shared between immutable paragraphs. Editing rebuilds only the affected paragraphs; undo keeps localized before/after deltas instead of copying the full document. Formatting reuses paragraph text. Single-paragraph typing merges history entries without retaining every intermediate string. Each history entry holds its delta directly, and disabled history skips retention work. CR/CRLF normalization copies unchanged text in one pass.

Grapheme, scalar, and word indexes are built independently on first use and shared across formatting/history snapshots. Repeated navigation uses binary searches instead of rescanning long paragraphs; ASCII grapheme/scalar offsets need no indexes, and ASCII word indexes are allocated only when used. `history_bytes()` estimates retained text and formatting, excluding shared navigation caches and allocator overhead.

A text edit costs time proportional to the affected paragraph bytes and runs. Inserting/removing paragraphs also shifts later paragraph references in the document's vector. Numbered-list edits may update the following continuation run. This is intended for application text fields and documents with reasonable paragraph sizes; it is not a rope for enormous single-line buffers.

Queries without paragraph breaks search paragraphs independently: case-sensitive searches borrow text, and lowercase matching and whole-word indexes use temporary text/index memory bounded by the current paragraph. Word indexes and lowercase coordinate maps are prepared only when a literal candidate needs validation. Find-next/previous start at the selection boundary paragraph, visit paragraphs in the requested direction, and wrap through a bounded second interval. Each visited paragraph keeps ordinary forward nonoverlapping matching and complete lowercase context. A nearby match can be found without searching unrelated earlier paragraphs; missing queries may still visit the whole document. Cross-paragraph queries temporarily flatten the document and preserve its complete forward match stream. Literal searchers are reused across matches within each searched text; replace-all builds each affected paragraph once.

The egui widget retains galleys and layout geometry, invalidates them on content/font/DPI/appearance changes, and paints visible paragraphs. Idle cache validation takes constant time; editing still updates paragraph geometry across the document. Accessibility uses shared content tokens to validate text snapshots and an index to locate text runs, while reusing paragraph indexes and galley geometry. Direct AccessKit selection/label changes reuse paragraph nodes; unchanged trees return an empty update in constant time.

```sh
cargo test --locked --no-default-features
cargo test --locked --all-features
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo doc --locked --all-features --no-deps
cargo bench --bench editing
cargo bench --features egui --bench rendering
cargo package --locked --all-features
```

The dependency-free benchmark harnesses measure local insertion/undo, long Unicode and ASCII navigation, literal search, batched replacement, and headless egui idle/edit frames. See [BENCHMARKS.md](https://github.com/Cameron-Lyons/textloom/blob/main/BENCHMARKS.md) for measured results and workload details. Tests cover Unicode editing, rich interchange and malformed files, search/replacement, selection direction, style preservation, history limits, list continuation/indentation, composition lifecycle, native event routing, read-only mode, and accessibility action conversion. Native candidate windows and screen-reader behavior still need integration testing in each host/platform.

## CI

[GitHub Actions](https://github.com/Cameron-Lyons/textloom/actions/workflows/ci.yml) runs on pushes to `main`, pull requests, and manual dispatch:

- Ubuntu stable runs `scripts/check-release.sh`: formatting, tests and doctests for all eight feature combinations, Clippy for all targets, complete API documentation with warnings denied, release benchmark smoke tests, the headless examples, explicit package inventory verification, and tests and all-target compilation from the extracted Cargo package.
- Rust 1.95 tests every feature combination, compiles all targets plus the standalone native host, and runs its CLI/report tests.
- macOS and Windows compile all targets and the native host, then run all-feature tests, doctests, and host CLI/report tests.

The Ubuntu release checks also test the native host, check its formatting and
Clippy warnings, and verify its dependency on the packaged library. The candidate
workflow runs required renderer smoke checks in editable, read-only, and disabled
modes on Linux X11 with Xvfb/Mesa, macOS, and Windows with signed MSYS2 Mesa
packages and application-local DLLs. Validation evidence is recorded in
`RELEASING.md`. Renderer smoke checks open native windows
and verify initialization and frame completion. IME, clipboard, physical keyboard,
and screen-reader interaction retain their manual release gates on all three
platforms.

Renderer jobs validate source-labelled, content-free JSON observations for
graceful completion, zero errors, snapshot integrity, and the requested mode.
Validated reports and smoke logs are available as CI artifacts.

Dependencies are locked, actions are pinned to commit SHAs, jobs have a 15-minute limit, and superseded runs are canceled. The workflow only reads repository contents and saves dependency caches on `main`. CI runs release benchmarks as smoke tests and records their output; timing thresholds are kept out of shared runners.

## Stable release policy

Textloom 1.x follows semantic versioning for its documented public API and feature names. Error enums are non-exhaustive; keep a fallback arm when matching them. Public adapter types use egui 0.36, winit 0.30, and AccessKit 0.24. Breaking upgrades to those exposed types require a Textloom major release. Exact error messages and benchmark timings may change.

TLFR v1 bytes and their meanings remain stable independently of crate version. Unicode segmentation is pinned to version 1.13.3 (Unicode 17.0) because grapheme rules determine valid persisted style boundaries. Minimum Rust version increases, if needed, are announced in minor or major releases.

Run `./scripts/check-release.sh --allow-dirty --tag v1.0.0` to check a pending release locally. A matching version tag runs the complete CI matrix and uploads the verified package plus a SHA-256 checksum. See [CHANGELOG.md](https://github.com/Cameron-Lyons/textloom/blob/main/CHANGELOG.md) for release notes and [RELEASING.md](https://github.com/Cameron-Lyons/textloom/blob/main/RELEASING.md) for compatibility rules, native-host checks, and publication steps.

Rust 1.95 or newer. MIT licensed; see [LICENSE](https://github.com/Cameron-Lyons/textloom/blob/main/LICENSE).
