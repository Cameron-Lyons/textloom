#!/usr/bin/env bash
# Exercise the tag gate with isolated changelogs and no dependency resolution.
set -euo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

test_root=$(mktemp -d "${TMPDIR:-/tmp}/textloom-release-version.XXXXXX")
trap 'rm -rf -- "$test_root"' EXIT
mkdir -p "$test_root/scripts" "$test_root/bin"
cp scripts/release-version.sh "$test_root/scripts/"
cat > "$test_root/bin/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ $# == 2 && $1 == pkgid && $2 == --offline ]]
printf '%s\n' 'path+file:///fixture#textloom@1.2.3'
EOF
chmod +x "$test_root/bin/cargo"
export PATH="$test_root/bin:$PATH"

checks=0
write_changelog() {
    printf '%s\n' "$@" > "$test_root/CHANGELOG.md"
}
expect_success() {
    local label=$1 output
    shift
    if ! output=$(bash "$test_root/scripts/release-version.sh" "$@" 2>&1); then
        printf 'FAIL: %s\n%s\n' "$label" "$output" >&2
        exit 1
    fi
    if [[ "$output" != 1.2.3 ]]; then
        printf 'FAIL: %s: unexpected output %s\n' "$label" "$output" >&2
        exit 1
    fi
    checks=$((checks + 1))
}
expect_failure() {
    local label=$1 expected=$2 output
    shift 2
    if output=$(bash "$test_root/scripts/release-version.sh" "$@" 2>&1); then
        printf 'FAIL: %s unexpectedly passed\n' "$label" >&2
        exit 1
    fi
    if [[ "$output" != *"$expected"* ]]; then
        printf 'FAIL: %s: unexpected error %s\n' "$label" "$output" >&2
        exit 1
    fi
    checks=$((checks + 1))
}

write_changelog '# Changelog' '' '## 1.2.3' '' '2026-10-31. Release notes.'
expect_success 'dated entry' v1.2.3
expect_failure 'tag mismatch' 'does not match package version' v1.2.4
expect_failure 'extra arguments' 'Usage:' v1.2.3 extra

write_changelog '# Changelog' '' '## 1.2.2' '' '2026-10-31.'
expect_failure 'missing version entry' 'has no release entry'

write_changelog '# Changelog' '' '## 1.2.3' '' 'Release notes without a date.'
expect_success 'untagged development entry'
expect_failure 'missing release date' 'YYYY-MM-DD release date' v1.2.3

write_changelog '# Changelog' '' '## 1.2.3' '' 'Unreleased.'
expect_success 'untagged unreleased entry'
expect_failure 'unreleased version entry' 'still marks 1.2.3 as Unreleased' v1.2.3

write_changelog '# Changelog' '' '## Unreleased' '' '- Pending fix.' '' '## 1.2.3' '' '2026-10-31.'
expect_success 'untagged pending changes'
expect_failure 'pending changes on a tag' 'pending Unreleased changes' v1.2.3

write_changelog '# Changelog' '' '## Unreleased' '' '## 1.2.3' '' '2026-10-31.'
expect_success 'empty unreleased placeholder' v1.2.3
write_changelog '# Changelog' '' '## Unreleased' '' '### Added' '' '### Fixed' '' '## 1.2.3' '' '2026-10-31.'
expect_success 'empty unreleased subsections' v1.2.3
write_changelog '# Changelog' '' '## UNRELEASED' '' '- Pending fix.' '' '## 1.2.3' '' '2026-10-31.'
expect_failure 'uppercase unreleased section' 'pending Unreleased changes' v1.2.3

write_changelog '# Changelog' '' '## 1.2.3' '' '2026-10-31. Release notes.' '' 'Some changes remain unreleased.'
expect_failure 'unreleased status after date' 'still marks 1.2.3 as Unreleased' v1.2.3
write_changelog '# Changelog' '' '## 1.2.3' '' 'Release notes.' '' '## 1.2.2' '' '2026-10-31.'
expect_failure 'date from another version does not count' 'YYYY-MM-DD release date' v1.2.3

for release_date in 2026-00-01 2026-13-01 2026-01-00 2026-04-31 2025-02-29 1900-02-29 0000-01-01; do
    write_changelog '# Changelog' '' '## 1.2.3' '' "$release_date."
    expect_failure "invalid date $release_date" 'invalid release date' v1.2.3
done
for release_date in 2024-02-29 2000-02-29 2026-12-31; do
    write_changelog '# Changelog' '' '## 1.2.3' '' "$release_date"
    expect_success "valid date $release_date" v1.2.3
done

write_changelog '# Changelog' '' '## 1.2.3' '' '2026-10-31. Release notes.' '' '## 1.2.2' '' 'Unreleased.'
expect_success 'date and status are scoped to the selected entry' v1.2.3

printf 'Release version gate: %s checks passed.\n' "$checks"
