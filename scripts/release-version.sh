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
    pending_entry=$(awk '
        tolower($0) == "## unreleased" { found = 1; next }
        found && /^## / { exit }
        found && NF && $0 !~ /^### / { print }
    ' CHANGELOG.md)
    if [[ -n "$pending_entry" ]]; then
        echo "CHANGELOG.md has pending Unreleased changes; move them into the $version entry first" >&2
        exit 1
    fi
    entry=$(awk -v heading="## $version" '
        $0 == heading { found = 1; next }
        found && /^## / { exit }
        found { print }
    ' CHANGELOG.md)
    if grep -qiw unreleased <<<"$entry"; then
        echo "CHANGELOG.md still marks $version as Unreleased; record the release date first" >&2
        exit 1
    fi
    release_line=$(awk 'NF { print; exit }' <<<"$entry")
    if [[ ! "$release_line" =~ ^([0-9]{4})-([0-9]{2})-([0-9]{2})($|[[:space:].]) ]]; then
        echo "CHANGELOG.md must start the $version entry with its YYYY-MM-DD release date" >&2
        exit 1
    fi
    year=$((10#${BASH_REMATCH[1]}))
    month=$((10#${BASH_REMATCH[2]}))
    day=$((10#${BASH_REMATCH[3]}))
    days_in_month=(0 31 28 31 30 31 30 31 31 30 31 30 31)
    if ((year % 4 == 0 && (year % 100 != 0 || year % 400 == 0))); then
        days_in_month[2]=29
    fi
    if ((year == 0 || month < 1 || month > 12 || day < 1 || day > days_in_month[month])); then
        echo "CHANGELOG.md has an invalid release date for $version" >&2
        exit 1
    fi
fi
echo "$version"
