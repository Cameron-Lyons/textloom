# Native interaction signoff

Use this protocol for the physical-keyboard and desktop checks in the root
[`RELEASING.md`](../../RELEASING.md). CI renderer reports and automated native
interaction probes provide supporting evidence. Record the observed outcome of
each applicable check separately; the host's JSON report always leaves manual
signoff unrecorded.

Start from a clean checkout of the candidate under review. On Linux or macOS:

```sh
TEXTLOOM_QA_REVISION="$(git rev-parse HEAD)" \
  cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --rich-clipboard --qa-report native-qa-editable.json
```

On Windows PowerShell:

```powershell
$env:TEXTLOOM_QA_REVISION = git rev-parse HEAD
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- --rich-clipboard --qa-report native-qa-editable.json
```

Choose an unused report path; the host refuses to start if the file exists or
cannot be created. Supply `--font PATH` when the default fonts do not
cover the scripts being tested. Close the window normally to write the report.
Run read-only and disabled checks in fresh sessions using `--read-only` or
`--disabled` and distinct report paths.

Use these two public test paragraphs when a check needs clipboard text:

```text
café 日本語 é 👩🏽‍💻
second
```

Record the exact UTF-8 text when comparing copied content. The `é` above contains
an `e` followed by U+0301. For shortcuts below, use Command on macOS and Control
on Linux/Windows. Use Command+Shift+Z for macOS redo; Control+Y or Control+Shift+Z
for Linux/Windows redo.

For macOS navigation, also check Option+Left/Right for words,
Option+Up/Down for paragraph boundaries (including repeated presses and Shift
selection), and Control+A/E, B/F, P/N for paragraph edges, graphemes, and vertical
movement. Control+H/D delete one grapheme; Option+Delete deletes one word.
Command+Delete deletes only to the current visual row's beginning, including
inside a wrapped paragraph or after Shift+Enter. Undo must restore the exact
text and selection. Repeat deletion in read-only mode to confirm it is blocked.

