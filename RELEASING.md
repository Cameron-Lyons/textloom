# Releasing Textloom

## 1.0 validation status

Clean implementation candidate `5f8ccfbb83279d962f8c5a16d6b9d80673bd88e8`
passed all six [release CI jobs](https://github.com/Cameron-Lyons/textloom/actions/runs/37931457171)
and both [hosted interaction jobs](https://github.com/Cameron-Lyons/textloom/actions/runs/37931457145)
on 2026-10-09. All reports and checkout logs identify the checked-out PR merge
`7eb9964451cb9d10b9d16c2dbfee97cd08af1588`. Publication still requires the human
signoffs below; the maintainer confirmed that test desktops are not available yet.

The library passes 223 unit/integration tests and four doctests with all features;
one egui host snippet is intentionally ignored. Rust 1.95.0 checks all eight
feature combinations. Stable Rust 1.99.0 checks formatting, strict Clippy,
warning-free API documentation, benchmarks, headless examples, package inventory,
extracted-package tests/targets, and the standalone native host against the
extracted library. Native host tests pass 20 cases on Linux, 15 on macOS, and 19
on Windows. Nine Python regressions reject incomplete or content-bearing report
evidence. A local complete release check and publication dry run previously
passed on clean `f05a518`, without upload; repeat publication checks on the final
clean commit.

All nine Linux X11/macOS/Windows renderer mode reports independently validated
21 frames, graceful exit, zero errors, clipboard request/completion flags, equal
TLFR round trips, and disabled focus suppression. Editable widget clipboard
roundtrip/undo/redo markers passed on all platforms. Runners used Ubuntu 24.04.5,
macOS 26.6.2 ARM64, and Windows 10.0.26100 with signed MSYS2 Mesa 26.2.4-1 and
LLVM 22.1.8-3. Linux uses private Xvfb; Windows uses application-local OpenGL DLLs.
Earlier private Sway 1.12/wlroots 0.20/pixman checks also passed all three modes,
Wayland MIME aliases, multi-megabyte transfers, and owner replacement without
changing the current desktop.

Hosted interaction results on clean `5f8ccfb`:

- macOS TextEdit preserved both Unicode paragraphs, bold/italic, ordered-list
  start 4, and exact RGBA `[42, 100, 200, 255]` after native HTML paste and RTF
  copy. AppKit default and explicit UTF-8 decoding agreed. The host declares
  UTF-8 in its macOS clipboard HTML document; without metadata, TextEdit had
  interpreted the UTF-8 bytes as legacy text. The library fragment exporter,
  plain text, and TLFR remain unchanged. External rich TextEdit input correctly
  used plain fallback. Native replacement undo/redo, focus-separated typing
  groups, read-only copy/cut and editing suppression, and disabled guards passed.
  All three reports had zero errors, graceful exit, and equal TLFR round trips;
  all native processes and the owned TextEdit process were reaped.
- Windows NVDA 2026.2 passed all 15 probe checks: ordinary document announcement,
  Unicode line/selection speech, UIA text/caret/selection exposure, native
  replacement, undo/redo and exact final restoration, editor close with NVDA
  still active, report validation, and reader exit code 0. Eleven actual
  reader-generated speech events were captured from the synthesis queue using
  its silent synthesizer. This proves generated speech, not audible speech
  usability. The probe verifies the real editor window: .NET's cached main
  window had selected winit's hidden thread tool window in earlier failed
  trials. Correct window targeting resolved closure without a host shutdown
  change. No owned processes remained.

The separate native interaction workflow archives bounded public fixture
captures alongside strict content-free host reports. It refuses ordinary desktop
invocation and never certifies manual signoff. The release workflow requires both
interaction jobs and the complete CI matrix before archiving a release package.
See the repository's `scripts/native-qa/README.md` for probe scope and the
`examples/native-editor/NATIVE_QA.md` protocol for physical desktop signoff.

The 1.0.0 working tree is an unreleased candidate in
[draft PR #1](https://github.com/Cameron-Lyons/textloom/pull/1). Linux Wayland rendering has
passed at 200% display scale with optional Noto CJK and Liberation Bold fonts.
Native ASCII typing and plain-text copy passed with exact clipboard content
verified. Native Unicode paste/copy, keyboard undo/redo, grouped adjacent caret
typing, and formatting shortcuts passed. Bold/italic/underline toolbar states
and rendered styles were checked before and after undo. Read-only selection/copy,
typing/deletion/paste suppression, and non-destructive cut passed. The isolated
Linux IME, Orca, and Qt interaction evidence below supplements these observations.
Physical dead keys/AltGr and complete macOS and Windows IME, screen-reader, and
external-application interaction checks still require signoff.
Linux keyboard checks on clean `f05a5187dc6ffe4a3d5396658c8fc3c10875f331`
used a persistent German XKB keymap in a private Sway session. Native virtual
AltGr+Q and AltGr+E produced visible `@€`, independently copied as exact UTF-8
bytes `40e282ac`. Dead acute followed by `e`, and Compose apostrophe followed
by `e`, each produced visible `é`, copied as `c3a9`. Undo restored the complete
styled fixture and selection: two entries for AltGr, one for each composed
character. All three sessions retained native focus, reported zero errors, and
left no isolated processes running. These checks supersede the earlier
inconclusive virtual AltGr attempt. Physical-keyboard signoff remains pending.

Additional Linux interaction checks on 2026-10-09 used builds explicitly labelled
`3e39b17-native-integration-dirty`, `3e39b17-native-integration-v2-dirty`, and
`3e39b17-native-integration-v3-dirty`/`3e39b17-native-integration-v4-dirty`.
They used private headless Wayland
Sway 1.12/wlroots 0.20/pixman sessions or isolated Xvfb displays, private D-Bus
buses and XDG directories, and virtual input. They did not modify the current
desktop's clipboard, configuration, or audio devices. No private-session process
remained after close. Repeat the interaction checks on the publication commit
with the intended desktop and physical keyboard.

- Real Fcitx 5.1.23 with Japanese Mozc 3.34.6239.2 and Chinese Pinyin 5.1.15
  (LibIME 1.1.17) passed preedit, native candidate navigation, full-selection
  replacement, undo/redo, and resumed plain input. Candidate popups followed the
  caret at the top of a replacement and after movement into a Unicode list
  paragraph. Focus loss during preedit canceled without changing content or
  history; read-only and disabled modes suppressed editing. Find committed the
  Japanese/Chinese query without changing document selection, content, or history.
  Retiring processed IME events stopped the host's unchanged cursor-area feedback
  loop: comparable search sessions reduced Japanese preedit events from 950 to 17
  and rendered frames from 964 to 126, and Chinese events from 891 to 19 and
  frames from 900 to 76.
  The final empty-preview check verified that canceling native preedit over a
  full selection immediately restores visible source text and selection while
  retaining the replacement for a later commit. Resumed plain typing and undo
  restored the exact original document and directional selection.
- Real Orca 51.0 with AT-SPI 2.62.0.1, Speech Dispatcher 0.12.1, and espeak-ng
  1.52.0 passed ordinary document-focus narration, Unicode document reading,
  selection and caret requests, and native typing over the accessible selection.
  One-character replacement, undo, redo, and final undo restored the exact
  fixture. Speech logs and synthesized nonzero PCM captured the document and
  selection announcements through a private file sink. The example's compatible
  [AccessKit backports](examples/native-editor/vendor/accesskit_unix/BACKPORT.md)
  fix current AT-SPI activation and the application's desktop parent.
  This Unix adapter exposes `Text` but does not expose `EditableText`; direct
  AT-SPI text replacement is unsupported and was not certified.
- A separate Qt 6.12 text editor received the native HTML/plain offer with exact
  Unicode multiline text, bold/italic emphasis, list structure, and foreground
  RGBA preserved. Its external HTML/plain offer without TLFR used the host's plain
  fallback, with exact copied text and one-step undo/redo. Qt's HTML parser also
  preserved every alpha byte in a 256-case color probe. The exporter uses
  six-digit opaque colors and `rgba()` with enough precision to support both
  rounding and truncating alpha consumers.

All checked reports had zero editing/accessibility/host/clipboard errors and
equal TLFR round trips. These reports remain content-free and record
`manual_signoff: "not_recorded"`; virtual input and captured synthesized speech
do not establish complete physical-keyboard or platform manual signoff.

Before publication, record the final commit, native host and version, operating
system and display backend, IMEs and screen readers used, and the outcome of each
native check below. Linux, macOS, and Windows host results and the CI matrix on
the final clean commit remain required. Publish only after these checks pass.

## Native host validation record

The standalone host in `examples/native-editor` uses eframe/egui 0.36.2 and
winit 0.30.13. It is available from a repository checkout and is intentionally
excluded from the published library archive. Its renderer, window, clipboard,
and platform accessibility dependencies remain outside the library's dependency
graph. The example transports plain text through the system clipboard by default.
`RichTextEditor::rich_clipboard()` now accepts a host-owned `RichClipboard`
callback: successful rich copies include a plain alternative and suppress egui's
later plain overwrite; rich paste replaces the selection in one undo step, with
plain fallback when the callback returns no fragment. The callbacks preserve
input order and focus, read-only/disabled, empty-paste, and IME guards.

`--rich-clipboard` opts into the example's native Linux Wayland/X11, macOS, and
Windows backends. They publish plain text, native HTML, and a length-framed TLFR
payload with a per-publication generation token. Linux/Windows register
`application/x-textloom-fragment`; macOS maps the rich format to
`org.textloom.fragment`. Windows HTML uses CF_HTML. The token distinguishes a
new offer during copy verification and does not alter TLFR v1 or establish trust.
Native access runs in helper processes with hard deadlines and bounded transfers;
native lengths are checked before Rust payload allocation. TLFR retains its
64 MiB limit, with a 32-byte transport envelope allowance. Copy verifies all
representations through read-back before suppressing the plain fallback.

Paste captures one native item and validates its rich data and plain alternative.
Ambiguous multiple events, empty events, unavailable/malformed rich data, and
mismatched text retain the paste event's plain alternative. HTML is exported for
other applications; arbitrary HTML import remains outside the example's scope.
The host retains one verified owner helper, reaps replaced or stalled children,
joins pipe workers, and retires its owner on close. Linux clipboard data remains
available after close only if a clipboard manager retains it; macOS/Windows use
eager native data. All platform dependencies are host-only safe wrapper APIs,
and unsafe code remains forbidden in the example. Automated native fixtures pass
on all three platforms and Linux Wayland. Linux Qt interoperability passed in the
isolated sessions above; hosted macOS TextEdit interoperability and Windows NVDA
interaction also passed. Remaining desktop signoffs are still required.

Reproduce the Linux host used for rendering checks from the repository root:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --font /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc \
  --bold-font /usr/share/fonts/liberation/LiberationSans-Bold.ttf
```

These optional font paths require the corresponding local font files; the host
does not install or change system fonts. Add `--smoke-test` to request a graceful
close after 20 native frames, `--read-only` for selection/copy checks with editing
suppressed, or `--disabled` to disable document focus and interaction. The toolbar
can toggle enabled and read-only modes during a session.
The smoke mode fails if the widget reports an editing or accessibility error.
Add `--rich-clipboard --clipboard-self-test` only in an isolated session: the
self-test writes a deterministic fixture to the system clipboard and checks
rich/HTML/plain read-back, rich insertion, and one-step undo/redo. It prints
`Native rich clipboard roundtrip and undo/redo passed` only on success and fails
the run on an error. Native IME, external clipboard interoperability, and
screen-reader behavior still need separate interaction tests.

Use `--qa-report PATH` to capture opt-in JSON observations on graceful exit.
Choose a new path with an existing parent directory; reports do not overwrite
existing files, and write failures return a nonzero exit status. Reports include
platform/scale information, host-wide input counts, observed editor transitions,
error counts, final-state metrics, and an in-memory TLFR round trip. Document,
clipboard, search and preedit text are omitted. Native window focus and retained
widget focus are recorded separately to diagnose focus loss. Counters describe
observed input and state changes; they do not prove document acceptance or a
manual check's outcome. Every report records `manual_signoff: "not_recorded"`.
Session fields `rich_clipboard_requested`, `clipboard_self_test_requested`, and
`clipboard_self_test_passed` describe the opt-in automated fixture;
`errors.clipboard_self_test` records a failed requested check. An unrun check
remains unpassed. Clipboard contents are omitted even when running the fixture.
Supply `TEXTLOOM_QA_REVISION` at build time to label the source revision, and label
builds with local changes accordingly. The native example guide describes
platform commands. Attach exact steps, IME/screen-reader versions, OS/backend,
and outcomes separately for final-commit signoff.

Partial QA record for 2026-10-09: clean implementation candidate `5f8ccfb`;
earlier desktop observations used an uncommitted candidate on Omarchy
4.0.0.r6815.g50d687a (Arch-based Linux), kernel 7.2.8-5-omarchy-bore,
Hyprland/Wayland, 200% display scale. The isolated Linux sessions above add IME,
reader, and external clipboard evidence. Final commit and complete platform
signoff remain pending.

| Check | Result |
| --- | --- |
| Native window rendering at 200% scale with Noto CJK and Liberation Bold | Passed |
| Native ASCII typing and plain-text copy | Passed; exact clipboard content verified |
| Unicode clipboard paste/copy | Passed; exact UTF-8 content with Japanese, combining marks, emoji sequences, flag, and paragraph break verified |
| Native keyboard/paste undo and redo | Passed; document restored exactly, adjacent caret typing undoes in one step |
| Native formatting shortcuts | Passed; bold/italic/underline and their undo verified in toolbar and rendered text |
| Native read-only selection/copy | Passed; exact clipboard content verified |
| Native read-only editing suppression | Passed; typing, Backspace/Delete, Enter, paste, and undo preserve the document; cut copies without deleting |
| Japanese/Chinese IME preedit, replacement, focus loss, and candidate placement | Real Linux Mozc/Pinyin sessions passed with virtual input; physical-keyboard and macOS/Windows signoff pending |
| Dead keys and Compose | German XKB virtual sequences on clean `f05a518` produced visible `é`, exact copied bytes, and complete undo restoration; physical-keyboard/platform signoff pending |
| AltGr | German XKB virtual AltGr on clean `f05a518` produced visible `@€`, exact copied bytes, and complete undo restoration without focus loss; physical-keyboard/platform signoff pending |
| Screen-reader selection and replacement | Linux Orca and hosted Windows NVDA generated speech, accessible selection/caret, native replacement and undo/redo passed; direct AT-SPI EditableText unavailable; VoiceOver, audible Windows speech, and full manual signoff pending |
| Rich clipboard MIME transport | Widget fixtures passed on all platforms; Linux Qt and macOS TextEdit interoperability passed; Windows external applications and full manual signoff pending |
| macOS and Windows native host checks | Hosted macOS native editing/focus/guard checks and Windows NVDA interaction passed; physical keyboards, IMEs, VoiceOver, scale changes, and full manual signoff pending |

## Compatibility contract

Textloom 1.x follows semantic versioning for the documented public API and feature
names. Existing default builds remain independent of GUI libraries. Optional
adapters expose types from egui 0.36, winit 0.30, and AccessKit 0.24; an upstream
upgrade that breaks those exposed types requires a Textloom major release.
Error enums are non-exhaustive: callers must retain a fallback match arm.
Exact error messages, private implementation details, cache layouts, and benchmark
timings are not API guarantees.

Rust 1.95 is the minimum supported compiler for 1.0. The locked dependency graph
is tested on that compiler. Any future minimum-version increase must be announced
in a minor or major release, never a patch release. Review dependency updates
against the minimum compiler and all eight feature combinations.

TLFR format version 1 is independent of the crate version. Its signature, fixed
integer widths, tag meanings, style bits, and decoded semantics are stable.
Future readers must continue to accept valid v1 snapshots within the documented
64 MiB, one-million-paragraph, and one-million-span decoding limits. In-memory
documents can exceed these limits; their encoded snapshots will not be accepted
by the bounded decoder. The format stores document content, not selection, undo
history, or an active composition.

Grapheme boundaries are part of the format contract. Textloom pins
unicode-segmentation 1.13.3 (Unicode 17.0) so dependency resolution cannot silently
change valid span boundaries. Audit any future segmentation update for old-file
compatibility; introduce a format migration if old boundaries become invalid.
The independent v1 fixtures in `tests/fixtures/` must keep decoding and encoding
to the same bytes. Keep malformed-input regression tests when changing the codec.

## Prepare and verify

1. Update `Cargo.toml`, dependency examples in `README.md`, and `CHANGELOG.md`.
   Refresh the package entry in `Cargo.lock` with `cargo check --offline`.
2. Run `./scripts/check-release.sh --allow-dirty --tag v1.0.0` while reviewing
   pending changes. This requires a matching changelog entry and checks formatting,
   every feature combination, doctests, Clippy, documentation, benchmarks, and
   headless examples. It also checks formatting and strict Clippy for the native
   host and its CLI/report/clipboard regression tests without opening a window.
   It then runs all-feature tests and compiles
   every target from the extracted Cargo package. An explicit inventory check
   requires every packaged source, test, fixture, headless example, benchmark,
   and release document before those checks, so accidentally omitting an entire
   target directory cannot silently pass. A temporary copy of the repository's
   native host is also compiled against the extracted library with its unchanged
   lockfile; this leaves the verified crate untouched.
3. Review `cargo package --locked --list --allow-dirty`. Source, headless examples,
   tests, fixtures, benchmark documentation, changelog, and license must be
   included. The repository-only native host, workflows, and local build output
   must be absent. Include patterns are anchored to the package root so nested
   QA README/license files are excluded. The release script rejects repository-only
   directories in the extracted crate as well as requiring the intended inventory.
4. Commit the release changes, then run `./scripts/check-release.sh --tag v1.0.0`
   on the clean checkout. Run `cargo +1.95.0 test --locked --all-features` and
   `cargo +1.95.0 check --locked --all-targets --manifest-path examples/native-editor/Cargo.toml --target-dir target`.
   Confirm the MSRV feature checks and macOS/Windows jobs pass on that commit;
   they compile and test the standalone native host as well as the library targets.
   Confirm required Linux X11, macOS, and Windows Mesa renderer smoke checks pass
   in editable, read-only, and disabled modes with rich clipboard enabled, plus
   editable-mode clipboard fixture read-back and undo/redo. Validate the fresh
   content-free reports against the run's source revision; pull-request reports
   identify the checked-out merge revision. Native platform interaction signoff
   remains required independently. Confirm the hosted macOS TextEdit and Windows
   NVDA interaction jobs also pass, with strict interaction report validation and
   owned-process cleanup. They do not certify audible speech or physical input.
5. Exercise a native host on Linux, macOS, and Windows. Check focus loss during
   composition, Japanese/Chinese IME replacement and candidate placement,
   physical-keyboard dead keys and AltGr, platform shortcuts, plain/rich clipboard
   transport and external-application HTML/plain interoperability,
   read-only/disabled widgets, high DPI, and screen-reader selection/replacement.
   Headless tests validate routing and semantics; hosts supply these platform
   services. Record the host/platform results in the release notes. Any failed
   host behavior claimed by this release is a release blocker. Use the native
   signoff protocol in `examples/native-editor/NATIVE_QA.md` and record unavailable
   checks explicitly. Test desktops are currently unavailable, so these human
   gates remain open and the candidate remains unreleased.

## Publish

Publishing and pushing the tag are explicit maintainer actions. The check script
and CI never publish to crates.io or create a GitHub release.

1. On the verified clean commit, run `cargo publish --locked --all-features --dry-run`.
2. With the crate owner's credentials configured, run
   `cargo publish --locked --all-features` and verify the crate and docs.rs page.
3. Create and push an annotated tag for the same commit:

   ```sh
   git tag -a v1.0.0 -m 'Textloom 1.0.0'
   git push origin v1.0.0
   ```

4. The tag workflow runs the complete CI matrix and hosted native interaction
   probes, rejects a version/tag mismatch, and uploads the verified `.crate` archive and its SHA-256 checksum. Confirm it
   passes before creating a GitHub release using the matching changelog entry.

Crates.io versions cannot be overwritten. Fix a published defect in a new patch
release; consider yanking a defective version where appropriate. Do not move an
existing release tag.
