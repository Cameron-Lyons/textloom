# Releasing Textloom

## 1.0 validation status

Local Linux validation on 2026-10-08 passed the complete release script with Rust
1.99.0, including all eight feature combinations, strict Clippy, documentation,
benchmarks, examples, and tests/all-target compilation from the extracted crate.
The all-feature suite and three doctests pass; one host widget
snippet is intentionally ignored. Rust 1.95.0 also passed all eight test/doctest
combinations and all-feature compilation of every target.

The 1.0.0 working tree is an unreleased candidate. As of 2026-10-08, no native
host testing has been performed. Do not treat passing automated checks as native
host signoff: candidate placement, clipboard transport, rendering, and platform
accessibility depend on services supplied by the host.

Before publication, record the final commit, native host and version, operating
system and display backend, IMEs and screen readers used, and the outcome of each
native check below. Linux, macOS, and Windows host results and the CI matrix on
the final clean commit remain required. Publish only after these checks pass.

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
   examples. It then runs all-feature tests and compiles every target from the
   extracted Cargo package, so missing fixtures or examples fail the release gate.
3. Review `cargo package --locked --list --allow-dirty`. Source, examples, tests,
   fixtures, benchmark documentation, changelog, and license must be included;
   repository workflows and local build output must be absent.
4. Commit the release changes, then run `./scripts/check-release.sh --tag v1.0.0`
   on the clean checkout. Run `cargo +1.95.0 test --locked --all-features` and
   confirm the MSRV feature checks and macOS/Windows jobs pass on that commit.
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
