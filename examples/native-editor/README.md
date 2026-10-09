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
Textloom's rich clipboard API is available for hosts with MIME-aware clipboard
transport; this example uses eframe's plain-text clipboard.

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
Smoke mode requires a working display and graphics driver; the release checks
compile the host without opening
a window. Smoke mode returns a failure if the widget reports an editing or
accessibility error. It does not automate IME preedit/candidate windows, clipboard
transport, or screen readers.

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

You can combine `--qa-report` with `--smoke-test`, `--read-only`, `--disabled`, and
the optional font flags. Run host option/report tests without opening a window:

```sh
cargo test --locked --manifest-path examples/native-editor/Cargo.toml --target-dir target
```

Before publishing, exercise the native checks in the root `RELEASING.md` on each
supported host/platform and record the results. Do not route raw winit editing
events to Textloom alongside this egui host: eframe already supplies those events
through egui.
