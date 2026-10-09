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
headings, lists, indentation, undo/redo, and read-only/disabled modes. Find supports case
and whole-word options; replace-all is one undo step. Capture/restore keeps a
native rich snapshot in memory. Restoring clears editing history.
Use `--read-only` to start with editing disabled while retaining selection and copy.
Use `--disabled` to start with the document widget disabled, including focus,
selection, and clipboard interaction. The independent **Enabled** and **Read only**
checkboxes let you change these modes during an IME session. Formatting,
undo/redo, replacement, and snapshot restoration controls follow the document's
editing mode. **Document**, **Find**, and **Replace** labels are associated with
their widgets for native accessibility.

The host provides the system's plain-text clipboard and native IME caret area.
Plain clipboard behavior remains the default. Add `--rich-clipboard` to enable
the example's native rich backend on Linux (Wayland/X11), macOS, or Windows:

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
retain egui's plain copy fallback. A rich paste uses one captured native item,
validates its fragment and plain alternative, and inserts it in one undo step.
Unavailable or malformed rich data, mismatched text, empty events, and ambiguous
multiple paste events retain the original paste event's plain fallback. The
backend exports HTML for other applications; it does not import arbitrary HTML.

The host retains one verified clipboard owner helper, reaps replaced or timed-out
helpers, joins pipe workers, and retires its owner on close. Linux serves clipboard
requests from that owner process; preserving its item after closing the host
depends on a clipboard manager. macOS and Windows publish eager native data.
All platform wrappers and their dependencies stay in this example's separate
workspace, and its Rust code continues to forbid unsafe code.

Egui's default fonts have limited script coverage. Supply a TTF/OTF/TTC font
with the scripts being tested through `--font PATH`; `--bold-font PATH` registers
the widget's `Bold` family. These flags do not install or change system fonts.
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
verifies rich/HTML/plain publication and read-back, then rich insertion and one-step
undo/redo. It can be combined with `--smoke-test` and `--qa-report`; successful
completion prints `Native rich clipboard roundtrip and undo/redo passed`.
Renderer CI requests rich clipboard in all three modes and this self-test in
editable mode. The isolated Linux X11 fixture has passed locally. Wayland,
macOS, and Windows rich runtime checks and final clean CI remain pending;
external-application interoperability still needs manual platform signoff.

For native QA, pass `--qa-report PATH` and close the window normally after testing.
The host writes a JSON report only on graceful exit. Existing paths are protected
from overwrite; a report write failure or smoke-test failure returns a nonzero
exit status. Choose a new file for each run and an existing parent directory.
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
supported host/platform and record the results. Do not route raw winit editing
events to Textloom alongside this egui host: eframe already supplies those events
through egui.
