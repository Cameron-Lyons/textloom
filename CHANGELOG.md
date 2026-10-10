# Changelog

## 1.0.0

Unreleased. Native host checks and publication steps are tracked in `RELEASING.md`.

First stable release of the native Rust rich-text editing library.

- Internal document/Unicode, history/IME, and egui layout/input/navigation/painting
  responsibilities are separated without changing the public API or TLFR v1.
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
  explicitly declares UTF-8 so TextEdit preserves Unicode; library HTML fragments
  and the TLFR format remain unchanged. Physical desktop signoffs remain required.
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
- Renderer-independent documents, inline styles, headings, lists, directional
  selections, Unicode grapheme/word editing, and transient IME composition.
- Bounded undo/redo with grouped typing, rich clipboard fragments, literal
  search, grouped replace-all, and list indentation.
- Versioned TLFR v1 persistence with bounded decoding and escaped semantic HTML.
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

Migration from 0.1: update the dependency to `textloom = "1.0"` and add a fallback
arm to exhaustive matches on error enums. Read accessibility snapshot state using
its accessor methods. The TLFR v1 encoding remains unchanged. Unicode
segmentation is pinned to preserve persisted span-boundary semantics.
