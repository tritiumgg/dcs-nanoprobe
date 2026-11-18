#!/bin/sh
# Check tools/release-tag-check.sh and tools/release-notes.sh, which
# .github/workflows/release.yml runs before it builds a release for a tag.
#
# Runs in scratch repositories.
#
# Usage: sh tools/release-check-test.sh
# POSIX sh. Needs git and awk.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TAGCHECK=$ROOT/tools/release-tag-check.sh
NOTES=$ROOT/tools/release-notes.sh

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

FAILED=0
fail() {
    printf 'FAIL  %s\n' "$1" >&2
    FAILED=1
}
pass() {
    printf 'ok    %s\n' "$1"
}

repo() {
    git init -q "$1"
    git -C "$1" config user.name test
    git -C "$1" config user.email test@example.invalid
    git -C "$1" config commit.gpgsign false
    git -C "$1" config tag.gpgsign false
    printf '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "%s"\n' "$2" >"$1/Cargo.toml"
    git -C "$1" add Cargo.toml
    git -C "$1" commit -q -m "version $2"
    git -C "$1" branch -q -M main
}

# The exit code of a command run in a folder; its output goes to $SCRATCH/out.
code() {
    dir=$1
    shift
    rc=0
    (cd "$dir" && "$@") >"$SCRATCH/out" 2>&1 || rc=$?
    printf '%s' "$rc"
}

# --- tools/release-tag-check.sh

T=$SCRATCH/tag
repo "$T" 0.1.0
git -C "$T" update-ref refs/remotes/origin/main HEAD
git -C "$T" tag v0.1.0
got=$(cd "$T" && sh "$TAGCHECK" v0.1.0) && [ "$got" = 0.1.0 ] &&
    pass "a tag of the workspace version on main passes and prints the version" ||
    fail "the tag v0.1.0 on main: $got"
# The tags exist, so only the version comparison can refuse them.
git -C "$T" tag v0.2.0
git -C "$T" tag 0.1.0
[ "$(code "$T" sh "$TAGCHECK" v0.2.0)" = 1 ] && grep -q 'the workspace version' "$SCRATCH/out" &&
    pass "a tag of another version is refused" ||
    fail "the tag v0.2.0 for version 0.1.0: $(cat "$SCRATCH/out")"
[ "$(code "$T" sh "$TAGCHECK" 0.1.0)" = 1 ] && grep -q 'the workspace version' "$SCRATCH/out" &&
    pass "a tag without the v is refused" ||
    fail "the tag 0.1.0: $(cat "$SCRATCH/out")"
git -C "$T" tag -d v0.1.0 >/dev/null
[ "$(code "$T" sh "$TAGCHECK" v0.1.0)" = 1 ] && grep -q 'no tag v0.1.0' "$SCRATCH/out" &&
    pass "a tag that does not exist is refused" ||
    fail "a missing tag: $(cat "$SCRATCH/out")"
git -C "$T" tag v0.1.0
N=$SCRATCH/noversion
repo "$N" 0.1.0
printf '[workspace]\nmembers = []\n' >"$N/Cargo.toml"
[ "$(code "$N" sh "$TAGCHECK" v0.1.0)" = 2 ] && grep -q 'no version' "$SCRATCH/out" &&
    pass "a Cargo.toml with no workspace version is an error" ||
    fail "no workspace version: $(cat "$SCRATCH/out")"
git -C "$T" checkout -q -b side
printf 'x\n' >"$T/side"
git -C "$T" add side
git -C "$T" commit -q -m side
git -C "$T" tag -d v0.1.0 >/dev/null
git -C "$T" tag v0.1.0
[ "$(code "$T" sh "$TAGCHECK" v0.1.0)" = 1 ] && grep -q 'not on main' "$SCRATCH/out" &&
    pass "a tag of a commit that is not on main is refused" ||
    fail "a tag off main passed: $(cat "$SCRATCH/out")"
git -C "$T" update-ref -d refs/remotes/origin/main
[ "$(code "$T" sh "$TAGCHECK" v0.1.0)" = 1 ] && grep -q 'no origin/main' "$SCRATCH/out" &&
    pass "with no origin/main the tag is refused" ||
    fail "no origin/main: $(cat "$SCRATCH/out")"

# --- tools/release-notes.sh

P=$SCRATCH/notes
repo "$P" 0.1.0
mkdir -p "$P/docs"
cat >"$P/docs/release-notes.md" <<'EOF'
# Release notes

## 0.1.0, the first release

