#!/usr/bin/env bash
# Run from any directory. Nothing is published or tagged by this script.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

allow_dirty=false
release_tag=
while (($#)); do
    case "$1" in
        --allow-dirty) allow_dirty=true ;;
        --tag)
            if (($# < 2)); then
                echo '--tag requires a version tag, such as v1.0.0' >&2
                exit 2
            fi
            release_tag=$2
            shift
            ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
    shift
done

if ! $allow_dirty && [[ -n "$(git status --porcelain)" ]]; then
    echo 'Commit the release changes first, or use --allow-dirty to validate pending edits.' >&2
    exit 1
fi

package_id=$(cargo pkgid --offline)
version=${package_id##*[#@]}
if [[ -n "$release_tag" && "$release_tag" != "v$version" ]]; then
    echo "Release tag $release_tag does not match package version v$version" >&2
    exit 1
fi
if ! grep -Fxq "## $version" CHANGELOG.md; then
    echo "CHANGELOG.md has no release entry for $version" >&2
    exit 1
fi

export CARGO_TERM_COLOR=${CARGO_TERM_COLOR:-always}
cargo fmt --all -- --check
# Test every feature combination: cfg-dependent behavior can differ from all-features.
for features in '' egui winit accesskit egui,winit egui,accesskit winit,accesskit egui,winit,accesskit; do
    cargo test --locked --no-default-features --features "$features"
done
cargo clippy --locked --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="${RUSTDOCFLAGS:+$RUSTDOCFLAGS }-D warnings" cargo doc --locked --all-features --no-deps
cargo bench --locked --all-features --bench editing --bench rendering
cargo run --locked --example rich_text
cargo run --locked --example egui_editor --features egui
cargo run --locked --example winit_input --features winit

package_args=(--locked --all-features)
if $allow_dirty; then
    package_args+=(--allow-dirty)
fi
# Test the files that users receive, including packaged fixtures and examples.
package_target_dir=${CARGO_TARGET_DIR:-target}
cargo package "${package_args[@]}" --target-dir "$package_target_dir"
package_manifest="$(cd -- "$package_target_dir" && pwd)/package/textloom-$version/Cargo.toml"
cargo test --locked --all-features --manifest-path "$package_manifest" --target-dir "$package_target_dir"
cargo check --locked --all-features --all-targets --manifest-path "$package_manifest" --target-dir "$package_target_dir"
echo "Textloom $version automated release checks passed. No release has been published."
echo 'Before publishing, complete native host validation and clean-commit CI as described in RELEASING.md.'
