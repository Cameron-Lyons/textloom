# Changelog

## 1.0.0

Unreleased. Native host validation is still pending; see `RELEASING.md`.

First stable release of the native Rust rich-text editing library.

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
- Corrected invalid-position style lookup, composition cancellation and focus
  handling, macOS navigation and AltGr shortcuts, and accessibility updates.
- Rich Unicode import keeps navigation indexes lazy. HTML export replaces NUL
  with U+FFFD instead of emitting a character discarded by HTML parsers.
- Escape cancels the current egui composition before releasing focus; earlier
  cancellations in the same frame no longer prevent focus release.
- Empty winit IME reset events preserve selected text; an active composition's
  empty commit still replaces its captured selection.
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
