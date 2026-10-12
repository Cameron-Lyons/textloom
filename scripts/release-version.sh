#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

if (($# > 1)); then
    echo 'Usage: scripts/release-version.sh [vX.Y.Z]' >&2
    exit 2
fi
release_tag=${1-}

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
if [[ -n "$release_tag" ]]; then
    entry=$(awk -v heading="## $version" '
        $0 == heading { found = 1; next }
        found && /^## / { exit }
        found { print }
    ' CHANGELOG.md)
    if grep -qiw unreleased <<<"$entry"; then
        echo "CHANGELOG.md still marks $version as Unreleased; record the release date first" >&2
        exit 1
    fi
fi
echo "$version"
