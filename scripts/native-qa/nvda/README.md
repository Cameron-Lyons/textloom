This directory contains an original NVDA test plugin, licensed under
GPL-2.0-or-later in LICENSE. It does not copy NVDA's GPL speech-spy implementation.
It runs only inside a separately downloaded, pinned official NVDA executable.
These repository QA files are excluded from the published MIT Textloom crate.

The Windows driver creates a fresh portable NVDA directory and private profile,
enables that profile's developer scratchpad, and sends native keyboard commands
to its own editor window. The plugin records strings from NVDA's
`pre_speechQueued` extension and reads NVDA's focused TextInfo provider. It does
not inject narration, change text or selection, or expose a network service.
NVDA's built-in silent synthesizer avoids depending on hosted audio hardware.
The result proves reader-generated speech queued for synthesis, not audible
speech quality, physical keyboard input, or manual release signoff.

The capture includes only the repository's deterministic editor fixture and
one-character replacement. Running this on a user's active desktop is outside
its intended scope. It is for a disposable, isolated hosted Windows GUI job.

Pinned release: [NVDA 2026.2](https://github.com/nvaccess/nvda/releases/tag/release-2026.2),
source commit `f62c980589d1ac30babf68ad48177e9ad29a2e84`, installer SHA-256
`f3f8d29974a88d687b3c4809be192219ec579c5bdabcda5aaf53635288bca824`.
Interfaces were checked against that release's
[speech extensions](https://github.com/nvaccess/nvda/blob/release-2026.2/source/speech/extensions.py),
[configuration schema](https://github.com/nvaccess/nvda/blob/release-2026.2/source/config/configSpec.py),
and [native reader commands](https://github.com/nvaccess/nvda/blob/release-2026.2/source/globalCommands.py).
