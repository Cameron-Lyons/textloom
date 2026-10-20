# Changelog

## 1.0.0

Unreleased. Native host checks and publication steps are tracked in `RELEASING.md`.

First stable release of the native Rust rich-text editing library.

- Word selection expands Unicode word boundaries to whole graphemes, so
  prepend characters joined to emoji or punctuation cannot produce empty selections.
- Accessibility rejects link clicks from stale document snapshots. Egui full-value
  replacement uses document-start formatting and restores the prior caret style on undo.
- The native host processes queued IME input before applying toolbar commands,
  then commits remaining preedit before Undo, Redo, and snapshot capture.
- Egui discards pending link presses after document changes and suppresses one
  repeated IME commit across focus loss and restoration. The native toolbar
  supports Undo during the first composition and filters control characters
  when committing preedit.
- Caret and character-offset queries avoid counting an entire Unicode paragraph
  when its scalar offset is already known.
- Winit ignores queued preedit and commit events after composition cancellation
  until IME synchronization resets the session. Native list-numbering commands
  resolve their target paragraph after queued IME replacement.
- Copying a selection without numbered items skips list-context scans through
  preceding paragraphs.
- Repeated `is_modified()` queries reuse the content comparison until an edit,
  undo/redo, or transaction rollback invalidates it.
- Egui accessibility locates formatting sections with binary search, avoiding
  quadratic scans when a paragraph contains many formatted runs.
- Egui suppresses delayed identical IME commits after cancellation, including
  cleared preedit followed by editing or navigation. Ignored and invalid input
  preserves that guard; new compositions and input-method surrounding-text
  replacement still work.
- Literal search avoids quadratic retries on repetitive queries rejected at
  grapheme or word boundaries, preserving overlapping valid candidates.
- Native clipboard verification borrows text when newline conversion is
  unnecessary and checks identical publications without copying plain text.
- Native clipboard worker startup failures terminate and reap their helper,
  preserving plain-text fallback instead of panicking.
- The native link shortcut leaves AltGr and combined command modifiers available
  for text input. macOS QA cleanup tolerates processes exiting before a signal.
- Body/quote and heading-level changes preserve cached list numbers, including
  undo/redo, and avoid unnecessary scans of subsequent egui list markers.
- Egui reuses visible painted text between font-atlas changes, avoiding repeated
  cloning and hashing of long formatting jobs on idle frames.
- Egui uses pointer-event positions and modifiers when several events arrive in
  one frame, preserving click anchors and drag endpoints across widgets. A first
  preedit queued before a click applies to the original selection.
- The native host suppresses one delayed document IME commit after focus moves
  to Link, Find, or Replace, including deferred keyboard focus traversal.
- Winit preserves cleared preedit across IME processing keys and unhandled input,
  so the following valid commit still replaces its original selection.
- Empty text and single-paragraph empty fragment insertions at a valid caret
  avoid rebuilding the paragraph, preserving editing and history behavior.
- Egui preserves the caret's visual row and preferred column when keys are
  delegated to the host, editing commands are blocked by read-only mode, or
  formatting shortcuts change only the pending typing style.
- Native Find and Replace commands use search options changed in the same
  input batch.
- Converting grapheme or scalar index zero returns byte zero without building
  a Unicode index, including vertical movement into a paragraph at column zero.
- Egui accessibility selection requests preserve the requested side of a wrapped
  line boundary. Moving the visual caret restarts its blink even when the text
  offset stays the same.
- Caret link queries avoid expanding the entire linked range across formatting
  runs; link editing still updates the complete range.
- Native Link and Find fields handle Enter/Escape according to focus changes
  within the input batch, so keys in another field cannot submit or cancel them.
- Egui Cut and winit Copy/Cut with no selection preserve an empty preedit,
  allowing its following IME commit to insert text normally.
- Editors moved out of transaction callbacks retain usable undo/redo after
  their transaction scopes end, including nested scopes and error/panic exits.
- Native QA reporting compares preedit before copying it, avoiding repeated
  string allocations on unchanged frames.
- Egui routes AccessKit focus changes in separate UI passes, preserving text
  before and after each handoff across rich editors and native `TextEdit` fields.
  `install_input_routing()` enables this from the first pass; widgets install it
  automatically on first use. Deferred input retains its batch order and window
  focus. Native rich paste, IME handoff, and QA counts follow the routed passes.
- Consuming a `Fragment` into a `Document`, including decoding with custom limits,
  transfers the paragraph array without cloning every shared paragraph reference.

