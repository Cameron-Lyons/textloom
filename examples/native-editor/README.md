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
headings, lists, indentation, undo/redo, and read-only mode. Find supports case
and whole-word options; replace-all is one undo step. Capture/restore keeps a
native rich snapshot in memory. Restoring clears editing history.
Use `--read-only` to start with editing disabled while retaining selection and copy.

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

Run `--smoke-test` to render 20 native frames and exit. This requires a working
display and graphics driver; the release checks compile the host without opening
a window. Smoke mode returns a failure if the widget reports an editing or
accessibility error. It does not automate IME preedit/candidate windows, clipboard
transport, or screen readers.

Before publishing, exercise the native checks in the root `RELEASING.md` on each
supported host/platform and record the results. Do not route raw winit editing
events to Textloom alongside this egui host: eframe already supplies those events
through egui.
