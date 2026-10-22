#!/usr/bin/env bash
# Formats every Cargo package in the repository, or checks formatting with
# --check. Besides `rustfmt.toml`, this applies the Google Rust Style Guide
# options that only nightly rustfmt supports, so it needs the nightly toolchain
# with rustfmt.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

check_args=()
while (($#)); do
    case "$1" in
        --check) check_args=(--check) ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

nightly_options=imports_granularity=Module,format_macro_matchers=true
manifests=(
    Cargo.toml
    examples/native-editor/Cargo.toml
    scripts/qa-tools/Cargo.toml
    scripts/windows-vm/winit-harness/Cargo.toml
)
for manifest in "${manifests[@]}"; do
    cargo +nightly fmt --manifest-path "$manifest" --all -- ${check_args[@]+"${check_args[@]}"} --config "$nightly_options"
    cargo fmt --manifest-path "$manifest" --all -- ${check_args[@]+"${check_args[@]}"}
done
