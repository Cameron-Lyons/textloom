# Textloom

A reusable native rich-text editor for Rust. The document and editing commands work without a window, renderer, browser, or async runtime. Optional adapters provide an **egui widget**, **winit keyboard/IME input**, and **AccessKit accessibility**.

The default package has one dependency: `unicode-segmentation`. There is no unsafe code in this crate.

```toml
[dependencies]
textloom = "1.0"
# Enable only the integrations your application uses:
# textloom = { version = "1.0", features = ["egui"] }
```

| Feature | Adds | Notes |
| --- | --- | --- |
| `egui` | `adapter::egui::RichTextEditor` | egui 0.36. Does not enable egui's `default_fonts`; eframe enables them by default. |
| `winit` | `adapter::winit::WinitAdapter` | winit 0.30, always with its `x11`, `wayland`, and `wayland-dlopen` backends. |
| `accesskit` | `accessibility::AccessKitAdapter` | AccessKit 0.24.1 or a later 0.24 release. |

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

- Bold, italic, underline, strikethrough, inline code, foreground and fill colors, hyperlinks, six heading levels, and block quotes. `InlineStyle::background` is a fill like Excel's Fill Color (`None` is "No Fill"), and `Color::STANDARD` lists Excel's ten standard colors with their names for font and fill pickers. `toggle_style(InlineAttribute)` toggles one attribute as the adapters' shortcuts do; `StylePatch::from(SelectionStyle)` copies a selection's uniform formatting, including fill, as a format painter does.
- `clear_formatting()` resets selected inline styles, colors, and fills and the typing style in one undo step, like Excel's Clear Formats, preserving links and paragraph kinds. At a caret it resets only future typing.
- `selection_style()` reports uniform or mixed attributes independently for formatting toolbars, including the pending typing style at a caret. `selected_paragraphs()` and `selection_paragraph_kind()` describe the paragraphs that block commands change.
- Links are validated `Link` targets (nonempty, at most `Link::MAX_LEN` bytes, no control characters or line/paragraph separators) carried by `Span::link`; Textloom stores them verbatim and never opens them. `set_link()` links or unlinks the selection in one undo step, or at a caret edits or removes the whole link there; `insert_link()` inserts linked text, or the target itself for empty text; `selected_link()` prefills a link field, and `Document::link_at()`/`link_range_at()` serve click handling. Typing strictly inside a link extends it, typing at its edges does not, and rich paste keeps pasted links.
- Bulleted and numbered paragraphs with indentation. An ordered item stores only an optional explicit start: `Ordered { start: Some(n), .. }` restarts at `n`, and `start: None` continues. `Document::list_number()` computes displayed numbers as a word processor does: an item continues the nearest earlier ordered item at its indent unless a heading or a shallower list item lies between them, so body text, quotes, and same-level bullets interrupt a list without ending it; deeper items are skipped. Edits never rewrite other paragraphs to renumber them, so converting an item to another kind and back restores its list exactly; when a deletion removes an item with an explicit start, the item that continued its list inherits that start. `apply_numbered_list(indent)` implements a Numbered List command: it extends a list ending right before the selection, or starts one at 1, and changes nothing when every selected paragraph is already numbered at that level. `set_paragraph_kind()` applies an ordered kind's `start` to the first selected item, and `indent_list()`/`outdent_list()` keep starts. Enter continues a list or quote; Enter on an empty item outdents it one level and leaves the list at level zero, and Enter on an empty quote paragraph makes it body text. Backspace at the start of an item outdents it the same way, so level zero becomes body text, and Backspace at the start of a quote paragraph makes it body text; a second Backspace joins paragraphs. Indentation commands leave quotes alone.
- Splitting a heading keeps the text after the split a heading; Enter at its end continues with body text. Typing or plain-text paste over a selection that starts at a paragraph start and crosses a paragraph break takes the kind of the paragraph containing the selection end, as deleting first would.
- Directional selections, replacement, grapheme and word movement/deletion, paragraph/document navigation, and selection extension. Word movement and deletion stop at Unicode words and at runs of punctuation, symbols, or emoji, skipping whitespace. `select_word_at()`, `select_paragraph()`, and `Document::word_range_at()` implement double and triple click; `delete_to_paragraph_start()` deletes back to the paragraph start in one undo step.
- Literal find/replace across paragraph breaks, optional Unicode lowercase matching and whole words, next/previous search with wrapping, `replace_selected_match()` for a Replace button, and `replace_all()` as one undo step; replacements keep the style and link of each match's first character. Matches always contain complete graphemes. Case-insensitive search uses lowercase mappings and treats final sigma `ς` as `σ`, without Unicode normalization or full case folding.
- Bounded undo/redo that restores text, formatting, selection, and typing style. Consecutive typing, Backspace, or Delete within a paragraph each coalesce into one undo step, including typing that first replaces a selection; a deleted paragraph break and each word deletion are separate steps. `break_history_group()` starts a new step, `transaction()` records several commands as one step (merging successive edits to one paragraph) and rolls them back on error or panic; edits that cancel out still record a step, and `HistoryLimits` bounds retained entries and estimated bytes. `mark_saved()` and `is_modified()` compare content, so undoing back to the saved text reads as unmodified.
- IME begin/update/commit/cancel. Preedit text is transient, replacement selections survive updates, and commits without paragraph breaks join adjacent typing for undo, so syllable-by-syllable input is one step.
- LF, CRLF, lone CR, and U+2029 are paragraph breaks on import and insertion. U+2028 LINE SEPARATOR is a soft line break within a paragraph: `insert_line_break()` (Shift+Enter) inserts one in the typing style, keeping the paragraph's kind and the typing undo step. Plain-text export and selected text write soft breaks as LF and HTML as `<br>`; a search query's U+2028 matches soft breaks, while its LF, CRLF, CR, or U+2029 matches only paragraph breaks.