| Check | Steps | Required observation |
| --- | --- | --- |
| Selection, typing, and history | Focus Document, select all, type a short word, undo, redo, then undo again. Repeat ordinary adjacent typing at a caret. | One undo restores the exact original rich content and selection after uninterrupted typing without paragraph breaks, including replacement followed by adjacent typing. Redo restores the complete edit. Separate history entries require a documented grouping boundary, such as navigation, a style change, or focus loss. |
| Platform navigation | Move by character/word and line/document boundaries; extend selections with Shift; use platform paragraph-start deletion. | Caret and selection land at the expected grapheme boundaries, including combining marks and emoji. Undo restores deletion and selection. |
| Formatting | Select mixed bold/plain text, toggle bold, then italic/underline. Undo and redo. | The whole selection toggles uniformly; rendered text and toolbar agree after history changes. |
| Unicode plain paste | Paste the fixture above, copy it to a plain text editor, undo, and redo. | Exact Unicode text and paragraph breaks survive. Paste is one undo step. |
| Paste as plain text | Copy styled text from Textloom, select different text, and press Ctrl/Command+Shift+V using native keys. Repeat with an external application's plain-text offer, in default and rich clipboard modes. Undo each paste. | Text is pasted exactly once without source formatting or links; destination typing style applies. One undo restores the complete original selection and rich content. A synthetic paste event alone does not validate the shortcut. |
| External rich copy | Format part of the fixture with bold/italic, a font color, and a fill, put the second paragraph in a list, and copy into a rich editor such as TextEdit or Word. | Text, emphasis, list structure, color, and fill survive the receiving application's supported HTML import. Record the application/version and any unsupported style. |
| External plain fallback | Copy rich text from the external editor into Textloom, then undo and redo. | Without a matching TLFR offer, plain text is inserted exactly; the host does not claim arbitrary HTML import. |
| Japanese IME | Select text, type `nihon` with a Japanese IME, navigate native candidates, and commit. Repeat after moving to another paragraph. | Candidate popup follows the composing caret; the chosen text replaces the captured selection; one undo restores the original. |
| Chinese IME | Repeat with a Chinese Pinyin IME and `nihao`, choosing a nondefault candidate before committing. | Candidate selection, placement, replacement, and undo behave correctly. |
| Composition cancellation | Begin preedit over a selection, cancel it using the IME's cancellation key, then resume plain typing. | Source text/selection become visible again without an edit; resumed input replaces the retained selection correctly. Undo restores it. |
| Composition focus loss | Begin preedit, switch to another application, then return. Repeat a click into Find and back into Document, followed by plain typing and a new composition. | The input method either commits the preedit once or cancels it; record which occurred. No duplicate or misplaced text appears, and canceled composition adds no history entry. Plain typing and a new composition work in the intended field after both round trips. |
| Search routing | Compose and commit Japanese/Chinese in Find and Replace. Wait briefly after preedit and after commit. | Only the focused field changes; Document content, selection, and history stay unchanged until an explicit search/replacement action. No continuous preedit/redraw feedback occurs while idle. |
| Physical dead keys/Compose | Switch to a dead-key layout and immediately type a dead key followed by a base letter, with no warm-up key. Repeat within the same layout. Where supported, test a configured Compose sequence. | The first and subsequent composed characters appear exactly once, no shortcut fires, and undo removes the input correctly. Record layout/sequence and report a lost first dead key separately from successful later input. |
| Physical AltGr | Use an actual AltGr layout, for example German AltGr+Q and AltGr+E. Copy the result and undo all resulting history entries. | Exact `@€` appears without formatting/navigation shortcuts; undo restores the original document and selection. Record the layout and actual keys used. |
| Fill and font color | Select text and click Fill Color (Yellow initially), choose another standard color from its menu, then No Fill. Repeat with Font Color (Red initially) and Automatic. Undo and redo each step. Select filled text, then use Find on a word inside it. | The fill paints behind the glyphs, and each split button repeats the last standard color chosen. No Fill and Automatic remove only their color. Each command is one undo step. The selection, including a found match, stays visible over the fill. |
| Soft line breaks | Press Shift+Enter inside a numbered item and inside a heading, then type. Move the caret across the break with arrows, Home/End, and Up/Down, and extend a selection over it. Read the lines with the platform reader. Copy into a plain text editor, and paste into Word or TextEdit. | The paragraph keeps its kind and number, and no new marker appears. Caret, selection, and Home/End/Up/Down follow the visual lines. The reader reads each line separately. Plain copy shows a line break; the rich editor shows a line break within one paragraph. |
| Links | Select text, press Ctrl/Command+K, enter an `https://` URL, and apply. Put the caret inside the link, edit its URL with the Link button, then click Remove link. Undo and redo each step. Hover the link with and without Ctrl/Command held, and Ctrl/Command+click it; then check Read only, click it, and uncheck Read only. Link text to `javascript:alert(1)` and Ctrl/Command+click it. With the platform reader, navigate to a link and activate it. Copy linked text into Word or TextEdit. | Add, edit, and remove are one undo step each and keep the selection. Hovering shows the target; the pointer becomes a pointing hand only while Ctrl/Command is held, or in read-only mode. Ctrl/Command+click and the read-only click open the default browser without moving the caret. The `javascript:` target is not opened, and the status message explains why. The reader announces a link and activates it. The receiving application keeps the hyperlink. |
| Block quotes | In light and dark themes (`--theme light` and `--theme dark`), choose Quote for two consecutive paragraphs. Press Enter at the end of the quote and type, press Enter on an empty quote paragraph, then Backspace at the start of a quote paragraph. Read the quote with the platform reader. Copy it into Word or TextEdit. | One continuous bar spans the consecutive quote paragraphs in both themes. Enter continues the quote; Enter on an empty quote paragraph and Backspace at a quote's start make it body text in one undo step. The reader announces a block quote and reads through it. The receiving application shows a block quote. |
| Numbering controls | Choose Numbered list for three paragraphs, change the middle one to Body, and change it back. Put the caret in the third item and click Restart at 1, then Continue numbering. Press Ctrl/Command+Shift+7 on the paragraph right after the list. Copy the second and third items into Word or TextEdit. Undo each step. | Markers and the toolbar's "Item N." label agree: body text interrupts the list without ending it (1, 2 around the body paragraph), and changing it back restores 1, 2, 3. Restart at 1 and Continue numbering change only the focused item, each in one undo step. The shortcut extends the list. The receiving application shows the source numbers 2 and 3. |
| Read-only | In a fresh read-only session select/copy text; try typing, deletion, paste, cut, undo, and IME composition. | Selection/copy work; editing does not change the document. Cut copies without deleting. Formatting and replacement controls follow the mode. |
| Disabled | In a fresh disabled session try to focus Document, select, type, paste, and compose. | Document receives no keyboard focus or edits. Find/Replace remain separate controls. |
| Runtime mode changes | Toggle Read only or Enabled during preedit, then restore editable mode. | Composition cancels without an unintended edit; the restored mode accepts input appropriately. |
| Touch and touchpad input | Swipe an unfocused document, focus it, then select text by touch. Tap to place the caret and activate a read-only link; also tap elsewhere during preedit. | Unfocused swipes scroll; focused interaction selects as intended. Taps place the caret or activate the intended link, and a preedit commits at most once. Virtual pointer events do not certify physical touch or touchpad behavior. |
| Reader focus and text | With the platform reader active, focus Document and read its text, then navigate its caret. | The reader announces the document control and exposes the correct Unicode text/caret. Record the actual reader/version and commands. |
| Reader selection and replacement | Select text through the reader's supported navigation, type over it, undo, redo, and undo again. | Selection is exposed and announced appropriately; replacement and history restore exact content and selection. Record whether replacement used native typing or a direct accessibility operation. |
| Display scale | Repeat caret placement, selection, candidate placement, and scrolling at the intended high-DPI scale. If supported, move between displays with different scales. | Text, pointer hit testing, caret, popup, and scroll position agree throughout scale changes. |

