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

The VM uses Microsoft's [Windows 11 Enterprise LTSC evaluation](https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise),
which expires 90 days after installation; recreate the VM afterwards.
`autounattend.xml` accepts its license terms during the unattended install.
Microsoft publishes no checksum for it; the copy downloaded on 2026-10-22
(5,112,850,432 bytes) had SHA-256
`67cec5865eaa037a72ddc633a717a10a2bed50778862267223ddb9c60ef5da68`.

## Setup

```sh
scripts/windows-vm/winvm.py create      # downloads the ISO and installs Windows (about 30 minutes)
scripts/windows-vm/winvm.py provision   # IMEs, layouts, Mesa, PowerShell 7, NVDA (20 to 40 minutes)
```

The VM lives in `~/.local/share/textloom-winvm` (`TEXTLOOM_WINVM` overrides
it). `create` saves an `installed` snapshot and `provision` a `provisioned`
one; `winvm.py restore provisioned` returns to a clean state. Set
`TEXTLOOM_WINVM_MEMORY` (MiB) and `TEXTLOOM_WINVM_CPUS` to change the defaults
of 4096 and 4.

Provisioning activates the evaluation (until then it reports an expired
license and shuts down hourly) and adds US English, the Microsoft Japanese
IME, Microsoft Pinyin (with Bing cloud candidates off, so its first-run prompt
cannot take a check's keys), German, French (AZERTY), and Russian input
methods; MSYS2's Mesa llvmpipe renderer, which the host uses as the hosted CI
jobs do, and Python for the report validator; PowerShell 7; and NVDA 2026.2,
the installer the hosted reader probe verifies. NVDA does not start at logon.

## Running checks

```sh
scripts/windows-vm/winvm.py up            # --view shows the screen; --audio pipewire plays sound
scripts/windows-vm/checks.py build        # cross-compile the host and the winit harness
scripts/windows-vm/checks.py deploy
scripts/windows-vm/checks.py all          # or name checks; `checks.py list` describes them
scripts/windows-vm/winvm.py down
```

Results accumulate in `results/results.json` under the VM directory, with
screenshots per check. By default guest audio is recorded to `audio.wav` there;
`nvda-audible-speech` saves the speech it measured for listening.

The checks cover the egui native host (dead keys and AltGr, Windows
clipboard keys, AZERTY digit-row shortcuts, Japanese and Chinese IME
composition, candidate placement, and commits on focus changes, IME in a long
scrolled document, the hosted NVDA reader probe, and audible NVDA speech) and
textloom's winit adapter through `winit-harness/`, a minimal winit host that
logs every event and its editor state as JSON lines. IME candidate lists are
neither windows nor UI Automation elements, so they are located by comparing
screenshots. Touch input, precision touchpads, and physical keyboards still
need real hardware.

## How it works

`winvm.py` starts QEMU and a job server on `127.0.0.1`. A PowerShell agent,
started at logon in the desktop session, polls the server at `10.0.2.2` (the
host as seen through QEMU user networking), runs each job, and posts its
output; requests carry a per-VM token from `config.json`. `winvm.py run`
sends a job, `keys`, `type`, and `click` send input through QMP, and
`screenshot` captures the screen. Processes the agent starts may take the
foreground, as on a CI runner. `nvda-reader-probe` runs the hosted probe from
a copy of the repository layout and sets the CI variables its disposable-runner
guard requires, since the VM returns to a snapshot. The guest account is `qa` with the password
`textloom-qa`; UAC elevates it without prompting, and automatic updates and
the lock screen are off.