The first release. The [README](../README.md) and [the guide](install.md), with
[a result](results/r.md), [a page](https://example.com/x) and [a part](#files).

### Files

| File | Holds |

## 0.0.9

Older. To be completed.
EOF
OUT=$SCRATCH/notes.md
base=https://github.com/o/r/blob/v0.1.0
if [ "$(code "$P" env RELEASE_LINK_BASE=$base sh "$NOTES" 0.1.0 "$OUT")" = 0 ]; then
    pass "final notes pass"
else
    fail "final notes were refused: $(cat "$SCRATCH/out")"
fi
[ "$(sed -n 1p "$OUT")" = "The first release. The [README]($base/README.md) and [the guide]($base/docs/install.md), with" ] &&
    sed -n 2p "$OUT" | grep -qF "[a result]($base/docs/results/r.md), [a page](https://example.com/x) and [a part](#files)." &&
    pass "the notes are the version's section, each relative link made absolute" ||
    fail "the notes were written as: $(cat "$OUT")"
tail -n 1 "$OUT" | grep -qF '| File | Holds |' && ! grep -q 'Older' "$OUT" &&
    pass "the notes end where the next version's section begins" ||
    fail "the notes run past their section: $(cat "$OUT")"
(cd "$P" && sh "$NOTES" 0.1.0 "$SCRATCH/plain.md") >/dev/null 2>&1 &&
    sed -n 1p "$SCRATCH/plain.md" | grep -qF '[README](../README.md)' &&
    pass "without RELEASE_LINK_BASE the links stay as written" ||
    fail "the links changed with no base: $(cat "$SCRATCH/plain.md")"

# Each refusal, on a copy of the passing notes changed in one way.
refused() {
    # refused <what> <grep pattern> <sed program for docs/release-notes.md>
    rm -rf "$SCRATCH/p2"
    cp -R "$P" "$SCRATCH/p2"
    sed "$3" "$P/docs/release-notes.md" >"$SCRATCH/p2/docs/release-notes.md"
    if [ "$(code "$SCRATCH/p2" sh "$NOTES" 0.1.0 "$SCRATCH/n2")" = 1 ] &&
        grep -q -- "$2" "$SCRATCH/out"; then
        pass "$1 are refused"
    else
        fail "$1 were not refused as expected: $(cat "$SCRATCH/out")"
    fi
}
# A bare heading is the version's section; a longer version is not.
rm -rf "$SCRATCH/p2"
cp -R "$P" "$SCRATCH/p2"
sed -e 's/^## 0.1.0, the first release/## 0.1.0/' -e 's/^## 0.0.9/## 0.1.01/' \
    "$P/docs/release-notes.md" >"$SCRATCH/p2/docs/release-notes.md"
if [ "$(code "$SCRATCH/p2" sh "$NOTES" 0.1.0 "$SCRATCH/n2")" = 0 ] &&
    grep -q 'The first release' "$SCRATCH/n2" && ! grep -q 'Older' "$SCRATCH/n2"; then
    pass "a bare heading is the section, and a heading of 0.1.01 is not"
else
    fail "a bare heading or 0.1.01: $(cat "$SCRATCH/out") $(cat "$SCRATCH/n2" 2>/dev/null)"
fi
refused "notes headed Draft" "still says Draft" \
    's/^## 0.1.0, the first release/## Draft: 0.1.0, the first release/'
refused "notes that say not yet run" "hold placeholders" \
    's/^| File | Holds |/| Windows | not yet run |/'
refused "notes that say To be completed" "hold placeholders" \
    's/^The first release\./To be completed./'
refused "notes with a TODO" "hold placeholders" \
    's/^### Files/### Files (TODO)/'
refused "notes with a TBD" "hold placeholders" \
    's/^| File | Holds |/TBD/'
refused "notes headed draft in lower case" "still says Draft" \
    's/^## 0.1.0, the first release/## 0.1.0 (draft)/'
refused "notes with no section for the version" "no section headed" \
    's/^## 0.1.0, the first release/## 0.1, the first release/'
refused "notes with two sections for the version" "more than one section" \
    's/^## 0.0.9/## 0.1.0 again/'
refused "notes with an empty section" "are empty" \
    '/^## 0.1.0/,/^## 0.0.9/{/^## /!d;}'
rm -rf "$SCRATCH/p2"
cp -R "$P" "$SCRATCH/p2"
rm "$SCRATCH/p2/docs/release-notes.md"
[ "$(code "$SCRATCH/p2" sh "$NOTES" 0.1.0 "$SCRATCH/n2")" = 1 ] && grep -q 'no docs/release-notes.md' "$SCRATCH/out" &&
    pass "a checkout with no release notes is refused" ||
    fail "no release notes: $(cat "$SCRATCH/out")"
rm -rf "$SCRATCH/p2"
bad_ok=1
for bad in 0.1 0.1.0.1 v0.1.0 '0.1.0;x' ''; do
    if [ "$(code "$P" sh "$NOTES" "$bad" "$SCRATCH/n2")" != 1 ] || ! grep -q 'not a version' "$SCRATCH/out"; then
        fail "the version \"$bad\" was not refused: $(cat "$SCRATCH/out")"
        bad_ok=0
    fi
done
[ "$bad_ok" = 0 ] || pass "a version that is not three numbers is refused"

# The repository's own notes for its workspace version parse: they pass, or
# they are refused for a placeholder or a Draft heading, never for a missing
# or doubled section.
version=$(awk '
    /^\[workspace\.package\]/ { in_sec = 1; next }
    /^\[/ { in_sec = 0 }
    in_sec && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print; exit
    }
' "$ROOT/Cargo.toml")
rc=$(code "$ROOT" sh "$NOTES" "$version" "$SCRATCH/own.md")
if [ "$rc" = 0 ] || grep -q -e 'hold placeholders' -e 'still says Draft' "$SCRATCH/out"; then
    pass "docs/release-notes.md has one section for $version ($(tail -n 1 "$SCRATCH/out" | cut -c1-60))"
else
    fail "docs/release-notes.md for $version: $(cat "$SCRATCH/out")"
fi

[ "$FAILED" = 0 ] || exit 1
