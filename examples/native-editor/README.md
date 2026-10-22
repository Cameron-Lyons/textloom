# Native editor example

This standalone eframe host opens a native window with Textloom's egui widget.
Its renderer, clipboard, window input, and platform accessibility dependencies
are isolated from the library's feature and dependency graph. The host is a
repository example; it is not part of the published library archive.

From a repository checkout:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

The toolbar demonstrates uniform/mixed selection styles, inline formatting,
fill and font colors, links, headings, block quotes, lists, indentation,
undo/redo, and read-only/disabled modes. Find supports case
and whole-word options; replace-all is one undo step.

- **Fill Color** and **Font Color** are Excel-style split buttons: the swatch
  button applies the last color chosen, initially Yellow and Red, and the arrow
  opens a menu with **No Fill** or **Automatic** and Excel's ten standard colors.
- **Link** and Ctrl/Command+K open a URL field prefilled with the selected link.
  AltGr and combinations with Alt or both Control and macOS Command do not
  trigger the shortcut.
  Applying it links the selection, or edits the link at the caret; with the
  caret outside a link it inserts the URL as linked text. An empty URL removes
  the link, and an invalid URL reports an error. **Remove link** removes the
  selected link or the link at the caret. Editing or removing a selection inside
  one link applies to the whole link in one undo step.
- Ctrl/Command+click on a link, or a click in read-only mode, opens it in the
  system browser or mail client. Only `http`, `https`, and `mailto` targets
  open; others show a status message.
- The paragraph-kind choice includes **Quote**. **Numbered list** extends a
  list ending right before the selection or starts one at 1, as
  Ctrl/Command+Shift+7 does. When the caret is in a numbered item, the toolbar
  shows its number (**Item N.**) and offers **Restart at 1** and **Continue
  numbering**, which continues the list before it, even across body text. Each
  is one undo step.
- Enter starts a new paragraph, continuing a list or quote; Shift+Enter starts a
  new line within the paragraph.

Capture/restore keeps a native rich snapshot in memory. Restoring clears
editing history. Use `--read-only` to start with editing disabled while
retaining selection and copy.
Use `--disabled` to start with the document widget disabled, including focus,
selection, and clipboard interaction. The independent **Enabled** and **Read only**
checkboxes let you change these modes during an IME session. Formatting,
undo/redo, replacement, and snapshot restoration controls follow the document's
editing mode. Toolbar, find, and replace commands wait for the document to process
queued IME input, then end any remaining composition, committing its visible preedit
as clicking outside the document does; an empty preedit is canceled. Undo/redo and
snapshot capture follow the same ordering. Choosing a list type keeps the item's
nesting level, and choosing the current paragraph type keeps list numbering.
**Document**, **Find**, and **Replace** labels are associated with their widgets
for native accessibility.

The host provides the system's plain-text clipboard and native IME caret area.
egui-winit 0.37 places the native cursor area at the composing caret rectangle,
so native candidate windows follow the caret within a multiline document.
After its text fields process IME input, the host retires those events to avoid
repeated unchanged cursor-area requests and Fcitx preedit/redraw feedback.
Moving focus from the document to Link, Find, or Replace suppresses one immediate
identical commit echoed by the old composition. This also covers deferred keyboard
focus traversal; fresh input or a new composition in the field ends the guard.
AccessKit focus changes run in separate egui passes, preserving text before and
after a handoff between the document and native fields. Longer sequences continue
on the next repaint if they reach egui's pass limit.
On Linux, AccessKit's Unix adapter 0.24 activates with current AT-SPI services,
gives the application its desktop parent, and exposes `EditableText`.
Plain clipboard behavior remains the default. Add `--rich-clipboard` to enable
the example's native rich backend on Linux (Wayland/X11), macOS, or Windows;
other platforms build the host but reject the flag:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --rich-clipboard
```

Rich copies publish plain text, HTML, and a length-framed TLFR fragment with a
per-publication generation token. Linux and Windows use the custom format
`application/x-textloom-fragment`; macOS maps it to `org.textloom.fragment`.
HTML uses `text/html` on Linux, `public.html` on macOS, and CF_HTML on Windows.
The token identifies a publication for read-back verification; it is not a
secret or a change to the library's TLFR format.

Native operations run in helper processes with hard deadlines and bounded
transfers. Native lengths are checked before allocating Rust payload buffers;
TLFR is limited to 64 MiB, plus the 32-byte transport envelope. Copy succeeds
only after reading back the new rich, HTML, and plain representations. Errors
retain egui's plain copy fallback. A pipe-worker startup failure also terminates
and reaps its helper before falling back. A rich paste uses one captured native item,
validates its fragment and plain alternative, and inserts it in one undo step.
The item is captured when the paste arrives and retained if focus routing defers
delivery, so later clipboard changes cannot replace its formatting.
Unavailable or malformed rich data, mismatched text, empty events, and ambiguous
multiple paste events retain the original paste event's plain fallback. The
backend exports HTML for other applications; it does not import arbitrary HTML.

The host retains one verified clipboard owner helper, reaps replaced or timed-out
helpers, joins pipe workers, and retires its owner on close. Linux serves clipboard
requests from that owner process; preserving its item after closing the host
depends on a clipboard manager. Linux helpers also exit when the host process
dies, including when it is killed or aborts. macOS and Windows publish eager native data.
All platform wrappers and their dependencies stay in this example's separate
workspace, and its Rust code continues to forbid unsafe code.

Egui's default fonts have limited script coverage, so the host also loads
installed system fonts for scripts and emoji they lack, as eframe does by
default. To test a specific font, supply a TTF/OTF/TTC file through
`--font PATH`; `--bold-font PATH` registers the widget's `Bold` family. These
flags do not install or change system fonts.
For example, on Linux with Noto CJK and Liberation installed:

```sh
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --font /usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc \
  --bold-font /usr/share/fonts/liberation/LiberationSans-Bold.ttf
