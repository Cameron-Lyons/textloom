# Textloom

A native rich-text editing library for Rust, with optional **egui**, **winit**,
and **AccessKit** adapters. The core works without a window, renderer, browser,
or async runtime. Its only default dependency is `unicode-segmentation`, and
this crate forbids unsafe code.

**Status: 1.0.0-rc.2.** The API may still change before 1.0. Native IME,
screen-reader, and physical-device signoff remains incomplete. See
[release readiness](RELEASING.md#current-work-toward-10) and
[known native limitations](RELEASING.md#native-dependency-limitations).

[API reference](https://docs.rs/textloom) · [Integration guide](GUIDE.md) ·
[Native example](https://github.com/Cameron-Lyons/textloom/tree/main/examples/native-editor) ·
[Changelog](CHANGELOG.md)

## Install

Requires Rust 1.95 or newer.

```toml
[dependencies]
textloom = "1.0.0-rc.2"
# Enable integrations as needed:
# textloom = { version = "1.0.0-rc.2", features = ["egui"] }
```

| Feature | Integration | Host provides |
| --- | --- | --- |
| None (default) | Document, editing, search, history, interchange | Application logic |
| `egui` | `adapter::egui::RichTextEditor` (egui 0.37) | Native window, renderer, clipboard |
| `winit` | `adapter::winit::WinitAdapter` (winit 0.30) | Layout, painting, pointer input, clipboard |
| `accesskit` | `accessibility::AccessKitAdapter` (AccessKit 0.25) | Platform accessibility bridge and geometry |

Features are independent and can be combined. The `winit` feature enables its
X11, Wayland, and dynamic Wayland loading backends. The `egui` feature leaves
font selection to the host; eframe enables default fonts.

## Editing

```rust
use textloom::{Editor, ParagraphKind, Position, Selection, StylePatch};

let mut editor = Editor::from_text("Hello, native Rust!");
editor.set_selection(Selection::new(Position::new(0, 0), Position::new(0, 5))).unwrap();
editor.apply_style(StylePatch { bold: Some(true), ..Default::default() }).unwrap();
assert_eq!(editor.selection_style().bold, Some(true));
editor.set_paragraph_kind(ParagraphKind::Bullet { indent: 0 }).unwrap();
assert!(editor.undo());
assert!(editor.redo());
```

The core supports:

- Inline emphasis, font and fill colors, links, headings, quotes, and nested lists.
- Grapheme-safe selection, navigation, deletion, soft breaks, and IME composition.
- Literal find/replace, Unicode lowercase matching, and whole-word search.
- Bounded undo/redo, grouped typing, transactions, and saved-state tracking.
- Immutable shared paragraphs and independent, inexpensive editor snapshots.

Positions are `(paragraph index, UTF-8 byte offset)` and must fall on grapheme
boundaries. Invalid public input returns an error before mutation. `Editor`,
`Document`, `Paragraph`, and `Fragment` are `Send + Sync`.
See [editing semantics and examples](GUIDE.md#editing).

## Rich clipboard and interchange

`Document` and `Fragment` export plain text, HTML, and the lossless TLFR binary
format. `selected_fragment()` and `insert_fragment()` preserve rich formatting
through copy/paste; the host owns clipboard transport.

```rust
use textloom::{Document, Editor};

let editor = Editor::from_text("Save me");
let bytes = editor.document().to_bytes();
let restored = Document::from_bytes(&bytes).unwrap();
assert_eq!(restored.plain_text(), "Save me");
```

The decoder reads TLFR versions 1 and 2 and validates text, spans, and resource
limits. The encoder writes version 2. HTML/Markdown parsing and embedded objects
are outside the package's scope. See [interchange contracts](GUIDE.md#rich-clipboard-and-interchange).

## egui

Install input routing when creating the egui context, then show the widget:

```rust,ignore
// Once, in your host's egui initialization:
textloom::adapter::egui::install_input_routing(&ctx);

// Each frame, inside an egui Ui:
ui.add(textloom::adapter::egui::RichTextEditor::new(&mut editor));
```

Use a stable widget ID for each editor and an egui `ScrollArea` for scrolling.
The widget handles selection, keyboard shortcuts, IME previews, and accessibility.
`.read_only(true)` retains selection and copy. Rich clipboard support is opt-in;
link activation is reported to the host, which decides which targets to open.

The [egui integration guide](GUIDE.md#egui) covers focus and input routing,
clipboard replies, fonts, shortcuts, and cache lifetime. Run the complete native
host from a repository checkout:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

See the [native host guide](https://github.com/Cameron-Lyons/textloom/blob/main/examples/native-editor/README.md)
for rich clipboard, fonts, read-only mode, and QA options.

## winit

`WinitAdapter` translates keyboard and IME events into editor commands. Forward
window events, maintain widget focus, service clipboard requests, and call
`sync_ime` with the caret's logical window rectangle. The host supplies layout
and painting. See the [winit integration contract](GUIDE.md#winit).

## Accessibility

The core exposes borrowed accessibility snapshots and validated editing actions.
The optional `AccessKitAdapter` publishes incremental trees with styled text,
links, quotes, and selection. Connect it to your platform adapter and supply
layout geometry. See [accessibility integration](GUIDE.md#accessibility) for
activation, invalidation, and read-only behavior.

## Performance and validation

Edits rebuild affected paragraphs, history stores localized changes, and Unicode
indexes are shared and built on demand. The egui widget caches layout and paints
visible paragraphs. Editing cost still grows with the affected paragraph's size;
this is intended for text fields and documents with reasonable paragraph sizes.

See [performance details](GUIDE.md#performance-and-validation) and
[benchmark measurements](BENCHMARKS.md). To validate a checkout:

```sh
./scripts/check-release.sh --allow-dirty
./scripts/format.sh --check # Also checks nightly rustfmt style options.
```

The release check covers every feature combination, release-mode search and
editing tests, native-host and QA-tool regressions, strict Clippy, API docs,
benchmark smoke runs, examples, and the extracted Cargo package.

## CI

[CI](https://github.com/Cameron-Lyons/textloom/actions/workflows/ci.yml) checks
stable and minimum Rust versions, feature combinations, dependency policy, and
macOS/Windows builds. Separate native jobs exercise rendering and clipboard
transport. See [CI details](GUIDE.md#ci); automated checks do not replace
[native interaction signoff](https://github.com/Cameron-Lyons/textloom/blob/main/examples/native-editor/NATIVE_QA.md).

## Stable release policy

Textloom 1.x follows semantic versioning for its documented API and features,
keeps reading TLFR versions 1 and 2, and pins Unicode segmentation to protect
persisted grapheme boundaries. Exposed adapter dependency upgrades must respect
that contract. Release candidates may still change their API.

See [compatibility rules and publication steps](RELEASING.md),
[API compatibility details](GUIDE.md#stable-release-policy), and
[release notes](CHANGELOG.md). Validation scripts never publish a release.

MIT licensed; see [LICENSE](LICENSE).