```rust
use textloom::{Editor, SearchOptions, StylePatch};

let mut editor = Editor::from_text("one two one");
let options = SearchOptions::default();
// A Replace button replaces the current match, then selects the next one.
assert!(editor.find_next("one", options, true).unwrap());
assert!(editor.replace_selected_match("one", "1", options).unwrap());
assert!(editor.find_next("one", options, true).unwrap());
editor.mark_saved(); // For example, after writing the document to a file.

editor
    .transaction(|editor| {
        assert!(editor.replace_selected_match("one", "1", options)?);
        editor.select_all();
        editor.apply_style(StylePatch { italic: Some(true), ..Default::default() })
    })
    .unwrap();
assert_eq!(editor.document().plain_text(), "1 two 1");
assert!(editor.is_modified());
assert!(editor.undo()); // The whole transaction is one step.
assert!(!editor.is_modified());
```

Replacing an editor inside a transaction leaves the replacement intact. An
editor moved out of the callback becomes an independent snapshot once its
transaction scopes end, retaining its edits as one undo step subject to history
limits, like a clone. Errors and panics roll back only the editor still
associated with the matching transaction guard.

```rust
use textloom::{Color, Editor, Link, ParagraphKind, Position, Selection, StylePatch};

let mut editor = Editor::from_text("Setup\nInstall Rust 1.95\nOptional\nRun the example");
editor.set_selection(Selection::new(Position::new(1, 0), Position::new(3, 15))).unwrap();
editor.apply_numbered_list(0).unwrap();
// Body text interrupts the list without ending it.
editor.set_selection(Selection::caret(Position::new(2, 0))).unwrap();
editor.set_paragraph_kind(ParagraphKind::Body).unwrap();
let numbers: Vec<_> = (0..4).map(|index| editor.document().list_number(index)).collect();
assert_eq!(numbers, [None, Some(1), None, Some(2)]);

// Fill "Rust 1.95" with Excel's Yellow, then link it.
editor.set_selection(Selection::new(Position::new(1, 8), Position::new(1, 17))).unwrap();
editor.apply_style(StylePatch { background: Some(Some(Color::YELLOW)), ..Default::default() }).unwrap();
let link = Link::new("https://www.rust-lang.org/").unwrap();
editor.set_link(Some(link.clone())).unwrap();
assert_eq!(editor.selected_link(), Some(link));
let html = editor.document().to_html();
assert!(html.contains(r#"<a href="https://www.rust-lang.org/">"#));
assert!(html.contains("background-color:#ffff00"));
```

Positions are `(paragraph index, UTF-8 byte offset)`. Document selections must land at extended grapheme boundaries: combining marks, emoji sequences, and flags cannot be split. Invalid public input returns an error before mutation. IME preedit selections use native UTF-8 scalar boundaries because a composition can contain unfinished graphemes.

`Document::paragraphs()` exposes immutable shared paragraphs and normalized style spans with their links. `plain_text()` and clipboard selection export omit visual list markers. `Paragraph` also maps grapheme/scalar indexes to UTF-8 bytes for custom GUI integrations. Importers build validated rich paragraphs, whose text excludes CR, LF, and U+2029, with `Paragraph::new()` and assemble them with `Document::from_paragraphs()`. `Editor`, `Document`, `Paragraph`, and `Fragment` are `Send + Sync`; cloned editors share unchanged paragraphs and keep independent history.

`SelectionStyle` uses `Some(true)` for uniformly enabled emphasis, `Some(false)` for uniformly disabled emphasis, and `None` for mixed values. Its foreground and background fields use `None` for mixed values, `Some(None)` for the renderer's default color or no fill throughout, and `Some(Some(color))` for one explicit color throughout. The summary ignores paragraph separators and empty paragraphs; a selection containing only separators uses the pending typing style. Reading it does not allocate or change selection, history, or IME state. Typing over a selection uses the style of its first selected grapheme in either direction; a caret uses the text before it, and moves that leave the selection unchanged keep a pending caret style.

## Rich clipboard and interchange

