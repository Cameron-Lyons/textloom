# Windows VM checks

Runs the native host's Windows checks in a local Windows 11 VM under QEMU/KVM,
for maintainers without a Windows machine. QEMU's virtual PS/2 keyboard and
USB tablet send hardware input, so keyboard layouts, dead keys, AltGr, and the
Microsoft IMEs see the scancodes a physical keyboard would produce. Editor
state is read through UI Automation. This is VM evidence: it does not replace
signoff on physical hardware, touch screens, or touchpads.

## Requirements

- Linux with KVM (`/dev/kvm`), 4 GB of free memory, and about 80 GB of disk.
- On Arch: `sudo pacman -S --needed qemu-desktop edk2-ovmf libisoburn mingw-w64-gcc`.
- `rustup target add x86_64-pc-windows-gnu`. The host is cross-compiled with
  MinGW; the hosted CI jobs cover the MSVC toolchain.
- ImageMagick (`magick`) for the IME checks, which compare screenshots.

The VM uses Microsoft's [Windows 11 Enterprise LTSC evaluation](https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise),
which expires 90 days after installation; recreate the VM afterwards.
`autounattend.xml` accepts its license terms during the unattended install.
Microsoft publishes no checksum for it; the copy downloaded on 2026-10-22
(5,112,850,432 bytes) had SHA-256
`67cec5865eaa037a72ddc633a717a10a2bed50778862267223ddb9c60ef5da68`.

## Setup

Both tools are Rust binaries in [`scripts/qa-tools`](../qa-tools), run through
Cargo aliases in `.cargo/config.toml` from the repository root.

```sh
cargo winvm create      # downloads the ISO and installs Windows (about 30 minutes)
cargo winvm provision   # IMEs, layouts, Mesa, PowerShell 7, NVDA (20 to 40 minutes)
```

The VM lives in `~/.local/share/textloom-winvm` (`TEXTLOOM_WINVM` overrides
it). `create` saves an `installed` snapshot and `provision` a `provisioned`
one; `cargo winvm restore provisioned` returns to a clean state. Set
`TEXTLOOM_WINVM_MEMORY` (MiB) and `TEXTLOOM_WINVM_CPUS` to change the defaults
of 4096 and 4.