- Internal document/Unicode, history/IME, and egui layout/input/navigation/painting
  responsibilities are separated as a refactor without behavior changes.
  Precise paragraph-change notifications and a persistent layout index make
  ordinary local layout updates proportional to the affected range and tree
  paths. Accessibility preparation uses layout generations and localized run
  updates while retaining complete egui tree publication.

- `RichTextEditor::rich_clipboard()` accepts a host-owned `RichClipboard`
  transport. Hosts receive selected fragments before cut, can publish TLFR/HTML
  with plain alternatives, and can supply rich paste as one undo step. Successful
  host copies suppress egui's plain clipboard overwrite and retire older queued
  text/image copies; unavailable rich formats
  retain plain fallback. Focus, read-only/disabled, IME, accessibility, and input
  ordering rules remain enforced without adding native clipboard dependencies.
- The standalone native host adds independently toggleable disabled/read-only
  modes, associated document/search accessibility labels, and opt-in content-free
  JSON QA reports on graceful exit. Reports capture native input/state/error
  observations and snapshot checks, protect existing paths, and explicitly leave
  manual signoff unrecorded. CLI/report and clipboard regression tests join local
  and cross-platform host checks.
- The repository-only native host adds opt-in `--rich-clipboard` backends for
  Linux Wayland/X11, macOS, and Windows. Copies publish HTML/plain alternatives
  and a framed TLFR payload with a generation token, verify read-back through
  bounded helper processes, and retire owned helpers on replacement or close.
  Rich paste uses a captured native item with plain fallback for unavailable,
  invalid, mismatched, or ambiguous inputs. Safe platform wrappers remain outside
  the library dependency graph. `--clipboard-self-test` writes a deterministic
  fixture for isolated sessions; QA reports record request/completion flags and
  its error counter. Isolated native widget fixtures passed on Linux X11/Wayland,
  macOS, and Windows; external-app interoperability signoff remains required.
- The candidate CI workflow adds required Linux X11/Mesa and macOS native
  rendering smoke checks for editable, read-only, and disabled hosts, together
  with required Windows rendering using signed MSYS2 Mesa and application-local
  DLLs. Rich clipboard is requested in all modes; editable runs additionally
  require native clipboard roundtrip and undo/redo evidence. Renderer and fixture
  checks do not replace manual platform interaction signoffs.
- Hosted native interaction checks cover macOS TextEdit rich clipboard import,
  native typing/history/focus and guarded modes, and Windows NVDA generated
  speech, accessible selection, native replacement/history and graceful close.
  Release package verification requires these probes. macOS clipboard HTML
  explicitly declares UTF-8 so TextEdit preserves Unicode; this host-side fix
  leaves library HTML fragments and TLFR untouched. Physical desktop signoffs
  remain required.
- HTML export uses six-digit opaque colors and `rgba()` for translucent colors,
  preserving channel order and all 256 alpha values in Qt's HTML importer as well
  as CSS consumers. Native Qt clipboard checks preserve Unicode text, emphasis,
  lists, and color; external plain paste retains one-step undo/redo.
- The native host forwards the composing caret rectangle to winit so Japanese
  and Chinese candidate windows follow the caret. It retires processed IME events
  after rendering its text fields to prevent a Fcitx preedit/redraw feedback loop
  in Find and Replace. Its Linux example workspace
  backports upstream AccessKit activation and application-parent fixes for
  current AT-SPI and Orca while retaining the documented AccessKit 0.24 API.
  The vendored dependency and its licenses remain outside the library archive.
- Bold, italic, and underline shortcuts in both input adapters apply emphasis
  uniformly to mixed selections, matching toolbar behavior, and disable it only
  when the whole selection already has that emphasis.
- Widget/window focus loss ends the typing undo group in both adapters. Winit
  adds body/heading shortcuts matching egui, and Command+Backspace deletes to
  the paragraph start in both adapters while preserving the original undo
  selection and read-only/IME behavior.
- Whole-word searches build Unicode word indexes and lowercase coordinate maps
  only after finding a literal candidate, reducing work for missing and sparse
  queries without changing match boundaries or navigation order. Word indexes
  extend only through the prefix needed to validate candidates.
- Case-sensitive find-next/previous without whole-word matching can scan
  directly from the selection in either direction when the query's first
  Unicode scalar occurs only once, retaining original grapheme boundaries
  and the forward nonoverlapping match set.
