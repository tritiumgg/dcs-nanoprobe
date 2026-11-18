#!/bin/sh
# Write a version's release notes from docs/release-notes.md, for the release
# .github/workflows/release.yml publishes, and refuse notes that are not
# final.
#
# The notes are the body of the one section headed `## <version>`, with
# optional words after the version. The script refuses when there is no such
# section or more than one, when its heading says "Draft", or when the section
# holds a placeholder: "To be completed", "not yet run", "TBD" or "TODO".
#
# A release page resolves no relative link, so with RELEASE_LINK_BASE set
# (such as https://github.com/<owner>/<repo>/blob/<tag>), each relative link
# becomes absolute: `../x` to <base>/x and `x` to <base>/docs/x.
#
# Run in the repository's checkout.
#
# Usage: sh tools/release-notes.sh <version> <notes-out>
# POSIX sh. Needs git and awk.
set -eu

[ $# -eq 2 ] || { printf 'usage: sh tools/release-notes.sh <version> <notes-out>\n' >&2; exit 2; }
VERSION=$1
NOTES_OUT=$2
ROOT=$(git rev-parse --show-toplevel)

refuse() {
    printf 'refused: %s\n' "$1" >&2
    exit 1
}

case $VERSION in
    *[!0-9.]* | .* | *. | *..* | *.*.*.* | '') refuse "$VERSION is not a version such as 0.1.0" ;;
    *.*.*) ;;
    *) refuse "$VERSION is not a version such as 0.1.0" ;;
esac

NOTES=$ROOT/docs/release-notes.md
[ -f "$NOTES" ] || refuse "no docs/release-notes.md"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

awk -v v="$VERSION" -v out="$TMP/section" -v heads="$TMP/heads" '
    /^## / {
        insec = 0
        title = substr($0, 4)
        t = title
        sub(/^Draft:[[:space:]]*/, "", t)
        if (index(t, v) == 1) {
            rest = substr(t, length(v) + 1)
            if (rest == "" || rest ~ /^[ ,]/) {
                insec = 1
                print title > heads
                next
            }
        }
    }
    insec { print > out }
' "$NOTES"
touch "$TMP/heads" "$TMP/section"
[ -s "$TMP/heads" ] || refuse "docs/release-notes.md has no section headed \"## $VERSION\""
[ "$(wc -l <"$TMP/heads" | tr -d ' ')" = 1 ] || refuse "docs/release-notes.md has more than one section for $VERSION"
case $(cat "$TMP/heads") in
    *Draft* | *draft*) refuse "the notes' heading for $VERSION still says Draft: \"$(cat "$TMP/heads")\"" ;;
esac
if LC_ALL=C grep -n -i -E 'to be completed|not yet run|(^|[^[:alnum:]])(TBD|TODO)([^[:alnum:]]|$)' "$TMP/section" >"$TMP/placeholders"; then
    printf 'refused: the notes for %s hold placeholders (line numbers within the section):\n' "$VERSION" >&2
    cat "$TMP/placeholders" >&2
    exit 1
fi
# Drop the blank lines at the section's start and end.
awk 'NF { for (; blank > 0; blank--) print ""; print; seen = 1; next } seen { blank++ }' "$TMP/section" >"$TMP/body"
[ -s "$TMP/body" ] || refuse "the notes for $VERSION are empty"

awk -v base="${RELEASE_LINK_BASE:-}" '
    function fix(target) {
        if (base == "" || target ~ /^(https?:|mailto:|#|\/)/) return target
        if (substr(target, 1, 3) == "../") return base "/" substr(target, 4)
        return base "/docs/" target
    }
    {
        line = $0
        out = ""
        while (match(line, /\]\([^)]*\)/)) {
            target = substr(line, RSTART + 2, RLENGTH - 3)
            out = out substr(line, 1, RSTART + 1) fix(target) ")"
            line = substr(line, RSTART + RLENGTH)
        }
        print out line
    }
' "$TMP/body" >"$NOTES_OUT"
printf 'ok  the notes for %s are final; written to %s\n' "$VERSION" "$NOTES_OUT"