Provisioning activates the evaluation (until then it reports an expired
license and shuts down hourly) and adds US English, the Microsoft Japanese
IME, Microsoft Pinyin (with Bing cloud candidates off, so its first-run prompt
cannot take a check's keys), German, French (AZERTY), and Russian input
methods; MSYS2's Mesa llvmpipe renderer, which the host uses as the hosted CI
jobs do; PowerShell 7; and NVDA 2026.2, the installer the hosted reader probe
verifies. NVDA does not start at logon.

## Running checks

```sh
cargo winvm up            # --view shows the screen; --audio pipewire plays sound
cargo winvm-checks build  # cross-compile the host, the winit harness, and the report validator
cargo winvm-checks deploy
cargo winvm-checks all    # or name checks; `cargo winvm-checks list` describes them
cargo winvm down
```

Results accumulate in `results/results.json` under the VM directory, with
screenshots per check; `TLQA_WINDOWS_OUT` names another results directory. By
default guest audio is recorded to `audio.wav` in the VM directory;
`nvda-audible-speech` saves the speech it measured for listening.

Use a new `TLQA_WINDOWS_OUT` directory for each evidence run to retain previous
screenshots and native reports; `--runs N` keeps artifacts in separate `run-N`
subdirectories. Failed checks are recorded and the remaining
checks still run; the command exits unsuccessfully if any selected check fails.
Every result records `manual_signoff: "not_recorded"`.

`build` records `target/windows-vm-build.json`: the source HEAD, dirty status,
a SHA-256 inventory of tracked and nonignored untracked files, and hashes of
all three executables. A dirty build embeds a revision such as
`<HEAD>-dirty-<source hash prefix>` in the native reports. The build refuses
to record provenance if source files change while compilation runs; rerun it
after those edits settle. `deploy` verifies the executable hashes and adds
the deployed guest helper's hash. Checks verify these hashes inside the VM
and validate reports against the **deployed build's** revision. Results also
record the test runner's current source inventory separately. A later checkout
or helper edit therefore cannot relabel an older executable as the current
clean commit. Rebuild and redeploy for final clean-commit evidence.

The checks cover the egui native host (dead keys and AltGr, Windows
clipboard keys, AZERTY digit-row shortcuts, Japanese and Chinese IME
composition, candidate placement, and commits on focus changes, IME in a long
scrolled document, the hosted NVDA reader probe, and audible NVDA speech) and
textloom's winit adapter through `winit-harness/`, a minimal winit host that
logs every event and its editor state as JSON lines. IME candidate lists are
neither windows nor UI Automation elements, so they are located by comparing
screenshots. Touch input, precision touchpads, and physical keyboards still
need real hardware.

`german-dead-keys-and-altgr` separately records the first acute-dead-key attempt
immediately after switching layouts, without a preparatory key, in
`german/first-dead-key-diagnostic.json` and its screenshot. Its own `passed`
field can be false even when the warmed-up dead-key/AltGr check passes: winit
0.30.13 consumes the first pending dead key while preparing the new layout
([upstream #4582](https://github.com/rust-windowing/winit/pull/4582)). Keep that
diagnostic result separate when reporting the suite; it is not a waived pass.

The feature checks also cover:

- `rich-colors`: native fill/font palettes, exact UIA foreground/background
  RGB, CF_HTML colors, independent clearing, and lossless TLFR undo/redo.
- `soft-breaks`: Shift+Enter within a paragraph/list item, Enter continuing a
  list, `<br>` export, plain newlines, and native rich paste with undo/redo.
- `links`: Ctrl+K, escaping the HTML target, UIA hyperlink exposure, removal,
  and undo/redo. No navigation to the link is requested.
- `block-quotes`: adjacent quotes, UIA grouping, HTML structure, undo/redo,
  and a pixel-contrast assertion for the quote rule in light and dark themes.
- `numbering`: computed item numbers, restart/continue, and indent/outdent.
- `plain-paste-shortcut`: actual Ctrl+Shift+V on a selected word, exact plain
  replacement with rich source formatting removed, and one-step undo/redo.
- `external-rich-clipboard`: QEMU Ctrl+V into a separate, independently
  rendering Windows HTML editing control, with DOM observations of Unicode,
  emphasis, font/fill colors, soft breaks, links, quotes, and lists. Its native
  Ctrl+C offers external HTML/Unicode without TLFR; Textloom must use exact
  plain fallback, with one-step undo/redo. Arbitrary HTML import is not supported
  by the example host.

The independent clipboard consumer uses the installed Windows
[MSHTML-backed Windows Forms WebBrowser control](https://learn.microsoft.com/en-us/dotnet/desktop/winforms/controls/webbrowser-control-overview)
in a separate STA PowerShell process. It displays an offline editable document,
blocks navigation, and requires no Office license or downloaded application.
The harness only initializes its own external fixture and observes its DOM;
clipboard paste/copy use native keyboard input. This establishes interoperability
with that recorded engine version, not with Word, Outlook, or every Windows
application. Screenshots are retained as rendering evidence, not human visual
signoff. Each new feature session also closes gracefully and saves a validated,
content-free `native-report.json`; bounded public fixture clipboard/DOM captures
are separate from those reports. The peer is stopped during session cleanup.

## How it works

`cargo winvm` starts QEMU and a job server on `127.0.0.1`. A PowerShell agent,
started at logon in the desktop session, polls the server at `10.0.2.2` (the
host as seen through QEMU user networking), runs each job, and posts its
output; requests carry a per-VM token from `config.json`. `cargo winvm run`
sends a job, `keys`, `type`, and `click` send input through QMP, and
`screenshot` captures the screen. Processes the agent starts may take the
foreground, as on a CI runner. `nvda-reader-probe` runs the hosted probe from
a copy of the repository layout, with the deployed Windows build of
`check-native-report`, and sets the CI variables its disposable-runner guard
requires, since the VM returns to a snapshot. The guest account is `qa` with the password
`textloom-qa`; UAC elevates it without prompting, and automatic updates and
the lock screen are off.
