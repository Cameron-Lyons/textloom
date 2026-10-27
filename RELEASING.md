# Releasing Textloom

## Current work toward 1.0

The changes in [PR #16](https://github.com/Cameron-Lyons/textloom/pull/16) are
unreleased; the package version remains 1.0.0-rc.2. They restore native
paste-as-plain-text with egui 0.37, repair repeated Windows rich-copy ownership,
and expand macOS TextEdit and Windows VM interaction checks. The native host
now reports clipboard operation failures instead of allowing a plain fallback
to leave every QA error counter at zero. The Windows example uses
`clipboard-win` 5.4.3 or later; 5.4.1 was yanked on 2026-10-23.

Stable 1.0 still requires final platform signoff. In particular, physical
keyboards, touch screens/touchpads, audible reader usability, display-scale
transitions, macOS IMEs/VoiceOver, and untested reader
or visual feature behavior remain open. Automated clipboard transport and
external-renderer assertions do not establish these observations. Record
unavailable checks explicitly rather than treating them as passes.

The [native dependency limitations](#native-dependency-limitations) distinguish
known failures from missing evidence and identify the configurations and
upstream fixes involved. Current automated evidence is recorded separately
from the historical sessions below; neither substitutes for manual signoff.

### macOS readiness refreshed 2026-10-27

The quote-export fix at `822297bbaa1ceba863c26cb46ca32c5c101809d5` passed
[native interaction run 40559669028](https://github.com/Cameron-Lyons/textloom/actions/runs/40559669028).
The tested pull-request merge revision was
`b8707c83e0d477ae1b71d488150806430ab65fe7`. Its downloaded macOS artifact
records 70 interaction checks, nine winit startup/Unicode clipboard checks,
no aggregate feature failures, and successful owned-process cleanup. The
three native interaction reports also pass the strict report validator with
zero reported errors. All nine jobs in
[CI run 40559669294](https://github.com/Cameron-Lyons/textloom/actions/runs/40559669294)
passed, including macOS rendering in all three modes with both clipboard
configurations.

TextEdit 1.20 (415) on macOS 26.6.2 (25G83) now preserves both quote indents
at 36 points in reused and fresh neutral documents. Before-copy captures show
both tested paragraph starts about 41 points from the text area's edge,
compared with 5 points for the matching plain HTML; the indented RTF control
agrees. The current exporter therefore clears the recorded TextEdit quote
blocker on this configuration. Historical bare-HTML and incomplete-margin
controls still fail and remain diagnostic evidence. The visible comparison
covers two short paragraph starts, not wrapped text or other TextEdit versions.

Local verification used macOS 27.2 (26B123), Apple Silicon, Rust 1.98.1,
and a 2x Retina display, on a working tree based on that fix. All 15 clipboard
tests passed, including three private-pasteboard tests. The strengthened AppKit
regression verifies HTML import and RTF export/reimport retain Unicode, soft
breaks, both quote indents, unindented body paragraphs, bold, font/fill colors,
and links. All 71 QA-tool tests, strict QA Clippy, nine Swift AX retry self-tests,
and nineteen feature/capture self-tests passed. Five local 20-frame rendering
sessions passed strict report validation: default clipboard in editable,
read-only, and disabled modes, and rich clipboard in read-only and disabled
modes. No local system-clipboard fixture was run. The library and native host
also pass all-target, locked Intel (`x86_64-apple-darwin`) cross-compilation
checks; that does not establish Intel runtime support. Local logs, reports,
and the downloaded hosted artifact are in `target/macos-readiness-20261027/`.

The complete local release-check sequence passed: all eight feature
combinations, release-mode tests, 74 native-host unit tests and its link-opening
integration test, strict library/host Clippy, warning-free rustdoc, quick
benchmarks, headless examples, package inventory and tests, and a native host
compiled against the extracted package. The first run stopped at a missing
`#[must_use]` on `AccessibilitySnapshot::paragraphs`; after adding it, the
remaining script steps were rerun from Clippy onward. Both logs are retained
as `release-checks.log` and `release-checks-resumed.log`. Stable/nightly
formatting and `actionlint` also passed. Nothing was published.

The input/navigation, composition routing, and accessibility review found no
additional production defect. The interaction probe now also requires
`RUNNER_ENVIRONMENT=github-hosted` before touching the desktop, preventing an
accidental run on a self-hosted developer Mac. Its hosted passes above predate
this guard and the stronger regression, so the final clean candidate must
repeat CI.

The first [PR #17 native run](https://github.com/Cameron-Lyons/textloom/actions/runs/40562889186)
failed in the harness: all nine winit behavior checks passed, but process-group
cleanup returned `Operation not permitted`; TextEdit then failed to resolve a
window during `AXRaise`, before the first paste. These failed aggregate results
remain preserved under `target/macos-ci-failure-40562889186/`. The corresponding
Mac and Windows lint jobs also rejected the bare `#[must_use]` with Rust 1.99;
the annotation now has an explicit message compatible with Rust 1.98 and 1.99.
The harness now retries only explicitly marked pre-input stale-window errors
within its existing deadline, and continues reaping on Darwin's transient
process-group permission error until the group actually disappears. Persistent
groups and uncertain input failures still fail. All 75 QA-tool tests and four
headless Python cleanup regressions pass; the Python regressions now run in
the hosted Mac job. Strict library, native-host, and QA Clippy pass with Rust
1.99.1, and the AppKit quote/RTF regression passes on that compiler too.
These failures do not invalidate the earlier quote-import observations, and
the corrected PR must pass fresh hosted checks before merging.

macOS 1.0 signoff remains incomplete. Record Japanese and Chinese IME startup,
candidate placement in scrolled text, cancellation, and focus round trips for
both the egui and winit integrations; VoiceOver reading, selection, replacement,
and link/quote semantics; physical keyboard/dead-key and touchpad behavior; and
display-scale transitions using the [native protocol](examples/native-editor/NATIVE_QA.md).
Intel runtime and older-macOS compatibility remain unverified. Keep the package
at its release-candidate version until the required outcomes are recorded on
the final clean revision.

### Automated evidence refreshed 2026-10-23

The production fixes are at clean commit `3908e3c208a58863246173720f6d5d9ff22ab4a7`.
The full `scripts/check-release.sh` passed: all eight feature combinations,
release tests, native host tests, strict Clippy, rustdoc, quick benchmarks,
package inventory, and extracted-package consumer validation. The publication
dry run passed without uploading. All nine CI jobs passed at `b923934` in
[run 39974423410](https://github.com/Cameron-Lyons/textloom/actions/runs/39974423410);
that excludes the separate native interaction workflow. Subsequent commits
extend repository-only QA and documentation; they do not change the published
library implementation.

The isolated Linux run used a copied executable with that revision and recorded
SHA-256. All 27 interaction checks passed, along with rich-source plain paste
and single undo on X11 and Wayland, and quote-rule geometry/contrast in both
themes. Seven X11 IME assertion groups passed, while their observations still
showed the Find round-trip keyboard stall and a 9-pixel candidate overlap.
Those failures remain open. Two repeat runs loading upstream xcb-imdkit commit
`621048bd50e8f3cbf52335f824f7b4d708a21b14` in Fcitx restored keyboard input and
new composition; each recorded the library actually loaded by Fcitx.

These Linux sessions used private HOME/runtime directories and display servers:
Sway 1.12, Xvfb 21.1.25, Fcitx 5.1.23, Mozc 3.35.6354.2, Pinyin 5.1.15,
xcb-imdkit 1.0.9, libX11 1.8.14, Orca 51.1, and AT-SPI 2.62.1. Sway required
running under GDB because direct launches exited with status 137; the editor
ran without instrumentation. Virtual input, static 200% scaling, and captured
reader output do not establish physical-device or audible-reader signoff.
The recorded Orca link/quote announcement gaps remain open. Local evidence,
harness snapshots, executable hashes, logs, and upstream research are retained
under `target/native-qa-1.0-20261023/`; the clean Linux run is `linux-3908e3c/`.

The Windows 11 VM completed all 21 aggregate checks on the same clean
`3908e3c` production revision, with source inventory and deployed executable
SHA-256 verification. The new groups cover seven consecutive rich copies,
colors, soft breaks, links, quotes in both themes, numbering (including a
partial list starting at 3), native Ctrl+Shift+V, and external MSHTML 11.00.26100.1
interoperability. MSHTML independently retained Unicode, emphasis, fill/font
colors, links, soft breaks, quotes, and ordered lists; its copy back exercised
plain fallback without Textloom data. Hosted NVDA assertions passed 15/15;
a sound-card capture recorded about 1.98 seconds above its silence threshold,
which does not establish audible usability. The no-warmup dead-key diagnostic
still failed: the first acute-plus-e after switching to German inserted `e`
instead of `é`. A separate native Ctrl+Alt+Q diagnostic left the document
unchanged instead of inserting `@`; the window remained open. Both diagnostics
explicitly record `passed: false`. Aggregate passes do not close these failures.

Supplemental Windows checks passed immediate modifier release (all six
Ctrl+Shift+V press/release events in one QMP packet) and the ignored repeated-copy
helper regression with a blocked host window. Fourteen native reports recorded
graceful shutdown, zero errors, and equal TLFR round trips. The VM, job server,
owned guest processes, and temporary NVDA copy were cleaned up. Evidence is in
`/home/cam/.local/share/textloom-winvm/results-3908e3c-final/summary.json` and its
per-check directories; the native executable SHA-256 is
`ef592901c717525257125483b7eb011bef799444e6de06b69d0025dd61ca8d41`.
Word/Outlook interoperability and physical hardware remain unverified.

The new macOS winit probe passed all nine observations on macOS 26.6.2 arm64,
most recently in
[run 39974423408](https://github.com/Cameron-Lyons/textloom/actions/runs/39974423408),
PR head `b923934` / tested merge `d131ca007e2e8c997c8090724643163330aae2c3`.
It verified natural startup focus before the first native key, exact Unicode
copy/paste, document and selection restoration through undo/redo, and owned
process cleanup. It did not test an IME or VoiceOver. The separate TextEdit
probe passed native Command+Shift+V from a rich source, fill/font color and link
controls with undo/redo, external TextEdit color/link import, a soft break
within one numbered item, quote continuation/exit and Backspace behavior,
numbering controls and external partial-list numbers, forced-wrap deletion,
and read-only/disabled guards. All nine Swift accessibility retry self-tests
and nineteen feature/capture self-tests passed. All three native sessions closed
gracefully with zero reported errors, and owned processes were cleaned up.

Before the quote-export fix above, the overall interaction result failed.
TextEdit's copied RTF retained
the exact quote text but no paragraph indentation from the native
`<blockquote>` offer, in both reused and fresh neutral documents. Controlled
HTML and AppKit parser comparisons reproduced the result. A margin on child
paragraphs produced a positive subsequent-line indent but left the first-line
indent at zero. AppKit retained a block-quote presentation intent; the copied
RTF did not. These five failed observations concern the same quote-import issue,
not five independent defects.

The pre-copy visual comparison confirms a quote-presentation interoperability
failure with TextEdit 1.20 (415) on macOS 26.6.2 (25G83). The native quote and
matching plain-paragraph HTML looked identical, with no visible quote treatment;
both paragraph starts were 5 points from the text-area edge. A known indented
RTF control placed both starts at 41 points, demonstrating the intended
36-point difference. All three used the same window/text-area dimensions and
fresh neutral documents. Identity, exact text, and clipboard generation were
verified before and after each window-only screenshot, before any copy or save.
The observation covers two short paragraph starts, not wrapped lines or other
TextEdit/macOS versions. Native source offers, returned RTF, screenshots, and
geometry are retained under `macos-run7-39974423408/` in the local evidence
archive; prior failed runs remain preserved. The corrected exporter clears this
blocker on the same TextEdit/macOS configuration in the 2026-10-27 run above.

## Earlier validation evidence

Version 1.0.0-rc.2 was prepared on 2026-10-22, after the rc.1 and Windows VM
work recorded below, as a second release candidate: `main` at
`91abb32939638a1243282bb444ed73858f32bbe6` plus the version change and
documentation fixes. Since rc.1, `main` has applied the
Google Rust Style Guide, which renames or retypes some public API (see
`CHANGELOG.md`), and the native host's Ctrl+Alt+Q fix described below. On
`91abb32`, `scripts/check-release.sh` and `scripts/format.sh --check` passed, as
did all nine CI jobs; both hosted interaction jobs passed on PR #14's head
`8f8cbdea032a284ec874dcdf95e5ccd516a65b6b`. The isolated Linux sessions and
Windows VM checks below were not repeated after the style changes. The open
Windows, macOS, and physical-device checks still block 1.0.0.

On 2026-10-22, version 1.0.0-rc.1 was prepared as a release candidate, because
no Windows desktop is available for the hands-on checks below. Its commit
`d7f2feec05ec599af280f64645c95fdd1b44c86e` is `main` at
`3d4c5ab0ab93e7bb379958e59562dd651274af34` plus the version change. The
isolated Linux checks described below were repeated on it: all 27 interaction
checks, the light- and dark-theme block quote check, and the seven X11 IME
checks passed. The winit adapter checks passed twice each under Wayland and
X11, except the X11 candidate placement diagnostic, which again found the list
10 px below the top of a 24 px caret (see the second table). The open Windows,
macOS, and physical-device checks are listed in the release candidate's notes;
1.0.0 still requires them.

Later on 2026-10-22, with no Windows machine available, the Windows checks ran
in a local Windows 11 Enterprise LTSC 2024 evaluation VM (build 26100) under
QEMU/KVM, using `scripts/windows-vm/`. QEMU's PS/2 keyboard and USB tablet sent
keys and clicks as hardware input, so layouts, dead keys, AltGr, and the
Microsoft IMEs handled them as from a physical keyboard; UI Automation read the
editor, and screenshots located IME candidate lists. This is VM evidence, not
physical-keyboard, touch, or manual signoff. On `main` at
`0d6e05cc401dcb0b8df0cf5aed0c93e6e9157f8a`, cross-compiled with MinGW and drawn
with Mesa llvmpipe as in the hosted jobs, 13 of 14 checks passed in the final
full run, among them the hosted NVDA probe (all 15 checks) and NVDA's default
voice reaching the virtual sound card; each had also passed in at least one
earlier run. The second table records the results. The failure: on a German
layout, Ctrl+Alt+Q, Windows' alternative to AltGr+Q for `@`, closed the native
host, because egui's default quit shortcut is Ctrl+Q matched with extra Alt
ignored. The host now handles Q itself and leaves Alt chords alone, as its
Ctrl+K link shortcut does; at `786edfb27b7a25e529bad8ceee997330a1b14b74` all
14 checks passed. egui-winit still drops text typed while Ctrl is held, so
egui hosts never type Ctrl+Alt characters; AltGr itself typed `@€`, and the
winit adapter typed `@` for Ctrl+Alt+Q.

The first dead key after a layout switch was lost in every run of the check,
and in one of two tries in egui's own `TextEdit`, while Notepad kept it; later
dead keys worked. winit 0.30.13 prepares a newly selected layout on the next
key press by translating every key with `ToUnicodeEx`, which consumes the
pending dead key. winit 0.31 prepares it on `WM_INPUTLANGCHANGE` instead
([rust-windowing/winit#4582](https://github.com/rust-windowing/winit/pull/4582)),
which 0.30 does not have. That change applied to 0.30.13 kept the first dead
key in 3 of 3 native host runs and 5 of 5 tries in egui's Find field, and the
layout and IME checks still passed. egui and winit hosts keep the loss until
they move to a winit with the change.

Local macOS verification on 2026-10-22 used macOS 27.2 (26C111), Apple Silicon
(`aarch64-apple-darwin`), Rust 1.99.1, and a 2x Retina display. This checked the
uncommitted working tree based on
`7512ce8b67ce461f2866e5a4b0a9f5e6f22be824`, labelled
`7512ce8b67ce461f2866e5a4b0a9f5e6f22be824-macos-working-tree` in the rebuilt
host reports. It is supporting evidence, not final clean-commit signoff.

`scripts/check-release.sh --allow-dirty` passed on this Mac, including all eight
feature combinations, release-mode search/replacement tests, strict Clippy for
the library and native host, warning-free documentation, benchmark smoke runs,
headless examples, and package inventory/build/test checks. The all-feature
library run passed 632 unit/integration tests. All 26 Python QA regressions and
the workflow `actionlint` checks passed. The complete release log was recorded
in `/private/tmp/textloom-macos-release.log`; no package was published.

The Mac compatibility changes cover Control+A/E/B/F/P/N and H/D,
Option+Up/Down with Shift selection, visual-row Command+Backspace, and
event-ordered plain paste modifiers. Layout refresh now uses the same platform
as key execution; same-frame paste/Alt+Up regressions cover Linux and Windows
as well. Native host tests passed all 71 cases, including three AppKit clipboard
tests using private named pasteboards. These tests require access to the macOS
pasteboard service; a filesystem sandbox that blocks that service cannot run
them. They do not read or replace the general clipboard.

Six rebuilt native rendering sessions passed: default and rich clipboard
configurations, each editable, read-only, and disabled. Every session rendered
21 frames at 2x, closed gracefully, reported zero errors, and preserved an equal
TLFR round trip. No clipboard self-test was requested, so these runs did not
publish a fixture to the system clipboard and do not establish native clipboard
interoperability. Local reports and logs were recorded in
`/private/tmp/textloom-macos-smoke/`.

The hosted Mac interaction probe now includes Control/Option navigation,
Shift selections, Unicode grapheme deletion, soft-line Command+Backspace, and
exact copied-text/undo assertions. Its Python regressions run locally. On PR
#10's head `2acb5842d1bae8dc7eede35be89215fa310680b3`, the
[hosted run](https://github.com/Cameron-Lyons/textloom/actions/runs/39793426804)
passed all 25 new shortcut checks within the workflow's interaction deadline,
along with the existing editable, read-only, and disabled checks. Forced-wrap
deletion is covered by headless widget regressions and remains a native signoff
check.
Physical keyboards, Japanese/Chinese IMEs, VoiceOver, native winit input, and
display-scale transitions remain pending. This local run does not establish
Intel or older-macOS compatibility. Retain the platform signoff requirements
below before publication.

On 2026-10-21 the candidate moved to egui/eframe 0.37 and AccessKit 0.25 in
[PR #8](https://github.com/Cameron-Lyons/textloom/pull/8). At its head
`4e5a126628432eb021544d639b8d67666fb9f457`, `scripts/check-release.sh`, the Rust
1.95.0 test and native host commands from step 4, and all eleven CI checks
passed, including both hosted interaction jobs. The hosted macOS TextEdit probe
found the host's missing system clipboard (eframe 0.37 moved it behind a
default feature), which that PR restores.

The same build then passed 27 interaction checks in isolated Linux sessions:
headless Sway 1.12 (wlroots 0.20.2, pixman) and Xvfb 21.1.25, each with its own
HOME, runtime directory, and D-Bus session, driven by wlroots virtual pointer
and keyboard clients and `wtype`. They used Fcitx 5.1.23 with Mozc 3.35.6354.2
and Pinyin 5.1.15, Orca 51.1 with AT-SPI 2.62.1, Speech Dispatcher 0.12.1, and
espeak-ng 1.52.0, and LibreOffice 26.8.1 Writer as the external rich editor.
Results are in the second table below. AT-SPI document text, caret, selection,
attributes, and links verified each outcome; screenshots verified painting.
This is virtual-input evidence, not physical-keyboard or platform manual
signoff, and the speech was synthesized, not heard. The evidence that follows
predates this candidate.

On 2026-10-22 the remaining Linux gaps were checked in the same kind of
sessions, on branch `x11-ime-window-blur` at
`025f9a79dd9bacbc01c82023e737d4a930fd02d3`. X11 IME, now testable under Xvfb
with Fcitx 5's XIM frontend, and a scratch winit host found three IME defects,
which that branch fixes: the egui widget and the winit adapter released IME
whenever the window lost focus, so after an app switch winit 0.30's X11 backend
recreated the input context and Fcitx 5 reset it to direct input; and the winit
adapter did not resend its caret area after `Ime::Enabled`, so on Wayland the
first preedit arrived without a cursor rectangle. The native host gained
`--theme light|dark` for the light-theme checks. At that commit
`scripts/check-release.sh` passed, the 27 checks above passed again, and the
light-theme, X11 IME, and winit adapter results below were recorded. One X11
problem remained: composing in the document, clicking Find, and clicking back
into the document stopped all keyboard input to the window. A follow-up the same
day traced it to Fcitx 5's XIM library, xcb-imdkit 1.0.9
([fcitx/xcb-imdkit#17](https://github.com/fcitx/xcb-imdkit/issues/17)). Egui's
own text fields lose input the same way, so it is a platform limitation, not a
Textloom defect (see the second table).

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

A later improvement pass changed editing behavior, both input adapters,
accessibility publication, and CI. A feature pass then added computed list
numbering, fill colors, soft line breaks, hyperlinks, and block quotes, with
native host controls for each, and moved the native format to TLFR version 2;
see `CHANGELOG.md`. The evidence below predates both. Repeat the automated and
native checks on the publication commit, including the pending checks listed
after the QA table.

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

The initial 1.0.0 working tree was an unreleased candidate in
[PR #1](https://github.com/Cameron-Lyons/textloom/pull/1). Its Linux Wayland rendering had
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
  selection announcements through a private file sink. These sessions used the
  example's former backport of the upstream AT-SPI activation and desktop-parent
  fixes that `accesskit_unix` 0.24 now includes. That adapter exposed `Text` but
  not `EditableText`, so direct AT-SPI text replacement was not certified.
  With `accesskit_unix` 0.24, AT-SPI `set_text_contents` replaced the document
  and one undo restored it on 2026-10-21; AccessKit does not support the other
  `EditableText` operations.
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

The standalone host in `examples/native-editor` uses eframe/egui 0.37.0 and
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
new offer during copy verification and does not alter the TLFR format or establish trust.
Native access runs in helper processes with hard deadlines and bounded transfers;
native lengths are checked before Rust payload allocation. TLFR retains the
library's default 64 MiB limit (`DecodeLimits::DEFAULT`), with a 32-byte
transport envelope allowance. Copy verifies all
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

Historical QA record, starting 2026-10-09: clean implementation candidate `5f8ccfb`;
earlier desktop observations used an uncommitted candidate on Omarchy
4.0.0.r6815.g50d687a (Arch-based Linux), kernel 7.2.8-5-omarchy-bore,
Hyprland/Wayland, 200% display scale. The isolated Linux sessions above add IME,
reader, and external clipboard evidence. Final commit and complete platform
signoff remain pending. The tables below retain their original observations;
the current evidence at the top supersedes their older pending statuses.

| Check | Result |
| --- | --- |
| Native window rendering at 200% scale with Noto CJK and Liberation Bold | Passed |
| Native ASCII typing and plain-text copy | Passed; exact clipboard content verified |
| Unicode clipboard paste/copy | Passed; exact UTF-8 content with Japanese, combining marks, emoji sequences, flag, and paragraph break verified |
| Native keyboard/paste undo and redo | Passed; document restored exactly, adjacent caret typing undoes in one step |
| Native formatting shortcuts | Passed; bold/italic/underline and their undo verified in toolbar and rendered text |
| Native read-only selection/copy | Passed; exact clipboard content verified |
| Native read-only editing suppression | Passed; typing, Backspace/Delete, Enter, paste, and undo preserve the document; cut copies without deleting |
| Japanese/Chinese IME preedit, replacement, focus loss, and candidate placement | Real Linux Mozc/Pinyin sessions passed with virtual input under Wayland and, from 2026-10-22, X11. In the Windows 11 VM, the Microsoft Japanese IME passed replacement, one-step undo/redo, and commits on focus changes, Microsoft Pinyin committed at the caret with one-step undo, and both candidate lists opened beside the composition; physical-keyboard and macOS signoff pending |
| Dead keys and Compose | German XKB virtual sequences on clean `f05a518` produced visible `é`, exact copied bytes, and complete undo restoration. In the Windows 11 VM, dead acute + `e` typed `é` and QWERTZ Ctrl+Z undid it, though winit 0.30 loses the first dead key after a layout switch (see above); physical-keyboard and macOS signoff pending |
| AltGr | German XKB virtual AltGr on clean `f05a518` produced visible `@€`, exact copied bytes, and complete undo restoration without focus loss. In the Windows 11 VM, AltGr+Q/E typed `@€` in the native host; Ctrl+Alt+Q closed it until the host stopped using egui's default quit shortcut (see above); physical-keyboard and macOS signoff pending |
| Screen-reader selection and replacement | Linux Orca and hosted Windows NVDA generated speech, accessible selection/caret, native replacement and undo/redo passed; direct AT-SPI EditableText unavailable then, since added by `accesskit_unix` 0.24 (see above); in the Windows 11 VM the hosted NVDA probe passed all 15 checks, and NVDA's default voice produced about 2 s of sound after Down, recorded for listening but not yet heard; VoiceOver and full manual signoff pending |
| Rich clipboard MIME transport | Widget fixtures passed on all platforms; Linux Qt and macOS TextEdit interoperability passed; Windows external applications and full manual signoff pending |
| macOS and Windows native host checks | Hosted macOS native editing/focus/guard checks and Windows NVDA interaction passed; physical keyboards, IMEs, VoiceOver, scale changes, and full manual signoff pending |

The later improvement and feature passes added the checks below. Their Linux
results come from the isolated sessions of 2026-10-21 and 2026-10-22 described
at the top; macOS, Windows, and physical-keyboard results were pending then.
Those Linux winit results came from a scratch host; the repository now includes
the [windowed winit harness](https://github.com/Cameron-Lyons/textloom/tree/main/scripts/windows-vm/winit-harness).
Both follow the README: the host calls `set_focused`
at startup, routes every window event through `handle_window_event`, and calls
`sync_ime` each frame with its painted caret. The `NATIVE_QA.md` protocol gives
steps for the feature rows.

| Check | Result |
| --- | --- |
| Winit adapter: macOS typing at launch without switching apps; IME candidate window at the caret on the first composition; Russian layout Ctrl+C/Ctrl+V; Linux Ctrl+Alt+T reaching the host; Windows Shift+Delete, Ctrl+Insert, Shift+Insert, and AltGr through Ctrl+Alt | Linux passed on Wayland and X11 with Fcitx 5 Mozc, twice each: the first composition had the caret's cursor rectangle in effect and its candidate list opened under the caret, also for a host that anchors its IME area at the composition start; under X11 composition worked from the first key and after an app switch; with a Russian layout, Ctrl and the physical A, C, V, and Z keys (logical ф, с, м, я) selected all, copied the exact text, pasted it once, and undid the paste in one step; Ctrl+Alt+T and Ctrl+T were left unhandled and typed nothing; typing worked at launch once the compositor focused the window. On X11 the candidate list covers the bottom 9–14 px of the caret line, because winit 0.30 sends only the area's top-left as the XIM spot. In the Windows 11 VM, Ctrl+Insert, Shift+Delete, and Shift+Insert copied, cut, and pasted through the Windows clipboard with one-step undo; AltGr+Q, Ctrl+Alt+Q, and AltGr+E typed `@@€` on a German layout; a Russian layout's Ctrl with physical A, C, V, and Z selected all, copied, pasted once, and undid; and the first Japanese composition had the caret's area, with its candidate list beside the caret. macOS rows pending |
| Egui click, drag, and focus loss during composition commit the preedit exactly once, without a double commit, on macOS, Windows, and IBus/Fcitx 5 under Wayland and X11 | Fcitx 5 under Wayland passed: click, drag, a touchpad tap, and moving focus to Find each committed the visible preedit once where it was composed. Switching to another application either committed it once or canceled it, depending on whether Fcitx's preedit clear reached egui in the same frame as the focus loss (both outcomes seen with egui 0.36 and 0.37); it was never duplicated, and typing resumed at the caret. Fcitx 5 XIM under X11 passed after the 2026-10-22 fix: click, drag, and moving focus to Find committed the visible preedit once; switching applications canceled the preedit, never duplicated it, and kept Mozc active for the next composition. X11 platform limitation: after composing and clicking an egui text field, keyboard input stopped for the whole window with Fcitx 5.1.23 and xcb-imdkit 1.0.9. Release builds lost it in 8 of 8 runs both for the document and Find and for egui's own Replace and Find fields. An earlier control between egui fields that kept working was invalid, because its click landed on the candidate window. Egui-winit recreates the X11 input context on each IME interrupt, and `TextEdit` interrupts on both press and release. The second recreation makes libX11 answer Fcitx for a destroyed context, and xcb-imdkit then queues every later key ([fcitx/xcb-imdkit#17](https://github.com/fcitx/xcb-imdkit/issues/17)). With xcb-imdkit built from its fix ([#18](https://github.com/fcitx/xcb-imdkit/pull/18), merged 2026-10-09, not yet released), both round trips kept the keyboard in all 18 runs. Restarting Fcitx 5 restored typing and composition in 3 of 3 runs. In the Windows 11 VM with the Microsoft Japanese IME, a click elsewhere in the document or in Find committed the preedit once where it was typed, with Find left empty, and Escape canceled it. macOS and IBus pending |
| Egui IME candidate placement in long scrolled documents | Fcitx 5 Mozc under Wayland passed: composing on the last line of a 300-line document kept the cursor rectangle inside the visible area, and the candidate list opened beside it, above the caret at the bottom edge. Also passed at 200% scale. Under X11 the list opened at the caret's x, 10 px below the caret's top, and flipped above the caret at the bottom edge. In the Windows 11 VM the Microsoft IME's list opened just above the caret on the last line of a 300-line document, inside the screen. macOS pending |
| Egui touch scrolling: swipes scroll until the editor has focus | Pending; virtual touch input was unavailable |
| AZERTY digit-row shortcuts (Ctrl/Command+0–6, Shift+7/8) | Linux French XKB layout through a virtual keyboard with evdev keycodes passed: Ctrl+physical 1/3/0 set Heading 1, Heading 3, and Body; Ctrl+Shift+physical 7/8 made numbered and bulleted lists; unmodified keys typed `&é`. The Windows 11 VM's French layout passed the same checks with hardware scancodes. Physical keyboards and macOS pending |
| Egui touchpad tap-to-click places the caret, commits a preedit, and activates links | Linux press-and-release in one frame passed: it placed the caret, committed a Mozc preedit once, and opened a link in read-only mode. Physical touchpads pending |
| Fill Color and Font Color: apply, No Fill/Automatic, undo/redo; selections and found matches stay visible over fills; fill survives external rich copy | Linux passed: AT-SPI attributes confirmed yellow, a menu color, the button repeating it, No Fill removing only that fill, Red, a menu color, and Automatic; each command undid and redid in one step. A selection and a found match painted visibly over the fill. LibreOffice kept the fill and font color. macOS and Windows pending |
| Soft line breaks: Shift+Enter in a list item and a heading; caret, selection, Home/End, and Up/Down across the break; screen-reader line reading; external plain copy shows a line break; HTML paste into Word/TextEdit shows the break | Linux passed: the numbered item kept its number and marker, the heading stayed one heading, Home/End/Up/Down and Shift+Left followed the visual lines, AT-SPI and Orca read the two lines separately, plain copy had a line break, and LibreOffice showed one numbered paragraph with a line break. macOS and Windows pending |
| Links: Link button and Ctrl/Command+K add, edit, remove, and undo; Ctrl/Command+click and read-only click open the browser; `javascript:` targets refused; pointing hand and target tooltip on hover; screen reader announces and activates links; copy to Word/TextEdit keeps the hyperlink | Linux passed: add, edit, and remove were one undo step each and kept the selection; Ctrl+click, a read-only click, and AT-SPI activation opened the URL once without moving the caret; the `javascript:` target was refused with a status message; hovering showed the target, and the cursor became a pointing hand only with Ctrl held or in read-only mode; LibreOffice kept the hyperlink. Orca read link text but did not announce links at the caret, because AccessKit's Unix adapter has no Hypertext interface. macOS and Windows pending |
| Block quotes: bar continuous across consecutive quotes in light and dark themes; Enter/Backspace behavior; screen reader announces a block quote and reads through it; HTML paste into Word/TextEdit shows a block quote | Linux dark theme passed: one bar spanned three consecutive quote paragraphs; Enter continued the quote, and Enter on an empty quote paragraph or Backspace at a quote's start made body text in one undo step; AT-SPI exposed one block quote; LibreOffice used its Quotations style. Orca read through the quote without announcing it. With `--theme light` and `--theme dark`, one bar ran along the quote's left edge for the full height of three consecutive quote paragraphs, in egui's weak text color (contrast 2.96:1 light, 2.78:1 dark). macOS and Windows pending |
| Numbering controls: Numbered list and Ctrl/Command+Shift+7 extend or start lists; markers and the "Item N." label continue past body text and restart after headings; Restart at 1 and Continue numbering change only the focused item in one undo step; copied items keep their numbers in Word/TextEdit | Linux passed: markers and the "Item N." label read 1, 2, 3; body text in the middle gave 1, 2, and changing it back restored 1, 2, 3; Restart at 1 and Continue numbering changed only the focused item, one undo step each; Ctrl+Shift+7 extended the list; an item after a heading restarted at 1; LibreOffice showed 2 and 3. macOS and Windows pending |

## Native dependency limitations

These are observed failures in the native integration stack, not untested
checkboxes. They remain relevant to a consumer even when the defect is outside
Textloom. A release candidate must disclose them. Before stable publication,
resolve a failed behavior claimed by the release or explicitly narrow that
claim and record the affected configuration; an upstream attribution is not
passing signoff. See the [native protocol](https://github.com/Cameron-Lyons/textloom/blob/main/examples/native-editor/NATIVE_QA.md)
for the precise interaction checks.

- **X11 with Fcitx 5 and xcb-imdkit 1.0.9:** composing in Document, clicking
  Find, and returning to Document can stop all keyboard input. The upstream
  [per-input-context synchronization fix](https://github.com/fcitx/xcb-imdkit/pull/18)
  restores the tested round trip. Use a distribution build carrying that fix,
  or a tested Wayland configuration; restarting Fcitx recovers a stuck session
  but does not prevent recurrence. Do not describe this X11 combination as
  fully supported solely because a basic composition test passes.
- **Windows with winit 0.30.13:** the first dead key immediately after switching
  layouts can be lost. Subsequent dead keys and ordinary AltGr input passing
  do not clear this failure. The upstream
  [layout-change initialization fix](https://github.com/rust-windowing/winit/pull/4582)
  needs a backport or a compatible upstream release. Textloom cannot recover
  text that winit did not deliver.
- **Ctrl+Alt text in egui hosts:** egui-winit 0.37 suppresses text whenever
  Control is held. Windows' Ctrl+Alt alternative to AltGr therefore does not
  type characters in the egui host; physical AltGr is the tested alternative.
  Textloom's separate winit adapter accepts the Windows Ctrl+Alt text events.
  The example's earlier unintended Ctrl+Alt+Q quit has been fixed.
- **X11 IME candidate placement with winit 0.30.13:** only the caret area's
  top-left point is sent to XIM, so a popup may overlap the caret line.
  Wayland needs the actual caret rectangle; moving that rectangle in the
  widget to hide the X11 limitation would make other integrations incorrect.
- **Linux reader semantics:** Orca reads the tested link and quote text but
  did not announce links at the caret or announce a block quote. AccessKit's
  Unix adapter lacks the Hypertext interface used for link navigation.
  Successful text, selection, and replacement probes do not certify those
  semantic announcements or audible usability.

As of 2026-10-23, winit's latest stable release is 0.30.13, egui's is 0.37.0,
and xcb-imdkit's latest tag is 1.0.9. An unreleased upstream fix or a locally
patched experiment is evidence for a remedy, not the dependency shipped by
the published crate. Breaking upgrades of exposed adapter types must also
respect the compatibility contract below.

## Compatibility contract

Textloom 1.x follows semantic versioning for the documented public API and feature
names. Existing default builds remain independent of GUI libraries. Optional
adapters expose types from egui 0.37, winit 0.30, and AccessKit 0.25; an upstream
upgrade that breaks those exposed types requires a Textloom major release.
Error enums (`Error`, `FragmentError`, `AccessKitError`, egui
`AccessibilityError`), `ParagraphKind`, `Movement`, `InlineAttribute`,
`AccessibleAction`, and winit `ClipboardEvent` are non-exhaustive: callers must
retain a fallback match arm. `Composition`, `DecodeLimits`, `Span`,
`NamedColor`, `AccessibleParagraph`, `AccessibilityOutcome`,
`RichTextEditorOutput`, and winit `InputOutcome` are non-exhaustive structs that
may gain fields. `InlineStyle`, `StylePatch`, `SelectionStyle`, `SearchOptions`,
`HistoryLimits`, `Position`, `Selection`, `Color`, `AccessiblePosition`, and
`AccessibleSelection` stay exhaustive, as do the two-way enums `SelectionMode`,
`SearchWrap`, `accessibility::Focus`, and winit `CommandModifier`; adding a
field or variant to them requires a major release. `Editor`, `Document`,
`Paragraph`, and `Fragment` must remain `Send + Sync`, which `src/lib.rs` checks
at compile time.
Exact error messages, private implementation details, cache layouts, and benchmark
timings are not API guarantees. Release candidates, such as 1.0.0-rc.1, precede
this contract: their API may still change before 1.0.0.

Rust 1.95 is the minimum supported compiler for 1.0. The locked dependency graph
is tested on that compiler, and a CI job also compiles the lowest direct
dependency versions that `Cargo.toml` permits (AccessKit 0.25.1 with egui 0.37). Any future minimum-version increase must be announced
in a minor or major release, never a patch release. Review dependency updates
against the minimum compiler, all eight feature combinations, and the cargo-deny
policy in `deny.toml`. Development-only dependencies, such as
`accesskit_consumer` for loading published accessibility trees in tests, are
outside the library's dependency contract.

TLFR format versions are independent of the crate version. Textloom 1.x writes
version 2 and reads versions 1 and 2. The signature, version byte, fixed integer
widths, tag meanings, style bits, and decoded semantics of both versions are
stable. Future readers must continue to accept valid v1 and v2 snapshots within
the documented default decoding limits (`DecodeLimits::DEFAULT`: 64 MiB, one
million paragraphs, and one million spans). In-memory documents can exceed these
limits; the default decoder rejects their encoded snapshots, and
`Fragment::from_bytes_with_limits` or `Document::from_bytes_with_limits` reads
larger trusted snapshots. The format stores document content, not selection,
undo history, or an active composition. The canonical v2 encoding rules and the
v1 reading rules follow.

Version 2 keeps v1's signature, little-endian `u64` lengths, canonical span
rules, limits, and trailing-data rejection, and changes only these rows:

- Paragraph kind tags 0 (body), 1 (heading + `u8` level), and 2 (bullet + `u8`
  indent) are shared. Tag 3, v1's ordered item (`u8` indent + `u32` displayed
  ordinal), is v1-only. Version 2 adds tag 4 (ordered item continuing its list,
  `u8` indent), tag 5 (ordered item with an explicit start, `u8` indent + `u32`
  start), and tag 6 (block quote, no payload).
- Style bits 0–5 (bold, italic, underline, strikethrough, code, foreground
  present + 4 RGBA bytes) are shared. Version 2 adds bit 6 (fill present + 4 RGBA
  bytes after the foreground bytes) and bit 7 (link present + `u64` byte length
  and UTF-8 target after the fill bytes). Adjacent spans must differ in style or
  link, and targets must be valid for `Link::try_new`.
- Paragraph text is UTF-8 without CR or LF in both versions; version 2 also
  rejects U+2029 PARAGRAPH SEPARATOR. U+2028 LINE SEPARATOR is a soft line break
  and was already valid v1 text, so soft breaks need no format change.

A v1 payload is decoded with v1 rules only: tags 4–6 and style bits 6–7 are
rejected inside it. Tag 3 is read into the computed-numbering model: an item
becomes `start: None` when its stored ordinal equals the number it would show by
continuation (`Document::list_number`) and `start: Some(ordinal)` otherwise, so
every decoded item shows its stored ordinal. U+2029 in v1 text is read as a
U+2028 soft line break. Re-encoding a v1 snapshot writes version 2, which
Textloom 0.1 and other v1-only readers reject. Because v1 reading and the
display of v2 kinds both depend on the numbering rule of
`Document::list_number`, that rule is part of the format contract.

Grapheme boundaries are part of the format contract. Textloom pins
unicode-segmentation 1.13.3 (Unicode 17.0) so dependency resolution cannot silently
change valid span boundaries. Audit any future segmentation update for old-file
compatibility; introduce a format migration if old boundaries become invalid.
The fixtures in `tests/fixtures/` were assembled from the documented layouts
independently of the encoder. `native-v1.tlfr` must keep decoding to the same
content, with the same displayed numbers, and re-encode as its version 2
equivalent; `native-v2.tlfr` must decode and re-encode to identical bytes. Quote,
fill, and link rows are covered by layout and rejection tests. Keep
malformed-input regression tests when changing the codec.

## Prepare and verify

1. Update `Cargo.toml`, dependency examples in `README.md`, and `CHANGELOG.md`.
   Replace `Unreleased.` in the version's changelog entry with the release date;
   the entry must not mention "unreleased" anywhere else.
   `scripts/release-version.sh` enforces this for `scripts/check-release.sh --tag`
   and the tag workflow, so both fail until the date is recorded.
   Refresh the package entry in `Cargo.lock` with `cargo check --offline`.
2. Run `./scripts/check-release.sh --allow-dirty` while reviewing pending changes,
   adding `--tag v1.0.0` once the changelog entry is dated. This requires a
   matching changelog entry and checks formatting, every feature combination,
   doctests, Clippy, documentation, benchmark smoke runs with
   `TEXTLOOM_BENCH_QUICK=1`, and headless examples. It also checks formatting
   and strict Clippy for the native host and its CLI/report/clipboard regression
   tests without opening a window.
   It then runs all-feature tests and compiles
   every target from the extracted Cargo package. An explicit inventory check
   requires every packaged source, test, fixture, headless example, benchmark,
   and release document before those checks, so accidentally omitting an entire
   target directory cannot silently pass. A temporary copy of the repository's
   native host is also compiled against the extracted library with its unchanged
   lockfile; this leaves the verified crate untouched.
   Its formatting check uses stable rustfmt; also run `./scripts/format.sh --check`,
   which needs nightly rustfmt for the Google Rust Style Guide's import and macro
   formatting options and covers every package in the repository. CI runs both.
3. Review `cargo package --locked --list --allow-dirty`. Source, headless examples,
   tests, fixtures, benchmark documentation, changelog, and license must be
   included. The repository-only native host, workflows, and local build output
   must be absent. Include patterns are anchored to the package root so nested
   QA README/license files are excluded. The release script rejects repository-only
   directories in the extracted crate as well as requiring the intended inventory.
4. Commit the release changes, then run `./scripts/check-release.sh --tag v1.0.0`
   on the clean checkout. Run `cargo +1.95.0 test --locked --all-features` and
   `cargo +1.95.0 check --locked --all-targets --manifest-path examples/native-editor/Cargo.toml --target-dir target`.
   Confirm the MSRV feature checks, minimal dependency versions, dependency
   policy, and macOS/Windows jobs pass on that commit; the MSRV and native jobs
   compile and test the standalone native host as well as the library targets,
   and macOS/Windows run strict Clippy on both.
   Confirm required Linux X11, macOS, and Windows Mesa renderer smoke checks pass
   in editable, read-only, and disabled modes with rich clipboard enabled, plus
   editable-mode clipboard fixture read-back and undo/redo. Validate the fresh
   content-free reports against the run's source revision; pull-request reports
   identify the checked-out merge revision. Native platform interaction signoff
   remains required independently. Confirm the hosted macOS TextEdit and Windows
   NVDA interaction jobs also pass, with strict interaction report validation and
   owned-process cleanup. They do not certify audible speech or physical input.
5. Exercise a native host on Linux, macOS, and Windows. Check clicks and focus
   loss during composition (one commit of the visible preedit), Japanese/Chinese
   IME replacement and candidate placement in long scrolled documents,
   physical-keyboard dead keys and AltGr, platform and AZERTY digit-row shortcuts,
   touch scrolling and touchpad taps, plain/rich clipboard
   transport and external-application HTML/plain interoperability,
   read-only/disabled widgets, high DPI, and screen-reader selection/replacement.
   Check fill and font colors, soft line breaks, link editing and activation,
   block quotes, and list numbering controls, including their screen-reader
   presentation and HTML paste into Word or TextEdit.
   Exercise the winit adapter rows in a host-supplied winit harness.
   Headless tests validate routing and semantics; hosts supply these platform
   services. Record the host/platform results in the release notes. Any failed
   host behavior claimed by this release is a release blocker. Use the native
   signoff protocol in `examples/native-editor/NATIVE_QA.md` and record unavailable
   checks explicitly. Windows VM and hosted automation are available; physical
   device checks and audible reader signoff remain open. A release candidate
   may be published with them open if its
   GitHub release lists them; 1.0.0 may not.

Re-apply `.github/branch-protection.json` to `main` whenever its required checks
change, as they did with the "Dependency policy (bans licenses sources)" job:

```sh
gh api --method PUT repos/Cameron-Lyons/textloom/branches/main/protection \
  --input .github/branch-protection.json
```

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

For a release candidate, substitute its tag in these steps and in
`check-release.sh --tag`, and mark its GitHub release as a pre-release:

```sh
git tag -a v1.0.0-rc.2 -m 'Textloom 1.0.0-rc.2'
git push origin v1.0.0-rc.2
```

Cargo selects a release candidate only for requirements that name one, such as
`textloom = "1.0.0-rc.2"`.

Crates.io versions cannot be overwritten. Fix a published defect in a new patch
release; consider yanking a defective version where appropriate. Do not move an
existing release tag.
