# QA tools

Rust tools for Textloom's native QA, outside the published crate. The crate
has its own lockfile and workspace, like the native example.

| Binary | Use |
| --- | --- |
| `check-native-report` | Validates a native host `--qa-report` file: strict, content-free fields, and the expected revision, OS, mode, and clipboard configuration. CI runs it after every renderer smoke run. It never certifies manual QA. |
| `macos-interaction` | The hosted macOS interaction probe; see [`../native-qa`](../native-qa/README.md). It refuses to run outside a GitHub Actions macOS runner. |
| `winvm` | Creates and runs the local Windows 11 VM; see [`../windows-vm`](../windows-vm/README.md). |
| `winvm-checks` | Runs the Windows checks in that VM. |

```sh
cargo run --manifest-path scripts/qa-tools/Cargo.toml --bin check-native-report -- \
    report.json --revision "$(git rev-parse HEAD)" --os linux --mode editable
cargo winvm status                    # aliases in .cargo/config.toml, from the repository root
cargo test --manifest-path scripts/qa-tools/Cargo.toml
```

The tests need no desktop or VM. They cover the report rules, replay the macOS
shortcut sequences against an editor oracle, and run the VM job server against
a simulated guest. One test uses ImageMagick when it is installed. Only
`check-native-report` builds on Windows; the other tools need a Unix host.
