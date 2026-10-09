# Hosted native interaction probes

The separate [Native interaction QA workflow](../../.github/workflows/native-qa.yml)
runs these probes on disposable macOS and Windows GitHub-hosted desktops. Each
probe requires a built native example and a full `TEXTLOOM_QA_REVISION`. They
refuse ordinary desktop invocation. Use the
[native signoff protocol](../../examples/native-editor/NATIVE_QA.md) for human
testing on the intended release platforms.

The macOS probe sends native keys through the runner's pre-authorized
`osascript`/System Events path. A small AppKit inspector checks clipboard
generations and representations independently. TextEdit receives native HTML
and returns RTF containing the expected Unicode text, emphasis, ordered list,
and exact color. A separate TextEdit rich source checks the host's plain-text
fallback. Native undo/redo, focus changes, read-only input, and disabled input
are also exercised. This probe does not run VoiceOver or an IME.

The Windows probe creates a fresh portable copy and private profile of pinned
NVDA 2026.2 after checking the official installer digest and signature. Native
keystrokes perform reader commands, caret movement, selection, replacement,
undo, and redo. The original [capture plugin](nvda/README.md) observes generated
speech queued for synthesis and reads NVDA's text provider. Its silent
synthesizer does not establish audible speech quality. It does not run an IME.

Drivers validate strict content-free native host reports with
`check_native_report.py --interaction`, then independently assert the observed
behavior. Their companion evidence and reader captures contain only public
deterministic fixture text. Owned windows and processes are identified by PID;
cleanup never uses a global reader quit or unrelated process termination.
The workflow archives `native-qa-observations/` even when a probe fails.

These results supplement renderer CI. Physical keyboards, Japanese/Chinese
IMEs on macOS and Windows, VoiceOver, and complete manual platform signoff
remain separate release checks. An automated pass never changes the native
report's `manual_signoff: "not_recorded"` field.

The PowerShell driver and macOS probe are covered by the repository MIT
license. The original NVDA plugin is GPL-2.0-or-later with its license in
[`nvda/LICENSE`](nvda/LICENSE). All these QA files and the standalone native
host are excluded from the published Textloom crate.