```

Run `--smoke-test` to request a graceful close after 20 native frames. The host
may render an additional frame while the native close request is acknowledged.
Smoke mode requires a working display and graphics driver; the local release
script checks the host without opening a window, while renderer CI opens isolated
native windows on all three platforms. Smoke mode returns a failure if the widget
reports an editing or accessibility error. It does not automate IME
preedit/candidate windows or screen readers.

`--clipboard-self-test` requires `--rich-clipboard` and **writes a deterministic
fixture to the system clipboard**. Use it only in an isolated test session. It
verifies rich/HTML/plain publication and read-back, then widget rich insertion and one-step
undo/redo. The fixture is a formatted, colored, and Yellow-filled numbered list. It can be combined with `--smoke-test` and `--qa-report`; successful
completion prints `Native rich clipboard roundtrip and undo/redo passed`.
Renderer CI requests rich clipboard in all three modes and this self-test in
editable mode. Isolated widget fixtures passed on Linux X11/Wayland, macOS, and
Windows; the root `RELEASING.md` records source revisions and validation evidence.
External-application interoperability still needs manual platform signoff.

Hosted interaction probes additionally verify macOS TextEdit HTML/RTF clipboard
interoperability and native editing guards, and Windows NVDA generated speech,
selection, replacement/history, and graceful close. macOS clipboard HTML declares
UTF-8 so TextEdit decodes Unicode correctly. See the
[probe guide](../../scripts/native-qa/README.md) for scope and evidence; physical
desktop signoff remains required.

For native QA, pass `--qa-report PATH` and close the window normally after testing.
The host creates PATH before opening its window and exits with status 2 if the
file already exists or cannot be created, so an unusable path cannot discard a
session. It writes the JSON report only on graceful exit; after an abnormal exit
the file stays empty, which report validation rejects. A report write failure or
smoke-test failure returns a nonzero exit status. Choose a new file for each run.
Without this flag, the host records no report and writes no document file.

The report contains OS/architecture, egui/native display scale, font-supply flags,
session times, raw host input event counts, observed selection, typing-style,
composition, focus and document-revision changes, error counts, and final document
size, selection offsets, formatting and undo/redo metrics. It also verifies an
in-memory TLFR encode/decode round trip and records the HTML export size. Document,
clipboard, search and IME preedit text, font paths, and window titles are omitted.
Host input counts include toolbar fields and are observations of arriving events;
they do not establish that the document accepted an input or that a manual check
passed. State counters count observed transitions between UI passes, so multiple
commands in one pass may appear as one change. The report always records
`manual_signoff: "not_recorded"`; attach your test steps, IME/screen-reader versions,
OS version/display backend and outcomes separately as described in `RELEASING.md`.
Exact eframe/egui/winit versions are in this example's `Cargo.lock`.
The final focus state distinguishes native window focus from retained widget
keyboard focus; `focused` requires both. Raw window focus events help diagnose
input producers or compositor focus changes separately from widget focus changes.
The session records `rich_clipboard_requested`, `clipboard_self_test_requested`,
and `clipboard_self_test_passed`; `errors.clipboard_self_test` counts a failed
requested fixture check. An unrun check remains unpassed. These flags describe
the automated fixture, not external-application or manual platform signoff.

Set `TEXTLOOM_QA_REVISION` when compiling to include the source revision in the
report. It is `null` when omitted. Use the final clean commit for release signoff;
label exploratory builds with local changes accordingly. On Linux or macOS:

```sh
TEXTLOOM_QA_REVISION="$(git rev-parse HEAD)" \
  cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- \
  --qa-report native-qa-linux.json
```

On Windows PowerShell:

```powershell
$env:TEXTLOOM_QA_REVISION = git rev-parse HEAD
cargo run --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target -- --qa-report native-qa-windows.json
```

You can combine `--qa-report` with `--smoke-test`, `--rich-clipboard`, `--read-only`,
`--disabled`, and the optional font flags. Run host option/report and clipboard
transport regression tests without opening a window:

```sh
cargo test --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

Before publishing, exercise the native checks in the root `RELEASING.md` on each
supported host/platform and record the results. The
[native signoff protocol](NATIVE_QA.md) provides exact steps and a result record
for physical keyboards, IMEs, readers, and external clipboard applications.
Do not route raw winit editing
events to Textloom alongside this egui host: eframe already supplies those events
through egui.