- Sparse replace-all operations retain complete untouched paragraphs without
  copying their text and spans into temporary buffers. Joined paragraphs still
  normalize grapheme boundaries and formatting. Batched replacement reuses
  validated search ranges and prepares replacement chunks once per operation.
- Successive egui IME preedits retain unchanged paragraph layouts, while width,
  font, DPI, and appearance changes still invalidate the preview cache. Preedit
  coordinate calculation reuses the shared newline-normalization helper, and
  cursor-only updates retain the preview document. Native scalar ranges convert
  to byte offsets in one scan without counting the full preedit first, and word
  selection reuses paragraph boundary queries.
- Egui accessibility snapshots share unchanged paragraphs' text runs and
  geometry after local edits, retain static run node properties, and register
  run widgets without allocating a separate UI per run. Publication applies the
  current coordinate transforms. Direct AccessKit updates affecting at most one
  paragraph without changing paragraph count retain paragraph storage and
  update only changed run index entries. Structural updates preserve distinct
  occurrence IDs, and failed updates leave the previous tree intact.
- Paragraph rebuilds reuse owned span vectors for ASCII and uniform Unicode
  text. Fragment text imports and document snapshot imports move their paragraph
  vectors instead of copying temporary references. Snapshot decoding validates
  borrowed text before retaining it, checks CR/LF bytes without decoding every
  scalar, and merges validated spans in place.
- Position validation, style lookup, and grapheme snapping inspect bounded local
  Unicode context when no full index exists. Incomplete context falls back to
  the shared full index, preserving long regional-indicator, emoji, and Indic
  sequences. Release checks now run search/replacement oracles with debug
  assertions disabled.
- Adjacent grapheme movement and deletion reuse the same bounded boundary
  queries, avoiding full Unicode indexes after ordinary text edits. Structural
  egui layout updates retain unchanged cache entries around the changed range
  and rebuild appearance-invalidated entries without a lookup table.
- The editor and egui preview share immutable IME snapshots, avoiding full
  preedit copies and comparisons during idle frames. Identical updates retain
  their snapshot, changed state preserves earlier snapshots, and exclusive text
  updates reuse their buffer. Document and fragment plain-text export share one
  implementation.
- Inline clear-formatting preserves paragraph kinds and selection direction,
  supports caret typing-style reset, and restores rich state through undo/redo.
- Inline formatting locates selected runs with binary search and retains
  unchanged paragraphs without rebuilding their runs. Changed formatting copies
  untouched edges directly, merges affected boundaries, and shares paragraph
  text and Unicode indexes.
- `SelectionStyle` reports uniform and mixed inline attributes independently for
  formatting toolbars. `Editor::selection_style()` respects partial Unicode and
  directional selections, ignores separators, and reports pending caret styles
  without allocating or changing editor state.
- Selectable read-only winit input suppresses keyboard edits, paste, and IME
  while retaining navigation and copy. Direct AccessKit read-only mode cancels
  existing preedit and blocks accessibility text replacements while retaining
  selection. Both support runtime mode changes; AccessKit full-value replacement
  uses one undo step.
- Egui accessibility full-value replacement restores the previous directional
  selection on undo. Malformed and stale accessibility actions are rejected
  without editing content.
- Egui keyboard and accessibility events follow their input batch order, so
  edits invalidate later stale accessibility coordinates instead of reordering
  the actions. Unrelated host events retain their original order.
- Egui pointer carets retain the clicked row at wrap boundaries, and hard-break
  selection highlighting extends only the final wrapped row of a paragraph.
  Focused caret and preedit changes scroll into view from clipped content,
  including external selection changes and newly allocated preedit height.
- Find navigation starts near the selection for paragraph-local queries and
  wraps through bounded paragraph intervals in either direction. Each paragraph
  retains forward nonoverlapping matches and full Unicode lowercase context.
  Queries with paragraph breaks retain document-wide matching. Paragraph-local
  lowercase and whole-word searches avoid flattening the document.
- History entries store their delta directly to avoid a per-edit allocation;
  disabled history skips retention work. Newline normalization uses one copied
  buffer and is shared by import, insertion, and search queries.
- Release verification checks the complete package inventory before testing
  the extracted crate, catching omitted test, example, and benchmark directories.
- Renderer-independent documents, inline styles, fill colors, hyperlinks,
  headings, lists, block quotes, soft line breaks, directional selections,
  Unicode grapheme/word editing, and transient IME composition.
