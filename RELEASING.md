# Releasing Textloom

## 1.0 validation status

Local Linux validation on 2026-10-09 passed all eight test/doctest feature
combinations with Rust 1.99.0 stable and Rust 1.95.0. The all-feature suite passes 212 unit and
integration tests and four doctests, including the compile-fail accessibility
API check: 216 passing tests in total. One egui host widget snippet is
intentionally ignored. All library targets and the standalone native host also
compile with Rust 1.95.0.

The complete stable-toolchain release script passed on the candidate working
tree: formatting, strict Clippy, warning-free API documentation, benchmark smoke
tests, headless examples, package inventory verification, and tests and all-target
compilation from the extracted crate. Native host formatting, strict Clippy,
and compilation against both the source and extracted library also passed.

The complete [CI run for commit `15bdda0`](https://github.com/Cameron-Lyons/textloom/actions/runs/37908584789)
passed on 2026-10-09: Linux release checks, all Rust 1.95 feature combinations,
and macOS/Windows all-target compilation, native host compilation, tests, and
doctests. Further working-tree fixes require the same checks on their final
clean commit; the baseline result does not verify subsequent changes.

The 1.0.0 working tree is an unreleased candidate. Linux Wayland rendering has
passed at 200% display scale with optional Noto CJK and Liberation Bold fonts.
Native ASCII typing and plain-text copy passed with exact clipboard content
verified. Native Unicode paste/copy, keyboard undo/redo, grouped adjacent caret
typing, and formatting shortcuts passed. Bold/italic/underline toolbar states
and rendered styles were checked before and after undo. Read-only selection/copy,
typing/deletion/paste suppression, and non-destructive cut passed. IME candidate
placement, screen-reader behavior, dead keys/AltGr, rich clipboard transport,
and macOS and Windows native interaction checks still require signoff.

Before publication, record the final commit, native host and version, operating
system and display backend, IMEs and screen readers used, and the outcome of each
native check below. Linux, macOS, and Windows host results and the CI matrix on
the final clean commit remain required. Publish only after these checks pass.

## Native host validation record

The standalone host in `examples/native-editor` uses eframe/egui 0.36.2 and
winit 0.30.13. It is available from a repository checkout and is intentionally
excluded from the published library archive. Its renderer, window, clipboard,
and platform accessibility dependencies remain outside the library's dependency
graph. The example transports plain text through the system clipboard; rich MIME
transport requires a host integration using Textloom's fragment API.

Reproduce the Linux host used for rendering checks from the repository root:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --font /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc \
  --bold-font /usr/share/fonts/liberation/LiberationSans-Bold.ttf
```

These optional font paths require the corresponding local font files; the host
does not install or change system fonts. Add `--smoke-test` to render 20 native
frames and exit, or `--read-only` for selection/copy checks with editing disabled.
The smoke mode fails if the widget reports an editing or accessibility error;
native IME, clipboard, and screen-reader behavior need separate interaction tests.

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
| Dead keys and AltGr | Manual validation pending |
| Screen-reader selection and replacement | Manual validation pending |
| Rich clipboard MIME transport | Host integration and validation pending |
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
   host without opening a window. It then runs all-feature tests and compiles
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
   they compile the standalone native host as well as the library targets.
5. Exercise a native host on Linux, macOS, and Windows. Check focus loss during
   composition, Japanese/Chinese IME replacement and candidate placement,
   dead keys and AltGr, platform shortcuts, plain/rich clipboard transport,
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