The hosted check on 2026-10-23 confirmed that TextEdit 1.20 (415) on macOS
26.6.2 (25G83) displays the native two-paragraph HTML quote like ordinary body
text. Window captures and character positions were compared with plain HTML
and a known indented RTF control before copy/save. The corrected exporter
subsequently passed on the same configuration in hosted run 40559669028 on
2026-10-27: both native quote paragraphs retained 36-point first-line and
subsequent-line indents, with before-copy character positions and screenshots
confirming the difference from plain HTML. This closes that specific failure;
IME, VoiceOver, physical-device, and display-scale signoff still require the
checks above. See [current macOS release evidence](../../RELEASING.md#macos-readiness-refreshed-2026-10-27).

## Winit adapter

The egui host does not exercise `textloom::adapter::winit`. Use the repository's
[`winit harness`](../../scripts/windows-vm/winit-harness/) or the consuming
application, and record its source revision separately. Check typing at launch
without switching applications first, followed by the first Japanese/Chinese
composition. Candidate placement must follow the caret before and after an
application switch and at the bottom of a scrolled document.

With a Russian layout, use Control and the physical A, C, V, and Z keys to
select, copy, paste, and undo. On Windows also exercise Ctrl+Insert,
Shift+Delete, Shift+Insert, AltGr+Q/E, and Ctrl+Alt+Q on a German layout. On
Linux verify that Ctrl+Alt+T reaches the host without inserting text. Repeat
startup and composition checks on macOS; a successful egui session does not
cover the winit adapter's focus and `sync_ime` integration.

## Signoff record

For each platform, attach a record with these fields:

```text
Candidate full commit:
Clean checkout confirmed:
OS/version and display backend:
Native host egui/eframe/winit versions:
Display scale(s) and supplied font(s):
Physical keyboard/layout and dead-key/AltGr sequences:
Japanese/Chinese IME names and versions:
Screen reader name/version and commands:
External clipboard application/version:
QA report paths:
Passed checks:
Failed or unavailable checks, exact steps, and observations:
Tester/date:
```

The pinned host versions are in [`Cargo.lock`](Cargo.lock). A failed behavior
claimed by this release is a release blocker. Resolve failures and repeat the
affected checks on the corrected candidate before publication.