`selected_fragment()` captures text, styles, links, and paragraph kinds. An ordered item that begins a list and is the fragment's first item at its level is captured with start 1, so a copied list stays a new list; items continuing an earlier list keep continuing. `insert_fragment()` pastes in one undo step, preserving surviving edge styles. Complete pasted paragraphs keep their source kinds and explicit starts, so pasted continuations continue the destination's list, and destination text that survives on a pasted line keeps its paragraph kind. A nonempty fragment without a paragraph break adopts its source kind only when it fills an empty line or whole selected paragraphs. A single empty fragment paragraph leaves document content unchanged at a caret, but deletes a nonempty selection. Multiple empty paragraphs still insert paragraph breaks. The GUI host owns clipboard MIME types and transport.

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

`Document` and `Fragment` support lossless `to_bytes()`/`from_bytes()` with a versioned, architecture-independent native format (TLFR); RELEASING.md's compatibility contract specifies its canonical rules. The encoder writes version 2, and the decoder also reads version 1, whose stored list ordinals become explicit starts only where they differ from the computed numbers, so old snapshots show the same numbers. `Fragment::from(&Document)` and `Document::from(Fragment)` convert whole documents. Decoding validates UTF-8, grapheme-safe canonical spans, paragraph kinds, lengths, and trailing data. By default (`DecodeLimits::DEFAULT`), inputs are limited to 64 MiB and one million paragraphs/spans. In-memory documents may exceed these limits, so the default decoder rejects their oversized snapshots; reopen trusted ones with `Document::from_bytes_with_limits(bytes, DecodeLimits::UNLIMITED)` or raised `DecodeLimits` fields. HTML export escapes text and preserves whitespace, colors, fills, formatting, links, headings, block quotes, soft line breaks as `<br>`, and nested lists with their computed numbers as `start`/`value` attributes. A fragment captured from a range keeps the numbers its items showed, so copying the third to fifth items exports `<ol start="3">`. Consecutive runs with one link share an `<a href>`; `javascript:`, `vbscript:`, and `data:` targets are exported as plain text. The color span, which also carries the fill as `background-color`, directly encloses `<u>` and `<s>`, inside any `<code>`, so decoration lines take the text color. NUL becomes U+FFFD because HTML cannot represent NUL text. Run `cargo run --example rich_text` for a complete core-only example. Embedded objects and HTML/Markdown parsing remain outside the package's scope.

## egui

Enable `features = ["egui"]` and add the widget from a native egui host:

```rust,ignore
ui.add(textloom::adapter::egui::RichTextEditor::new(&mut editor));
```

The widget draws rich paragraphs, selections, list markers, headings, block quotes, and preedit text. Fills paint behind the glyphs and replace inline code's background, with selections and highlights visible over them. Links use egui's hyperlink color unless explicitly colored and are always underlined. Quote paragraphs are indented with a bar in the gutter that continues across consecutive quotes. Soft line breaks start new rows for the caret, hit testing, Home/End, Up/Down, IME preview, and selection painting. It routes pointer and keyboard input through the same core and publishes a caret-local IME area (the caret plus same-row preedit) through egui's platform output, so candidate windows follow the caret. It checks layout character counts and retries inconsistent font shaping to keep rendered text and cursor positions aligned. Each independently stored editor should use a stable widget ID. For a scrolling editor, place the widget inside an egui `ScrollArea`. Cached layouts stay in egui's temporary memory until `RichTextEditor::forget_state(ctx, id)` releases them, for example when an editor closes.

A primary-button press places the caret, dragging extends the selection from the press point, Shift+click extends it, and double/triple click select a word or paragraph on release; a press and release within one frame, such as a touchpad tap, also counts as a click. Ctrl/Command+click on a link in an editable widget, or a click in a read-only one, activates it on release over the same link without moving the caret; a drag from a link still selects. Document changes discard pending link presses. Hovering a link shows its target in a tooltip, and a pointing hand while a click would activate it. On touch screens, swipes scroll the enclosing area until the editor has focus. Clicks retain the chosen visual row at wrap boundaries, and selection highlighting extends a hard paragraph break only after the final wrapped row. Focused caret and preedit changes scroll into view even when the widget starts outside the viewport. Clicking, dragging, or moving focus away during IME composition commits the visible preedit, as native text views do, after first applying IME events queued earlier in the same frame; one immediate identical commit that an input method repeats after the click or focus change is ignored, including after focus returns. Escape, read-only mode, and disabling the widget cancel it. Cancellation also suppresses one immediate identical commit, even after ignored or invalid input; valid nonempty preedit starts a fresh composition. Typing or navigation after an empty preedit also cancels the old candidate. Keyboard and accessibility actions follow their input batch order; accessibility actions based on content changed by an earlier event are rejected as stale.

When pointer events arrive together, selection uses the press position and its
modifiers, and a drag ends at its release position even if the pointer subsequently
moves elsewhere. A first preedit queued before a click applies to the original
selection before the click places the caret.
Ignored keys, editing commands blocked by read-only mode, and changes to pending
typing style preserve the caret's visual row and preferred vertical column.

Both input adapters share these Ctrl/Command shortcuts:

