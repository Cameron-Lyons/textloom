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
package_target_dir=${CARGO_TARGET_DIR:-target}
native_example_manifest=examples/native-editor/Cargo.toml
cargo fmt --all -- --check
cargo fmt --manifest-path "$native_example_manifest" --package textloom-native-example -- --check
# Test every feature combination: cfg-dependent behavior can differ from all-features.
for features in '' egui winit accesskit egui,winit egui,accesskit winit,accesskit egui,winit,accesskit; do
    cargo test --locked --no-default-features --features "$features"
done
# The repository-only host has CLI/report tests that need no display.
cargo test --locked --manifest-path "$native_example_manifest" --all-targets --target-dir "$package_target_dir"
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo clippy --locked --manifest-path "$native_example_manifest" --all-targets --target-dir "$package_target_dir" -- -D warnings
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
cargo package "${package_args[@]}" --target-dir "$package_target_dir"
package_root="$(cd -- "$package_target_dir" && pwd)/package/textloom-$version"
# Cargo succeeds with zero tests/targets if an include rule drops their entire
# directory. Check the inventory before trusting the extracted-package checks.
required_files=(Cargo.toml Cargo.lock README.md BENCHMARKS.md CHANGELOG.md RELEASING.md LICENSE)
check_package_file() {
    if [[ ! -f "$package_root/$1" ]]; then
        echo "Required release file is missing from the package: $1" >&2
        exit 1
    fi
}
for path in "${required_files[@]}"; do
    check_package_file "$path"
done
# Cargo excludes nested packages; the native host is a repository-only example.
# pipefail also rejects missing/unreadable source directories during discovery.
find src tests examples benches -type d \( -name target -o -path examples/native-editor \) -prune -o -type f -print0 |
    while IFS= read -r -d '' path; do
        check_package_file "$path"
    done
package_manifest="$package_root/Cargo.toml"
cargo test --locked --all-features --manifest-path "$package_manifest" --target-dir "$package_target_dir"
cargo check --locked --all-features --all-targets --manifest-path "$package_manifest" --target-dir "$package_target_dir"
# Compile the repository's native host against the extracted library without
# changing the verified crate or Textloom's runtime dependency graph.
native_package_check_dir=$(mktemp -d "$package_target_dir/native-package-check.XXXXXX")
trap 'rm -rf -- "$native_package_check_dir"' EXIT
cp -R examples/native-editor/src "$native_package_check_dir/src"
cp -R examples/native-editor/vendor "$native_package_check_dir/vendor"
cp examples/native-editor/Cargo.lock "$native_package_check_dir/Cargo.lock"
cp examples/native-editor/README.md "$native_package_check_dir/README.md"
awk -v package_path="../package/textloom-$version" '
    /^textloom = / {
        if (!sub(/path = "\.\.\/\.\."/, "path = \"" package_path "\"")) exit 1
        rewritten = 1
    }
    { print }
    END { if (!rewritten) exit 1 }
' "$native_example_manifest" > "$native_package_check_dir/Cargo.toml"
cargo check --locked --all-targets --manifest-path "$native_package_check_dir/Cargo.toml" --target-dir "$package_target_dir"
echo "Textloom $version automated release checks passed. No release has been published."
echo 'Before publishing, complete native host validation and clean-commit CI as described in RELEASING.md.'
