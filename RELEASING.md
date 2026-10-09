# Releasing Textloom

## 1.0 validation status

Linux validation of commit `d697471` on 2026-10-09 passed all eight test/doctest feature
combinations with Rust 1.99.0 stable and Rust 1.95.0. The current library's
all-feature suite passes 221 unit and integration tests and four doctests,
including the compile-fail accessibility API check: 225 passing tests in total.
One egui host widget snippet is intentionally ignored. The standalone native
host CLI/report tests passed, including on Rust 1.95.0. The current Linux host
has 20 passing option/report/clipboard tests on stable; Rust 1.95 passed the same
20 tests before the latest fixture callback extension. Strict stable Clippy
passed after that extension. Final checks on the publication commit remain
required.

The complete stable-toolchain release script passed on clean commit `d697471`:
formatting, strict Clippy, warning-free API documentation, benchmark smoke
tests, headless examples, package inventory verification, and tests and all-target
compilation from the extracted crate. Native host formatting, strict Clippy,
and compilation against both the source and extracted library also passed.
The crates.io publish dry run and archive verification also passed on that
commit, without uploading a release. Repeat the complete checks on the final
publication commit after the new clipboard changes.

All six required jobs in the [CI run for commit `d697471`](https://github.com/Cameron-Lyons/textloom/actions/runs/37916826884)
passed on 2026-10-09: Linux release checks, all Rust 1.95 feature combinations,
macOS/Windows compilation and tests, Linux X11 rendering, and Windows Mesa
rendering. Native renderer checks passed editable, read-only, and disabled modes
on all three platforms. Linux uses an isolated Xvfb display with Mesa and the
X11 xkbcommon runtime. Windows uses signed MSYS2 Mesa packages with
application-local OpenGL DLLs; the verified run used Mesa 26.2.4-1 and LLVM
22.1.8-3. Reports independently validated zero errors and equal TLFR round trips.
This is the baseline before the current opt-in rich clipboard changes. An
isolated Linux X11/Xvfb run of the new callback-based rich fixture passed locally
on `d697471-dirty`: native rich/HTML/plain roundtrip, one-step undo/redo, 21 frames,
zero errors, and an accepted content-free report. Rich-enabled read-only and
disabled runs also passed locally before that fixture callback extension.
Wayland, macOS, and Windows rich runtime checks and final clean CI remain pending.

Renderer jobs now request `--rich-clipboard` in every mode and require
`--clipboard-self-test` in editable mode on isolated CI desktops. They check the
exact native clipboard roundtrip/undo/redo completion marker and validate
source revision, platform, mode, graceful completion, clipboard request/result
flags, error counts, and snapshot integrity before archiving content-free JSON
reports alongside smoke logs. Six validator regressions pass locally. These
observations never certify manual signoff; interactive platform checks below
remain required on Linux, macOS, and Windows.

The 1.0.0 working tree is an unreleased candidate in
[draft PR #1](https://github.com/Cameron-Lyons/textloom/pull/1). Linux Wayland rendering has
passed at 200% display scale with optional Noto CJK and Liberation Bold fonts.
Native ASCII typing and plain-text copy passed with exact clipboard content
verified. Native Unicode paste/copy, keyboard undo/redo, grouped adjacent caret
typing, and formatting shortcuts passed. Bold/italic/underline toolbar states
and rendered styles were checked before and after undo. Read-only selection/copy,
typing/deletion/paste suppression, and non-destructive cut passed. IME candidate
placement, screen-reader behavior, dead keys/AltGr, rich clipboard runtime and
external-application interoperability, and macOS and Windows native interaction
checks still require signoff.
Linux virtual-keyboard `dead_acute` followed by `e`, and Compose apostrophe
followed by `e`, produced a visually observed `é` with undo. These exploratory
results leave physical-keyboard signoff pending. An isolated AltGr virtual-keyboard
attempt recorded a text event and a document change before compositor focus moved
away from the native host. The report recorded native window focus loss with
widget keyboard focus retained and no editing/accessibility errors. Visual,
undo, and physical-keyboard AltGr signoff remain pending.

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
and unsafe code remains forbidden in the example. Beyond the local X11 fixture,
native runtime checks and external-application interoperability signoff are pending.

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

Partial QA record for 2026-10-09: uncommitted 1.0.0 candidate; Omarchy
4.0.0.r6815.g50d687a (Arch-based Linux), kernel 7.2.8-5-omarchy-bore,
Hyprland/Wayland, 200% display scale. Final commit and complete platform signoff
remain pending.

| Check | Result |
| --- | --- |
| Native window rendering at 200% scale with Noto CJK and Liberation Bold | Passed |
| Native ASCII typing and plain-text copy | Passed; exact clipboard content verified |
| Unicode clipboard paste/copy | Passed; exact UTF-8 content with Japanese, combining marks, emoji sequences, flag, and paragraph break verified |
| Native keyboard/paste undo and redo | Passed; document restored exactly, adjacent caret typing undoes in one step |
| Native formatting shortcuts | Passed; bold/italic/underline and their undo verified in toolbar and rendered text |
| Native read-only selection/copy | Passed; exact clipboard content verified |
| Native read-only editing suppression | Passed; typing, Backspace/Delete, Enter, paste, and undo preserve the document; cut copies without deleting |
| Japanese/Chinese IME preedit, replacement, focus loss, and candidate placement | Manual validation pending |
| Dead keys and Compose | Virtual `dead_acute`/Compose sequences produced visually observed `é` with undo; physical-keyboard/platform signoff pending |
| AltGr | Virtual-keyboard attempt recorded text/document changes before compositor focus loss; visual, undo, and physical-keyboard/platform signoff pending |
| Screen-reader selection and replacement | Manual validation pending |
| Rich clipboard MIME transport | Isolated Linux X11 fixture passed locally; remaining native runtime and external-application interoperability signoff pending |
| macOS and Windows native host checks | Manual validation pending |

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
   must be absent.
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
   remains required independently.
5. Exercise a native host on Linux, macOS, and Windows. Check focus loss during
   composition, Japanese/Chinese IME replacement and candidate placement,
   physical-keyboard dead keys and AltGr, platform shortcuts, plain/rich clipboard
   transport and external-application HTML/plain interoperability,
   read-only/disabled widgets, high DPI, and screen-reader selection/replacement.
   Headless tests validate routing and semantics; hosts supply these platform
   services. Record the host/platform results in the release notes. Any failed
   host behavior claimed by this release is a release blocker.

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

4. The tag workflow runs the complete CI matrix, rejects a version/tag mismatch,
   and uploads the verified `.crate` archive and its SHA-256 checksum. Confirm it
   passes before creating a GitHub release using the matching changelog entry.

Crates.io versions cannot be overwritten. Fix a published defect in a new patch
release; consider yanking a defective version where appropriate. Do not move an
existing release tag.