- Bounded undo/redo with grouped typing, rich clipboard fragments, literal
  search, grouped replace-all, and list indentation.
- Versioned TLFR persistence, written as version 2 and reading version 1, with
  bounded decoding and escaped semantic HTML.
- Optional egui widget, winit input bridge, and AccessKit accessibility adapters.
- Shared immutable paragraphs, lazy Unicode indexes, and cached egui layouts.
- Documented public API, non-exhaustive error enums, explicit API/file-format
  compatibility policy, and a reproducible release verification script.
- Full feature-combination checks, Rust 1.95 validation, cross-platform CI,
  and tag-gated package artifacts.
- A repository-only native eframe host demonstrates mixed-state formatting,
  headings/lists, search/replacement, snapshots, read-only mode, clipboard and
  IME integration. It has a separate lockfile and font/smoke-test options; native
  host compilation joins Ubuntu, Rust 1.95, macOS and Windows validation.
- Corrected invalid-position style lookup, composition cancellation and focus
  handling, macOS navigation and AltGr shortcuts, and accessibility updates.
- Rich Unicode import keeps navigation indexes lazy. HTML export replaces NUL
  with U+FFFD instead of emitting a character discarded by HTML parsers.
- Escape cancels the current egui composition before releasing focus; earlier
  cancellations in the same frame no longer prevent focus release.
- Empty winit IME reset events preserve selected text; an active composition's
  empty commit still replaces its captured selection.
- Empty egui preedit restores the visible document and selection while retaining
  the captured replacement for a later commit. Canceling native composition no
  longer leaves selected text hidden until the next input.
- Direct AccessKit trees keep distinct node IDs for repeated rich paragraphs
  that share the same underlying allocation.
- Release checks test and compile the extracted package, including its fixtures,
  examples, and benchmark targets.
- Empty plain clipboard pastes preserve selection and undo history. Double-clicking
  at the end of an egui paragraph selects its trailing word or symbol.
- ASCII word navigation caches boundaries lazily, literal searches reuse their
  searcher, and single-paragraph searches borrow text. Accessibility reuses
  grapheme indexes and character geometry and skips unchanged direct tree updates.
- Redo-history eviction and egui event removal use linear traversal instead of
  repeatedly shifting entries or scanning consumed event indices. Winit borrows
  unfiltered input and compares preedit state without a second clone.
- Ordinary literal searches borrow each paragraph instead of flattening a
  multi-paragraph document. Accessibility validates cached content with a
  shared identity token, indexes text run IDs, and reuses paragraph nodes for
  selection and label changes.
- Egui vertical movement preserves the intended horizontal column through
  short paragraphs, retains affinity at wrapped-row boundaries, and crosses
  graphemes split by the renderer without getting stuck. Native
  surrounding-text deletion visits only the paragraphs it crosses. Repeated
  winit preedit events avoid cloning prior text and skip unchanged updates.
- Egui retries font layouts whose character counts disagree with the source,
  preventing duplicated RTL/prepend glyphs and invalid cursor offsets while
  preserving paragraph text and formatting.

### Added

- `Editor::transaction()` records several commands as one undo step, merges
  successive edits to one paragraph, rolls back on error or panic, nests, and
  keeps an editor that the closure replaced. `Editor::mark_saved()` and `is_modified()` track
  unsaved changes by content, so undoing back to the saved text reads as
  unmodified.
- `Editor::replace_selected_match()` implements a Replace button with the
  per-match style of `replace_all()`. `select_word_at()`, `select_paragraph()`,
  and `Document::word_range_at()` implement double and triple click;
  `delete_to_paragraph_start()`, `selected_paragraphs()`,
  `selection_paragraph_kind()`, and `toggle_style(InlineAttribute)` serve
  adapters and toolbars.
- `Paragraph::new()` validates host-built rich paragraphs, and
  `Document::from_paragraphs()` assembles them. `Fragment::from(&Document)`,
  `Document::from(Fragment)`, `StylePatch::from(InlineStyle)`, and
  `StylePatch::from(SelectionStyle)` (a format painter) convert between types.
- `DecodeLimits`, `Fragment::from_bytes_with_limits()`, and
  `Document::from_bytes_with_limits()` reopen trusted snapshots beyond the
  unchanged default limits of 64 MiB and one million paragraphs and spans.
- `Error::InvalidGraphemePosition`, which
  `AccessibilitySnapshot::selection_from_graphemes()` returns instead of
  `InvalidPosition`.