| Ctrl/Command + | Action |
| --- | --- |
| A; C, X, V | Select all; copy, cut, paste |
| Z; Shift+Z or Y | Undo; redo |
| B, I, U | Toggle bold, italic, underline |
| 0; 1–6 | Body text; heading level |
| Shift+7; Shift+8 | Numbered list (`apply_numbered_list(0)`); bullet list |
| ]; [ | Indent; outdent selected list items |
| Up or Home; Down or End | Document start; document end |

Without Ctrl/Command, Enter inserts a paragraph break and Shift+Enter a soft line break in both adapters; read-only mode consumes both, and Ctrl/Command+Enter or Ctrl/Command+Shift+Enter reaches the host. Toggling makes a mixed selection uniformly emphasized and a uniformly emphasized selection plain for that attribute. Shortcuts listed without Shift ignore chords with Shift held, which reach the host (for example, Ctrl+Shift+V for paste as plain text). Latin logical keys select shortcuts, so Dvorak and Colemak keep their letters; non-Latin layouts use the physical US key, and the digit row uses its physical digit (AZERTY, German Shift+7). Ctrl/Alt+Left/Right/Backspace/Delete move or delete by word, and macOS Command+Backspace deletes to the paragraph start, or deletes the selected text, in one undo step. Tab/Shift+Tab indent/outdent selections containing list items; other Tab presses stay available for host focus traversal. Losing widget or window focus ends the current typing undo group.

In the widget, Home/End and macOS Command+Left/Right move to the edges of the visual (wrapped) row, Up/Down move by visual line, and PageUp/PageDown move about one viewport while keeping the column; Shift extends each. Escape cancels a composition, or otherwise releases focus. `.heading_shortcuts(false)` leaves Ctrl/Command+0–6 to the host, such as egui's zoom reset. `.read_only(true)` retains selection, copy, and accessibility while suppressing edits and IME, and consumes only the editing chords an editable widget handles. Typing, paste, and IME commits drop control characters; paste and commits keep line breaks and tabs, and IME text consisting of one line break is ignored, as in egui's `TextEdit`. A nonempty preedit blocks paste, cut, and typed text.

`widget.show(ui)` returns `RichTextEditorOutput` with rejected commands, accessibility errors, and `caret_rect`/`selection_rect`, computed even while the widget is unfocused: for example, call `ui.scroll_to_rect(output.caret_rect, None)` after `find_next()` from a search field, or anchor pop-ups. Its `clicked_link` reports a link activated by a click or by assistive technology; the widget never opens links, so the host decides which targets to follow. `.highlight_ranges(&matches, color)` paints sorted, nonoverlapping ranges such as `Document::find()` results under the selection. The caret blinks according to egui's `text_cursor` visuals. List and quote gutters and marker gaps scale with the font size, and ordered markers reserve three digits so items 1–999 align.

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
and IME rules. Read-only cut copies without deleting; empty or control-only
paste events preserve selection and history. A nonempty preedit blocks paste and
cut, while an empty preedit is canceled before a nonempty paste. Plain clipboard behavior remains
the default and does not capture rich fragments. Egui hosts may omit a paste
event when native plain text is unavailable, so rich writers also need that fallback.

Call `textloom::adapter::egui::install_input_routing(&context)` when creating
the egui context (in eframe, use the creation callback's `egui_ctx`). This
preserves input order around AccessKit focus requests for rich editors and
native `TextEdit` fields, regardless of widget draw order. Each focus request
gets a separate UI pass before subsequent input is delivered. Ordinary input
needs no extra pass; if egui's pass limit is reached, remaining input waits for
the next repaint, ahead of newer input. The widget also installs routing on
first use, taking effect on the next pass. Hosts must honor the returned repaint
requests to deliver pending input, including when using manual `begin_pass` /
`end_pass` calls. Window lifecycle and dropped-file handling remain host-owned.

Run `cargo run --example egui_editor --features egui` for a display-independent integration example. A native window/renderer is supplied by your existing egui host; this crate does not select one for you. Egui integrations other than eframe that rely on egui's bundled fonts must enable `egui/default_fonts` themselves. Bold spans use egui's strong text color; register a bold font family and select it with `.bold_font_family()` for bold glyphs as well. If the selected bold family is not registered, bold text keeps the base family, and without a monospace family, code uses the base family; neither panics.

For a complete native window, run the repository's standalone eframe host:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

The host demonstrates mixed selection state, formatting with Excel-style fill
and font colors, links, quotes, lists with numbering controls, find/replace,
snapshots, and read-only/disabled modes with a system plain-text clipboard and
native IME caret area. Add `--rich-clipboard` for its native Linux (Wayland/X11),
macOS, and Windows backend; other Unix targets build the host but reject the
flag. Copies publish HTML and plain text alongside a framed
TLFR fragment with a generation token, and verify all representations through
bounded helper processes. Paste uses one validated native snapshot; ambiguous,
unavailable, malformed, or mismatched inputs retain the event's plain fallback.
Owner helpers are retired on replacement or close. Platform wrapper dependencies
stay in the example's separate Cargo workspace and lockfile, outside the published
library archive and library dependency graph. `--read-only` starts a selectable
viewer; `--disabled` disables document focus and interaction. The toolbar can
toggle both modes, and document/search fields have accessibility labels.
`--font PATH` and `--bold-font PATH` register application fonts; `--smoke-test`
requests a graceful close after 20 native frames. Pass these arguments after
`--` in the command above.

`--clipboard-self-test` requires `--rich-clipboard` and writes a fixture to the
system clipboard, so use it only in isolated sessions. It checks native rich,
HTML, and plain read-back plus widget rich insertion and undo/redo. Isolated
fixtures passed on Linux X11/Wayland, macOS, and Windows; evidence is recorded in
`RELEASING.md`. External-application interoperability, IME, dead keys/AltGr, and
screen-reader behavior still need native platform signoff.

`--qa-report PATH` creates a new report file at startup, exiting with status 2
if it exists or cannot be created, and writes a JSON report on graceful exit.
It records platform, scale, event/error counts, observed editor transitions,
final-state metrics, and an in-memory TLFR round trip without document,
clipboard, search, or preedit text. Clipboard request/completion flags and a
self-test error counter describe the opt-in fixture. The report always records
manual signoff as unrecorded. Reports and smoke runs provide observations;
manual native checks still need separate steps and outcomes. See the [native example guide](https://github.com/Cameron-Lyons/textloom/blob/main/examples/native-editor/README.md)
for report revision labeling, font examples, and host checks.

## winit

Enable `features = ["winit"]`. `adapter::winit::WinitAdapter` maps keyboard events, modifiers, and native IME events to editor commands. The host provides text layout, pointer hit testing, clipboard access, and painting. Calls return a `#[must_use]` `InputOutcome`: whether the event was handled, whether presentation changed, and any clipboard request.

Call `set_focused` when the editor widget gains or loses focus and pass window events to `handle_window_event`, which supplies each key's physical key; hosts calling `handle_key(&key, physical_key, state, text, editor)` directly pass `event.physical_key`. Forward `ModifiersChanged` to every adapter, because modifiers belong to the window and survive widget blur. Call `sync_ime(&window, &editor, position, size)` with the caret's logical window area every frame while the editor is focused and at least once after it blurs; when focus moves between editors, sync the one losing focus first. It sends only changes: it allows IME after focus gain or `Ime::Disabled`, releases it once on blur or read-only mode, and interrupts a preedit the editor dropped. Pending preedit and commit events from a canceled composition are ignored until that interruption, including an empty preedit preceding the commit. Winit 0.30's spurious macOS `Focused(false)` at startup (winit #4371) is corrected automatically. Deliver clipboard text through `paste`. Avoid routing the same events through both egui and winit adapters. Run `cargo run --example winit_input --features winit` for a window-independent example of selection, clipboard requests, and IME replacement.

Shortcuts use the `CommandModifier`: Control by default, or Super (Command) on macOS. Beyond the shared table above, Home/End move to paragraph edges and Up/Down move by paragraph; `host_vertical_navigation(true)` leaves Up/Down to the host for visual-line movement, and PageUp/PageDown always stay with the host. With Super, Command+Left/Right also move to paragraph edges. With Control, Shift+Delete cuts a selection, Ctrl+Insert copies, and Shift+Insert requests paste; dedicated Cut, Copy, Paste, Undo, and Redo keys work in both modes, and read-only rules apply to all of them. Ctrl+Alt character chords type text only on Windows, where they produce AltGr characters; elsewhere they reach the host. Typed text drops control characters; paste and IME commits keep line breaks and tabs. During a nonempty preedit, `paste` and `paste_fragment` return a handled outcome without changes; an empty preedit is canceled first.

An empty preedit survives IME processing keys, Copy/Cut with no selection, and
input the adapter leaves unhandled, including navigation delegated to the host.
Other handled editor commands and actual typed text cancel it before applying
their action.

Opt into `WinitAdapter::new().rich_clipboard(true)` for `CopyRich`/`CutRich` fragment payloads, then deliver native rich clipboard content through `paste_fragment()`. Plain clipboard events remain the default.

Call `set_read_only(true, &mut editor)` for a selectable viewer: navigation and copy remain available, cut copies, and keyboard edits, paste, and IME are suppressed. Enabling read-only mode cancels active preedit; call `sync_ime` to update the native window.

## Accessibility

`accessibility::AccessibilitySnapshot` exposes borrowed paragraph semantics and directional selection without an extra dependency, and `apply_action` applies validated `AccessibleAction`s through the editor. The optional `accesskit` feature provides `AccessKitAdapter`, a text input tree with styled runs, grapheme boundaries, selection, and validated accessibility actions. Each paragraph's runs form one logical line, or one line per soft line break, which is published as LF at the end of its line; AccessKit has no separate soft-break concept, so consumers may also treat it as a paragraph end. Runs hold at most 255 characters, carry word starts that match word movement, publish fills as background color, and split where formatting or links change. Runs with a hyperlink are children of a `Link` node with the target as its URL and the linked text as its label; consecutive quote paragraphs are children of one `Blockquote` node. Text navigation passes through both levels. Connect its tree updates and action requests to your host's platform AccessKit adapter.

Updates are incremental. Call `AccessKitAdapter::invalidate()` whenever the platform adapter requests an initial tree (`ActivationHandler::request_initial_tree`, or accesskit_winit's `WindowEvent::InitialTreeRequested` before the next update) and after discarding an update; its documentation includes a recipe. Hosts that add bounds or character geometry must reapply them to every node in each update, including the root and editor nodes, and can call `invalidate()` to republish every node after layout changes. `locate_text_run()` and `locate_link()` map a run or link node back to its paragraph and byte range for `ScrollIntoView` requests. A `Click` on a link node, also in read-only mode, returns the link in `AccessibilityOutcome::activated_link` without changing the editor. A tree grafted with `with_tree_id()` uses a generic container root.

The host owns focus and layout geometry. Both adapters use AccessKit 0.24, matching egui's integration and avoiding a duplicate dependency version. Use a matching platform adapter for the direct tree. Text movement is logical; a renderer must provide visual movement across wrapped lines or bidirectional text. AccessKit limits a character to 255 UTF-8 bytes, so the direct adapter and the egui widget publish a longer grapheme as several characters whose interior positions resolve to the grapheme start. The egui widget publishes the same link, block quote, and soft-break structure and reports link clicks in `clicked_link`. Egui list items also publish their position and set size: headings close every set, shallower items close deeper sets, body text, quotes, and the other list kind at the same level interrupt a set without closing it, and an explicit start begins a new set. Displayed list numbers are not published because AccessKit 0.24 has no item-number property.

The direct `AccessKitAdapter` supports selection replacement and full-value replacement as one undo step; full-value replacement uses the style at the document start. `AccessibilityOutcome::composition_canceled` reports discarded preedit so the host can reset the platform input method. The adapter's `set_read_only(true, &mut editor)` advertises read-only capabilities and suppresses replacement actions while retaining focus and selection. Publish `update()` after changing the mode, and keep it aligned with your input adapter.

## Performance and validation

The editor and egui widget share immutable IME snapshots. Idle frames retain
preedit state without copying or comparing its full text, and identical updates
reuse the existing snapshot. Updates copy shared state only when it changes;
exclusively owned preedit text reuses its buffer. Cursor changes retain the
preview document and refresh its shared snapshot. Document and fragment
plain-text export use one implementation.

Text and formatting are shared between immutable paragraphs. Editing rebuilds only the affected paragraphs; undo keeps localized before/after deltas instead of copying the full document. ASCII and uniformly styled Unicode edits reuse their owned span vectors, mixed-style runs that already end at grapheme boundaries are kept without segmenting the paragraph, and Enter at a paragraph end keeps the existing paragraph. Fragment text imports and document snapshot imports move paragraph vectors from their temporary owners. Snapshot decoding validates borrowed text before retaining it and merges validated spans in place. Single-paragraph typing merges history entries without retaining every intermediate string. Each history entry holds its delta directly, and disabled history skips retention work. CR/CRLF normalization copies unchanged text in one pass.

Capturing a selection without numbered items visits only the selected paragraphs;
it does not scan earlier paragraphs for list context.
Inserting empty text or a single empty fragment paragraph at a valid caret
returns without rebuilding paragraph text or formatting runs.
`is_modified()` caches its content comparison until the next document change,
so repeated reads between edits take constant time. The first comparison after
an edit or history replay may still visit the complete document.

Inline formatting locates selected runs with binary search. Commands that leave
their styles unchanged retain the original paragraph and runs. Changes patch
selected runs, copy untouched edges directly, and share paragraph text and
Unicode indexes. Rebuilding changed runs still copies the paragraph's run array.
Reading the selected link at a caret also uses binary lookup. Explicit link-range
queries and link edits still visit the linked formatting runs.

Grapheme, scalar, and word indexes are built independently when needed and shared across formatting/history snapshots. Position validation, style lookup, boundary snapping, and adjacent grapheme movement reuse an existing grapheme index or inspect bounded local Unicode context. If that context is insufficient, they build the full index once and reuse it. Ordinary arrow movement and grapheme deletion avoid indexing an entire newly edited paragraph. Conversions at paragraph start return zero without building either index. Grapheme counting and other index conversions still build the full index, while scalar counting does not; indexed navigation uses binary searches instead of rescanning long paragraphs. ASCII grapheme/scalar offsets need no indexes, and ASCII word indexes are allocated only when used. `history_bytes()` estimates retained text, formatting, and link targets, charging the larger side of each change once so that text and runs shared by both sides are not counted twice, and each shared link target once per side; shared navigation caches and allocator overhead are excluded.

A text edit costs time proportional to the affected paragraph bytes and runs. Inserting/removing paragraphs also shifts later paragraph references in the document's vector. List edits change only the paragraphs they touch: displayed numbers are computed from paragraph kinds and cached, and the first lookup after a change that affects numbering or the paragraph count recomputes them in one pass. Switching between body and quote, or between heading levels, preserves the numbering cache, including undo/redo. This is intended for application text fields and documents with reasonable paragraph sizes; it is not a rope for enormous single-line buffers.

Queries without paragraph breaks search paragraphs independently: case-sensitive searches borrow text, and lowercase matching and whole-word indexes use temporary text/index memory bounded by the current paragraph. Word indexes and lowercase coordinate maps are prepared only when a literal candidate needs validation. Word indexes grow only as far as candidates need and remain local to each search. Find-next/previous start at the selection boundary paragraph, visit paragraphs in the requested direction, and wrap through a bounded second interval. When a query cannot overlap itself, navigation searches from the selection: case-sensitive queries without whole-word matching scan directly, and other queries lowercase windows that double in size away from the selection, segmenting words from the nearest earlier space followed by an ASCII character other than a space, or from the paragraph start. Queries that can overlap themselves retain the complete forward match stream and lowercase context. All matches respect the original paragraph's grapheme boundaries. A nearby match can be found without searching unrelated earlier text; missing queries may still visit the whole document. Cross-paragraph queries that cannot overlap themselves are matched paragraph by paragraph through the same navigation intervals, without copying the document: the query's first line must end a paragraph, its middle lines must fill the following paragraphs, and its last line must start the next one. Cross-paragraph queries that can overlap themselves temporarily flatten the document and preserve its complete forward match stream. Literal searchers are reused across matches within each searched text. After a rejected candidate, case-sensitive paragraph-local searches without whole-word matching can skip to the next grapheme start. Potentially overlapping candidates get at most two retries for short queries before using a linear byte matcher with temporary memory proportional to the query length; patterns that cannot overlap retain the standard string iterator. This avoids repeatedly rescanning long, repetitive queries. Replace-all shares complete untouched paragraphs and builds text only for joined or edited paragraphs. It reuses validated search ranges and prepares replacement chunks once per operation.

The egui widget separates layout, input, navigation, painting, and composition
preview. It retains galleys and paints visible paragraphs. Committed edits carry
a compact description of the replaced paragraph range to the adapters. Ordinary
local edits update the affected cache entries and paths in a persistent layout
index, which tracks paragraph heights, scalar totals, and list membership.
Retained layout snapshots keep their original geometry. Structural edits also
shift later paragraph-cache references; after changes that affect numbering or
the paragraph count, only markers whose computed number changed are rebuilt,
reusing their items' shaped text. Actual changes to font definitions, text
options, DPI, or appearance still rebuild all layouts. If an adapter misses an edit or receives
an unrelated document, it compares paragraph identities to recover safely.
Height totals use `f64` internally before conversion to egui coordinates,
reducing accumulated rounding drift in long documents.

Successive IME preedit updates reuse unchanged paragraph layouts in a separate
preview layout cache; cursor-only changes reuse the preview document too. Native
preedit scalar ranges convert to byte ranges in one scan through the requested
endpoint. Word-selection snapping reuses paragraph boundary queries. Idle frames
compare the layout environment (pixels per point, text options, and font
definitions) and reuse cached painted galleys for visible paragraphs without
cloning or hashing their full layout jobs. A tiny cached job detects font-cache
resets and refreshes visible glyphs when needed. Each widget retains only its
most recently visible painted galleys. Other viewports' passes and frames that
hide the widget neither relay out the document nor reset vertical navigation, and font atlas
recreation refreshes only painted texture coordinates. Accessibility validates
text and layout snapshots with shared identity tokens and locates text runs
through an index. Egui paragraph node IDs stay stable across structural edits,
shifted and renumbered paragraphs reuse their runs, and at a wrapped-row seam
the published caret follows the painted caret's affinity. A uniquely owned egui
accessibility snapshot patches a single changed paragraph and its run locations;
retained snapshots and broader invalidation use validated rebuilds. Publication
still registers the complete tree required by egui, cloning cached run
properties and applying current coordinate transforms. Direct AccessKit
selection/label changes reuse paragraph nodes; precise single-paragraph edits
update that paragraph and its run locations without scanning unchanged paragraph
identities, and structural edits rebuild only the replaced paragraphs. The
editor child list still scales with paragraph count. Direct runs of ASCII
paragraphs skip Unicode segmentation; other paragraphs pay word segmentation for
their published word starts. Unchanged direct trees return an empty update in
constant time.

The private editor modules keep history retention/coalescing and transient IME
state separate from editing commands. Change
notifications retain one predecessor identity and no paragraph data; they do not
grow with history or add allocations to the existing document identity updates.

```sh
cargo test --locked --no-default-features
cargo test --locked --all-features
cargo test --release --locked --all-features --test operations --test search
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo doc --locked --all-features --no-deps
cargo bench --bench editing
cargo bench --features egui --bench rendering
cargo package --locked --all-features
```

The dependency-free benchmark harnesses measure local insertion/undo, paragraph imports, long Unicode and ASCII navigation, literal search, batched replacement, and headless egui idle/edit/preedit frames. Set `TEXTLOOM_BENCH_QUICK=1` for shortened smoke runs. See [BENCHMARKS.md](https://github.com/Cameron-Lyons/textloom/blob/main/BENCHMARKS.md) for measured results and workload details. Tests cover Unicode editing, rich interchange, TLFR version 1 and 2 fixtures, and malformed or randomly corrupted files, search/replacement, selection direction, style preservation, fills, links, soft line breaks, block quotes, history limits and transactions, computed list numbering/continuation/indentation, composition lifecycle, native event routing, read-only mode, accessibility action conversion, and AccessKit trees loaded through `accesskit_consumer`. Native candidate windows and screen-reader behavior still need integration testing in each host/platform.

## CI

[GitHub Actions](https://github.com/Cameron-Lyons/textloom/actions/workflows/ci.yml) runs on pushes to `main`, pull requests, and manual dispatch:

- Ubuntu stable runs `scripts/check-release.sh`: formatting, tests and doctests for all eight feature combinations, release-mode search/replacement oracles, Clippy for all targets, complete API documentation with warnings denied, release benchmark smoke tests with `TEXTLOOM_BENCH_QUICK=1`, the headless examples, explicit package inventory verification, and tests and all-target compilation from the extracted Cargo package.
- Rust 1.95 tests every feature combination, builds the benchmarks, and compiles and tests the standalone native host, including its CLI/report tests.
- macOS and Windows run strict Clippy on all library targets and the native host, then all-feature tests, doctests, and host CLI/report tests.
- A minimal-versions job resolves the lowest direct dependency versions that `Cargo.toml` permits and compiles all targets on Rust 1.95.
- cargo-deny checks advisories and, as a separate required job, bans, licenses, and sources for both the library and native-host manifests, following `deny.toml`.

The Ubuntu release checks also test the native host, check its formatting and
Clippy warnings, and verify its dependency on the packaged library. The candidate
workflow runs required renderer smoke checks in editable, read-only, and disabled
modes on Linux X11 with Xvfb/Mesa, macOS, and Windows with signed MSYS2 Mesa
packages and application-local DLLs. Validation evidence is recorded in
`RELEASING.md`. Renderer smoke checks open native windows and verify initialization
and frame completion. They now enable rich clipboard in all modes and require
the isolated clipboard roundtrip/undo/redo fixture in editable mode. IME, external
clipboard interoperability, physical keyboard, and screen-reader interaction retain their manual release
gates on all three platforms.

Renderer jobs validate source-labelled, content-free JSON observations for
graceful completion, zero errors, snapshot integrity, and the requested mode.
They also validate rich clipboard activation and the requested fixture result.
Validated reports and smoke logs are available as CI artifacts.

Dependencies are locked, actions are pinned to commit SHAs, and jobs have time limits. Superseded pull-request runs are canceled; every `main` commit completes. The workflow only reads repository contents and saves dependency caches on `main`. A newly published advisory does not fail unrelated pull requests, but `main`, tag, and dispatched runs report it. CI runs release benchmarks as smoke tests and records their output; timing thresholds are kept out of shared runners.

## Stable release policy

Textloom 1.x follows semantic versioning for its documented public API and feature names. These enums are `#[non_exhaustive]`, so keep a fallback arm when matching them: the error enums (`Error`, `FragmentError`, `AccessKitError`, and the egui `AccessibilityError`), `ParagraphKind`, `Movement`, `InlineAttribute`, `AccessibleAction`, and the winit `ClipboardEvent`. `Composition`, `DecodeLimits`, `AccessibleParagraph`, `AccessibilityOutcome`, `RichTextEditorOutput`, and the winit `InputOutcome` are non-exhaustive structs that may gain fields; start `DecodeLimits` from `DecodeLimits::default()` or `UNLIMITED` and assign fields. `InlineStyle`, `StylePatch`, `SelectionStyle`, `SearchOptions`, `HistoryLimits`, `Position`, `Selection`, `Color`, and `Span` stay exhaustive, so adding a field requires a major release. Before 1.0, `ParagraphKind` gained `Quote`, and the `start` of `Ordered` became an optional explicit start (`Option<u32>`) instead of the displayed number; `InlineStyle`, `StylePatch`, and `SelectionStyle` gained `background`; `Span` gained `link`; `AccessibilityOutcome` stopped implementing `Copy`; and `Error` and `FragmentError` gained `InvalidLink`. `Editor`, `Document`, `Paragraph`, and `Fragment` remain `Send + Sync`, checked at compile time. Public adapter types use egui 0.36, winit 0.30, and AccessKit 0.24. Breaking upgrades to those exposed types require a Textloom major release. Exact error messages and benchmark timings may change.

TLFR is versioned independently of the crate: 1.x writes version 2 and keeps reading versions 1 and 2, whose bytes and meanings remain stable, including the list numbering rule that determines how stored kinds display. Unicode segmentation is pinned to version 1.13.3 (Unicode 17.0) because grapheme rules determine valid persisted style boundaries. Minimum Rust version increases, if needed, are announced in minor or major releases.

Run `./scripts/check-release.sh --allow-dirty` to check pending changes locally; `--tag v1.0.0` also requires a matching version and a dated changelog entry. A matching version tag runs the complete CI matrix and uploads the verified package plus a SHA-256 checksum. See [CHANGELOG.md](https://github.com/Cameron-Lyons/textloom/blob/main/CHANGELOG.md) for release notes and [RELEASING.md](https://github.com/Cameron-Lyons/textloom/blob/main/RELEASING.md) for compatibility rules, native-host checks, and publication steps.

Rust 1.95 or newer. MIT licensed; see [LICENSE](https://github.com/Cameron-Lyons/textloom/blob/main/LICENSE).
