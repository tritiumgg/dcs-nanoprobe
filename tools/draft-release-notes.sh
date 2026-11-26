#!/bin/sh
# Draft a version's section of docs/release-notes.md from the commits since the
# last release tag. A maintainer edits the draft and merges it into the notes.
#
# The commits are the first-parent history of HEAD after <from-ref>. A commit
# on that line is one change. A merge commit stands for the commits of its
# second parent, so a history with merges and one with squashed commits both
# work. These commits are skipped: merges, `docs(adr): number ...`, and the
# types docs, test, ci, chore, build, style and wip, unless the commit is
# breaking. A subject that is not a Conventional Commit, and a revert, gives
# its whole subject and the TODO.
#
# An entry comes from the commit's `Release-note:` trailers. Each trailer is
# one entry, kept as written; indented lines that follow it continue it.
# `Release-note: none` drops the commit. A commit with no trailer gives its
# subject, links to the ADRs and results it names, and `TODO: reword for
# users`, which tools/release-notes.sh refuses, so a draft nobody edited cannot
# ship. A commit of a skipped type with a trailer still gives its entries.
#
# Grouping: feat is Added, fix is Fixed, and everything else, a breaking commit
# (`type!:` or a `BREAKING CHANGE:` line) included, is Changed. A breaking
# commit's subject entry is bold.
#
# The draft goes to standard output under `## <version>, Draft`. Run in the
# repository's checkout.
#
# Usage: sh tools/draft-release-notes.sh <version> [<from-ref>]
#   <from-ref> defaults to the newest v* tag.
# POSIX sh. Needs git and awk.
set -eu

[ $# -ge 1 ] && [ $# -le 2 ] || { printf 'usage: sh tools/draft-release-notes.sh <version> [<from-ref>]\n' >&2; exit 2; }
VERSION=$1
case $VERSION in
    *[!0-9.]* | .* | *. | *..* | *.*.*.* | '') printf 'refused: %s is not a version such as 0.2.0\n' "$VERSION" >&2; exit 1 ;;
    *.*.*) ;;
    *) printf 'refused: %s is not a version such as 0.2.0\n' "$VERSION" >&2; exit 1 ;;
esac
ROOT=$(git rev-parse --show-toplevel)
FROM=${2:-$(git -C "$ROOT" describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)}
[ -n "$FROM" ] || { printf 'refused: no v* tag to start from; pass <from-ref>\n' >&2; exit 1; }
git -C "$ROOT" rev-parse --verify -q "$FROM^{commit}" >/dev/null || { printf 'refused: %s is not a commit\n' "$FROM" >&2; exit 1; }

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM
: >"$TMP/seen"
: >"$TMP/Added"
: >"$TMP/Changed"
: >"$TMP/Fixed"

# Links for the ADRs ("ADR 0044") and results files ("results/x.md") a message
# names, when the file exists. Relative to docs/, as the notes write them.
links() {
    {
        grep -o -E 'ADR [0-9]{4}' "$1" | sort -u | while read -r _ n; do
            for f in "$ROOT"/docs/adr/"$n"-*.md; do
                if [ -f "$f" ]; then printf '[ADR %s](adr/%s)\n' "$n" "$(basename "$f")"; fi
            done
        done
        grep -o -E 'results/[A-Za-z0-9._-]+\.md' "$1" | sort -u | while read -r r; do
            if [ -f "$ROOT/docs/$r" ]; then printf '[%s](%s)\n' "$(basename "$r")" "$r"; fi
        done
    } | paste -s -d ',' - | sed 's/,/, /g'
}

draft() {
    git -C "$ROOT" log -1 --format=%B "$1" >"$TMP/msg"
    awk -v dir="$TMP" -v links="$(links "$TMP/msg")" '
        function flush() {
            if (cur != "") { entries[++n] = cur; cur = "" }
        }
        NR == 1 { subject = $0; next }
        /^Release-note:/ {
            flush()
            v = $0; sub(/^Release-note:[ \t]*/, "", v)
            cur = v; intrailer = 1; next
        }
        /^[ \t]+[^ \t]/ && intrailer { v = $0; sub(/^[ \t]+/, "", v); cur = cur "\n  " v; next }
        { intrailer = 0; flush() }
        /^BREAKING[ -]CHANGE:/ { breaking = 1 }
        END {
            flush()
            if (!match(subject, /^[a-z]+(\([^)]*\))?!?: /)) type = "other"
            else {
                head = substr(subject, 1, RLENGTH - 2)
                type = head; sub(/[(!:].*/, "", type)
                if (head ~ /!$/) breaking = 1
            }
            desc = subject; sub(/^[^:]*: /, "", desc)
            group = (type == "feat") ? "Added" : (type == "fix") ? "Fixed" : "Changed"
            if (breaking) group = "Changed"
            if (n == 0) {
                if (subject ~ /^docs\(adr\): number /) exit
                if (!breaking && (type ~ /^(docs|test|ci|chore|build|style|wip)$/ || subject ~ /^Merge /)) exit
                if (type == "other") desc = subject
                text = (breaking ? "**" desc "**" : desc)
                if (links != "") text = text " (" links ")"
                entries[++n] = text " TODO: reword for users"
            }
            for (i = 1; i <= n; i++) {
                e = tolower(entries[i]); sub(/[ \t.]*$/, "", e)
                if (e == "none") continue
                print "- " entries[i] >> (dir "/" group)
            }
        }
    ' "$TMP/msg"
}

for c in $(git -C "$ROOT" rev-list --first-parent --reverse "$FROM..HEAD"); do
    if [ "$(git -C "$ROOT" rev-list --parents -n 1 "$c" | wc -w | tr -d ' ')" -gt 2 ]; then
        inner=$(git -C "$ROOT" rev-list --no-merges --reverse "$c^1..$c^2")
    else
        inner=$c
    fi
    for i in $inner; do
        grep -q "^$i\$" "$TMP/seen" && continue
        printf '%s\n' "$i" >>"$TMP/seen"
        draft "$i"
    done
done

printf '## %s, Draft\n' "$VERSION"
for g in Added Changed Fixed; do
    [ -s "$TMP/$g" ] || continue
    printf '\n### %s\n\n' "$g"
    cat "$TMP/$g"
done
