# Hosted native interaction probes

The separate [Native interaction QA workflow](../../.github/workflows/native-qa.yml)
runs these probes on disposable macOS and Windows GitHub-hosted desktops. Each
probe requires a built native example and a full `TEXTLOOM_QA_REVISION`. They
refuse ordinary desktop invocation. Use the
[native signoff protocol](../../examples/native-editor/NATIVE_QA.md) for human
testing on the intended release platforms.

The macOS probe, `macos-interaction` in [`scripts/qa-tools`](../qa-tools),
sends native keys through the runner's pre-authorized
`osascript`/System Events path. A small AppKit inspector checks clipboard
generations and representations independently. TextEdit receives native HTML
and returns RTF containing the expected Unicode text, emphasis, ordered list,
and exact color. A separate TextEdit rich source checks the host's plain-text
fallback. Native undo/redo, focus changes, read-only input, and disabled input
are also exercised.

The same editable session runs 25 focused macOS shortcut checks: Control+A/E
paragraph navigation, Control+B/F grapheme navigation, Control+P/N visual-line
navigation, and Option+Up/Down paragraph navigation, including Shift selections
and selection anchors across empty paragraphs. Independent Command/arrow keys
establish positions and witness individual aliases. Control+H deletes a
decomposed combining grapheme, Control+D deletes a skin-tone ZWJ emoji, and
Command+Backspace deletes to the start of a line following a native
Shift+Return soft break. Each deletion checks the whole document and requires
one native undo to restore its exact public fixture. Every copy requires a
fresh clipboard generation; a separate raw UTF-8 `pbpaste` comparison also
rejects Unicode normalization and changed line endings or trailing newlines.

The companion test caps the shortcut extension at 160 native actions (currently
133). The driver avoids OS-reserved Control+arrow chords.

The editable session also uses PID-scoped accessibility buttons and native keys
to exercise Fill Color, Font Color, No Fill, Automatic, link add/edit/remove,
quote continuation/exit, and numbering restart/continuation. Every asserted
formatting change compares exact copied HTML, with one-step undo/redo where
specified in the observations. Shift+Enter must retain one numbered item or
heading, while plain copy contains LF. TextEdit's actual copied RTF must retain
the fill, foreground, hyperlink, soft separator within one numbered paragraph,
quoted indentation, and a partial list's numbers 2 and 3. These checks avoid
installed font names and pixel coordinates.

A dedicated native Command+Shift+V check starts with a verified colored, linked
Textloom clipboard offer, prepares a plain target without overwriting that
offer, and verifies exact Unicode and unformatted HTML after paste. One native
undo must restore the target and one redo must restore the plain pasted text.
The Linux-run history oracle rejects missing paste, accidental rich paste,
stale copies, and ineffective undo/redo.

Forced-wrap Command+Backspace uses a long, single paragraph. A native
Command+Shift+Left selection first witnesses a nonempty, proper suffix of that
paragraph; deletion must remove exactly that suffix and undo must restore every
byte. This measures the actual visual boundary instead of assuming font metrics.

The feature extension reuses the native host and TextEdit documents. Allow
roughly 5–7 minutes for the complete interaction step (an estimate until the
hosted run); its timeout is eight minutes, including helper compilation and
cleanup. It adds no app launches. `last_feature_html` records the last exact
expected/observed native representation, and `macos-fixture-feature-import.*`
captures the most recent TextEdit import for diagnosing failures. Rust tests
check orchestration and oracles without a desktop; they do not establish that
AppKit, System Events, or TextEdit passed on macOS.

Native winit-adapter startup, IMEs, VoiceOver, link activation/hover, selection
and search painting over fills, quote painting in both themes, and complete
manual feature signoff remain separate. The existing winit harness needs macOS
host/clipboard integration before it can honestly cover the adapter startup
row; this probe continues to exercise the egui host.

The Windows probe creates a fresh portable copy and private profile of pinned
NVDA 2026.2 after checking the official installer digest and signature. Native
keystrokes perform reader commands, caret movement, selection, replacement,
undo, and redo. The original [capture plugin](nvda/README.md) observes generated
speech queued for synthesis and reads NVDA's text provider. Its silent
synthesizer does not establish audible speech quality. It does not run an IME.

Drivers validate strict content-free native host reports with
`check-native-report --interaction` (the Windows driver takes its path as
`-ReportValidator`), then independently assert the observed behavior. Their
companion evidence and reader captures contain only public deterministic
fixture text. Owned windows and processes are identified by PID; cleanup never
uses a global reader quit or unrelated process termination. The workflow
archives `native-qa-observations/` even when a probe fails.

These results supplement renderer CI. Physical keyboards, Japanese/Chinese
IMEs on macOS and Windows, VoiceOver, and complete manual platform signoff
remain separate release checks. An automated pass never changes the native
report's `manual_signoff: "not_recorded"` field.

The PowerShell driver and macOS probe are covered by the repository MIT
license. The original NVDA plugin is GPL-2.0-or-later with its license in
[`nvda/LICENSE`](nvda/LICENSE). All these QA files and the standalone native
host are excluded from the published Textloom crate.