- `AccessKitAdapter::invalidate()` republishes the complete tree when a platform
  adapter requests an initial tree or an update is discarded. `locate_text_run()`
  maps a run to its paragraph text for `ScrollIntoView` requests, and
  `AccessibilityOutcome::composition_canceled` reports discarded preedit.
- Egui: `RichTextEditorOutput::caret_rect` and `selection_rect`, computed while
  unfocused; `.highlight_ranges()` for search matches; `.heading_shortcuts()`;
  `RichTextEditor::forget_state()`; PageUp/PageDown; Ctrl/Command+] and [ list
  indentation; and a caret blink that follows egui's `text_cursor` visuals.
- Winit: `WinitAdapter::host_vertical_navigation()`, the Shift+Delete,
  Ctrl+Insert, and Shift+Insert clipboard aliases in Control mode, and dedicated
  Cut, Copy, Paste, Undo, and Redo keys.
- `Editor` implements `Clone` and `Debug`, `Selection` implements `Hash`, and
  the egui widget and its output implement `Debug`. `Editor`, `Document`,
  `Paragraph`, and `Fragment` are `Send + Sync`, checked at compile time.
  docs.rs pages label feature-gated items.
- Computed list numbering: `Document::list_number()` returns the number an
  ordered item displays, computed from paragraph kinds and cached until kinds or
  the paragraph count change. `Editor::apply_numbered_list(indent)` implements a
  Numbered List command as one undo step: it extends a list ending right before
  the selection or starts one at 1, and does nothing when every selected
  paragraph is already numbered at that level.
- Fill colors: `InlineStyle::background`, `StylePatch::background`, and
  `SelectionStyle::background` work like Excel's Fill Color, with `None` as
  "No Fill". `Color::STANDARD` and the `Color::DARK_RED` through
  `Color::PURPLE` constants provide Excel's ten standard colors for font and
  fill pickers. `clear_formatting()` removes fills, and
  `StylePatch::from(SelectionStyle)` copies them. HTML exports the fill as
  `background-color` on the color span; egui paints it behind the glyphs,
  replacing inline code's background, with selections and highlights visible
  over it; both accessibility bridges publish it as run background color.
- Soft line breaks: U+2028 LINE SEPARATOR is a line break within a paragraph.
  `Editor::insert_line_break()` and Shift+Enter in both adapters insert one in
  the typing style, keeping the paragraph kind and the typing undo step. HTML
  exports `<br>`, plus a placeholder `<br>` after a final soft break. Egui caret
  placement, hit testing, Home/End, Up/Down, IME preview, and selection painting
  follow its rows, and both accessibility bridges end a published line at each
  soft break. `Document::word_range_at()` treats it like a paragraph end.
- Hyperlinks: `Link` validates a target (`Link::MAX_LEN`, `Error::InvalidLink`,
  `FragmentError::InvalidLink`), and `Span::link` carries it. `Editor::set_link()`
  links, edits, or removes links as one undo step, `insert_link()` inserts linked
  text, `selected_link()` prefills a link field, and `Document::link_at()` and
  `link_range_at()` serve click handling. Typing extends a link only strictly
  inside it, rich paste and replacements keep links, and `clear_formatting()`
  preserves them. HTML shares one `<a href>` per link and exports `javascript:`,
  `vbscript:`, and `data:` targets as plain text. Egui underlines links in the
  hyperlink color unless explicitly colored, shows the target on hover with a
  pointing hand while a click would activate it, and reports Ctrl/Command+click,
  or a read-only click, in `RichTextEditorOutput::clicked_link` without opening
  it. Both accessibility bridges publish `Link` nodes whose `Click` action is
  reported through `AccessibilityOutcome::activated_link` or `clicked_link`;
  `AccessKitAdapter::locate_link()` maps a link node to its text.
- Block quotes: `ParagraphKind::Quote`. Enter continues a quote; Enter on an
  empty quote paragraph and Backspace at a quote paragraph's start turn it into
  body text as one undo step, and indentation commands and Tab leave quotes
  alone. HTML groups consecutive quote paragraphs into one `<blockquote>` with a
  `<p>` each. Egui indents them with a gutter bar that continues across
  consecutive quotes, and both accessibility bridges group them under one
  `Blockquote` node.
- TLFR version 2 adds paragraph kind tags 4 and 5 (ordered items that continue
  or restart their list) and 6 (block quote), and style bits 6 (fill) and 7
  (link). The decoder still reads version 1.
- Native host: Excel-style **Fill Color** and **Font Color** split buttons
  apply the last color chosen, initially Yellow and Red, and their menus offer
  "No Fill" or "Automatic" and the ten standard colors. **Link** and
  Ctrl/Command+K open a URL field prefilled from the selected link; applying it
  links the selection or edits the link at the caret, a caret elsewhere inserts
  the URL as linked text, and an empty URL unlinks. Editing or removing part of
  one link applies to the whole link. **Remove link** unlinks.
  Ctrl/Command+click, or a click in read-only mode, opens only http, https, and
  mailto targets. The paragraph-kind choice adds **Quote**, and a focused
  numbered item shows **Item N.** with **Restart at 1** and **Continue
  numbering**, each one undo step. The clipboard self-test fixture includes a
  Yellow fill.

### Changed

Library behavior changes relative to 0.1 come first, then host and CI changes.

- Lists number like a word processor's. `ParagraphKind::Ordered` stores an
  optional explicit start instead of its displayed number: `Some(n)` restarts
  at `n`, and `None` continues the nearest earlier ordered item at the same
  indent unless a heading or a shallower list item lies between them, showing
  1 otherwise. Body text, quotes, and same-level bullets interrupt a list
  without ending it, and deeper items are skipped. Edits never rewrite other
  paragraphs to renumber them, so converting an item to another kind and back
  restores its list exactly, and a heading placed mid-list restarts the
  following items at 1. When a deletion removes an item with an explicit start,
  the item that continued its list inherits that start. Indent and outdent keep
  starts, and `replace_all()` numbers lists exactly as step-by-step replacement
  does. Applying `Ordered { start, .. }` gives `start` to the first selected
  item and continues the rest.
- Ctrl/Command+Shift+7 uses `apply_numbered_list(0)`: it extends a list ending
  right before the selection and does nothing on items already numbered at
  level zero, instead of always restarting at 1.
- Enter on an empty list item outdents it one level instead of always leaving
  the list. Backspace at the start of an item outdents it, turning level zero
  into body text; a second Backspace joins paragraphs.
- Splitting a heading before its end keeps both parts headings. Typing over a
  selection that starts at a paragraph start and crosses a paragraph break takes
  the kind of the paragraph containing the selection end.
- Rich paste keeps the kinds and explicit starts of complete pasted
  paragraphs, so pasted continuations continue the destination list; surviving
  destination text keeps its kind. A fragment without a paragraph break adopts
  its source kind only when it fills an empty line or whole selected
  paragraphs. Empty fragments change nothing. An ordered item that begins a list
  and is the fragment's first item at its level is captured with start 1, so a
  copied list stays a new list. HTML of a captured range keeps the numbers its
  items showed, for example `<ol start="3">` for the third to fifth items,
  without changing its TLFR bytes. Fragment equality compares, and `Debug`
  shows, only paragraphs.
- Import and insertion treat U+2029 PARAGRAPH SEPARATOR as a paragraph break
  like LF, CRLF, and CR; `Paragraph::new()` and TLFR version 2 reject it in
  paragraph text. U+2028 is a soft line break: plain-text export,
  `Document::text()`, `selected_text()`, and `Fragment::plain_text()` write it
  as LF instead of verbatim. A search query's LF, CRLF, CR, or U+2029 matches
  only paragraph breaks, and its U+2028 matches only soft breaks.
- `to_bytes()` writes TLFR version 2. Version 1 snapshots still decode: a
  legacy ordered item becomes an explicit start only when its stored ordinal
  differs from the number it would show by continuation, so every item shows
  its stored ordinal, and U+2029 in version 1 text becomes a soft line break.
- Word movement and deletion stop at Unicode words and at runs of punctuation,
  symbols, or emoji; accessibility word starts use the same stops.
- Consecutive Backspace or Delete within a paragraph coalesce into one undo step,
  like typing; a deleted paragraph break is its own step. Typing over a
  selection continues as one step, and IME commits without paragraph breaks
  join adjacent typing instead of always forming their own step.
- Typing over a selection uses the style of its first selected grapheme in
  either direction; forward selections previously used the last. Moves that
  leave the selection unchanged keep a pending caret style.
- Case-insensitive search treats final sigma `ς` as `σ`.
- HTML export places the color `<span>` directly around `<u>` and `<s>`, inside
  any `<code>`, so decoration lines take the text color.
- `history_bytes()` charges the larger side of each change once, without counting
  text and runs both sides share, so formatting multi-megabyte documents stays
  undoable. `Debug` output of paragraphs and documents omits internal caches.
- Accessibility full-value replacement uses the style at the document start
  instead of the caret's typing style. The direct AccessKit adapter publishes
  graphemes longer than 255 bytes as several AccessKit characters instead of
  failing, and `AccessKitError::Editor` no longer repeats its message in error
  chains.
- Egui: a press places the caret and a primary-button drag extends from the
  press point; double and triple clicks select on release. Home/End and macOS
  Command+Left/Right use the visual row, and Ctrl/Command+Home/End move to the
  document start or end. Clicking, dragging, or moving focus away during IME
  composition commits the preedit after applying IME events queued earlier in
  the frame, and ignores one immediate repeat of that commit; Escape, read-only
  mode, and disabling cancel it. Touch swipes scroll until the editor has focus. `min_rows(n)` fits `n`
  single-row paragraphs. List geometry scales with the font, and ordered markers
  reserve three digits: at 16 pt, level-zero ordered text moves from 32 to
  39 pt. Bold spans always use the strong text color, and missing bold or
  monospace families fall back instead of panicking.
- Both adapters: shortcuts without Shift ignore chords with Shift held, which
  reach the host. Latin logical keys select shortcuts, other layouts use the
  physical US key, and the digit row uses its physical digit. Typed text drops
  control characters; paste and IME commits keep only line breaks and tabs among
  them. Egui read-only mode consumes only the chords editable mode handles.
- Winit: Ctrl+Alt chords type text only on Windows (AltGr). Modifiers survive
  widget blur. `sync_ime` sends only changes, allows IME after focus gain or
  `Ime::Disabled`, releases it once after blur or read-only mode, and
  interrupts preedits the editor dropped.
- The `egui` feature no longer enables `egui/default_fonts`; eframe enables it by
  default. The minimum AccessKit version is 0.24.1.
- Native host: toolbar and search commands commit an active preedit instead of
  failing with `CompositionActive`. The paragraph-kind choice keeps indentation
  and numbering and shows the kind of a paragraph that commands change; its
  "Numbered list" uses `apply_numbered_list()`, as Ctrl/Command+Shift+7 does.
  The rich clipboard preflights the exact TLFR version 2 size.
  `--qa-report` claims its path at startup and exits with status 2 if the file
  exists or cannot be created. Linux clipboard helpers exit when the host dies.
  The host builds on other Unix targets, which reject `--rich-clipboard`, and
  uses the public `DecodeLimits::DEFAULT`.
- CI adds cargo-deny checks, with bans, licenses, and sources as a required
  check, and a build with minimal direct dependency versions. macOS and Windows
  run strict Clippy, only pull requests cancel superseded runs, and benchmark
  smoke runs use `TEXTLOOM_BENCH_QUICK=1`. `scripts/release-version.sh` gives
  local and tag release checks one version gate, which also requires this entry
  to record its release date.

### Fixed

- Egui: IME candidate windows follow the caret through a caret-local IME area.
  Drags anchor at the press point. Space and Enter no longer act as clicks that
  refocus or interrupt IME. Bare IME commits group like typing, and IME text
  consisting of one line break no longer duplicates Enter. The caret stays
  visible during an empty preedit, selection is visible over inline code, the
  Tab focus filter no longer goes stale, read-only mode no longer swallows host
  shortcuts, and hint text uses the first paragraph's layout. A click whose
  press and release arrive in the same frame, such as a touchpad tap, places
  the caret, commits a preedit, and activates links instead of being ignored.
- Egui accessibility: oversized graphemes no longer fail every frame. Runs
  publish word starts and stay within 255 characters including the synthetic
  line break, so AccessKit word navigation no longer wraps to the run start.
  List items publish their position and set size, following the numbering
  rule: headings close every set, shallower items close deeper sets, body text,
  quotes, and the other list kind at the same level interrupt a set without
  closing it, and an explicit start begins a new set. At wrapped-row seams the
  published caret follows the painted caret's affinity.
- Winit: typing works at macOS startup despite winit 0.30's spurious
  `Focused(false)` (winit #4371). `sync_ime` sends the caret area without
  waiting for `Ime::Enabled`, so candidate windows follow the caret from the
  first composition. Shortcuts work on non-Latin, AZERTY, and German layouts.

### Performance

Timings were measured informally during development; see `BENCHMARKS.md`.

- Mixed-style typing in a 460 KB Unicode paragraph dropped from about 5.5 ms to
  20 µs per keystroke. Enter at a paragraph end reuses the paragraph: Enter and
  undo in a huge paragraph dropped from 5.1 ms to 0.3 µs. `scalar_count()`
  builds no index (2.4 MB of CJK text: 2.4 to 0.14 ms), index storage is
  allocated lazily, and history per Enter in a 10,000-item list fell from 2.0 MB
  to 0.64 MB.
- Direct AccessKit structural edits rebuild only replaced paragraphs (Enter at
  100,000 paragraphs: 26.5 to 0.27 ms), with lazy position lookups, no
  per-grapheme offset tables, and no segmentation for ASCII paragraphs. Long
  non-ASCII paragraphs now pay word segmentation for word starts, about twice
  the previous per-keystroke cost at 10–200 KB.
- Egui no longer relays out the document on other viewports' passes or hidden
  frames, which took up to about 294 ms at 2,000 paragraphs. Accessibility
  geometry is linear (40,000 characters: 68.5 to 3.5 ms), node IDs stay stable
  across structural edits (Enter at the top of 2,000 paragraphs: 34 to
  0.78 ms), and renumbered list items reuse shaped text and accessibility runs
  (Enter in a 10,000-item list: 4.2 to 2.9 ms of relayout). Layout builds no
  scalar indexes, and the first preedit no longer reshapes the whole document.
  Idle frames request visible galleys again so font-atlas changes cannot leave
  stale texture coordinates: 10,000-paragraph idle frames rose from 7.6 to
  11.0 µs, and long-preedit idle frames from 6.7 to 20 µs.
- List edits no longer rewrite following items to renumber them. Displayed
  numbers are computed in one pass after a change to paragraph kinds or count
  and cached, and egui rebuilds only markers whose number changed, reusing the
  shaped text. With links supported, typing in heavily formatted paragraphs
  still takes about 4–5 µs per keystroke with 1,000 style runs and 76–95 µs
  with 20,000.

### Migration from 0.1

Update the dependency to `textloom = "1.0"`.

- Add a fallback arm when matching the error enums, `ParagraphKind`, `Movement`,
  `InlineAttribute`, `AccessibleAction`, or winit `ClipboardEvent`.
  `Composition`, `DecodeLimits`, `AccessibleParagraph`, `AccessibilityOutcome`,
  `RichTextEditorOutput`, and winit `InputOutcome` cannot be built with struct
  literals. Read accessibility snapshot state using its accessor methods.
- `AccessKitError::CharacterTooLong` is removed because oversized graphemes are
  published instead.
- Pass the physical key to `WinitAdapter::handle_key`: `event.physical_key`, or
  `PhysicalKey::Unidentified(NativeKeyCode::Unidentified)` when unknown.
  `handle_window_event` does this automatically.
- `WinitAdapter::sync_ime` takes `&mut self` and the editor:
  `sync_ime(&window, &editor, position, size)`. Call it every frame while the
  editor is focused and at least once after it blurs.
- `InputOutcome` is `#[must_use]`, as are the egui widget's builder methods.
  During a nonempty preedit, winit `paste` and `paste_fragment` return a handled,
  unchanged outcome instead of `Err(CompositionActive)`.
- Egui applications that do not use eframe and rely on egui's bundled fonts
  enable `egui/default_fonts` themselves.
- `ParagraphKind::Ordered { indent, start }` takes `start: Option<u32>`:
  `Some(n)` restarts at `n` and `None` continues the list, instead of storing
  the displayed number. Read displayed numbers with `Document::list_number()`,
  and start a new list with `Editor::apply_numbered_list()` or `Some(1)`.
  `ParagraphKind` also gained `Quote`.
- Add `background` to `InlineStyle` and `StylePatch` struct literals, or use
  `..Default::default()`, and `link` to `Span` literals and patterns.
  `AccessibilityOutcome` no longer implements `Copy` because it can carry the
  activated `Link`. `Error` and `FragmentError` gained `InvalidLink`.
- `to_bytes()` writes TLFR version 2, which Textloom 0.1 cannot decode; 1.0
  still reads version 1 snapshots, turning U+2029 in their text into soft line
  breaks. Imported U+2029 starts a new paragraph, and plain-text export writes
  U+2028 soft line breaks as LF.
- Review the behavior changes above, notably list numbering and
  Enter/Backspace, undo grouping, typing style over selections, and HTML
  element order.

Unicode segmentation is pinned to preserve persisted span-boundary semantics.
