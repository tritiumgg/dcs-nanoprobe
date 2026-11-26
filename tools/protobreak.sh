#!/bin/sh
# Checks the public API schema for breaking changes (AGENTS.md, "Conventions").
#
# The base is a git ref: the first argument, else PROTOBREAK_BASE, else the
# first of origin/main, main and HEAD whose merge base with HEAD holds a
# schema; the check says which. The all-zero commit a push event gives for a
# new branch counts as no ref. CI names the base: the base commit of a pull
# request, or the commit before a push.
#
# The schema is compared at the merge base of HEAD and that ref, never at the
# ref's tip. A branch is checked for what it changes: a field that main gained
# after the branch was cut is not a field the branch removed. On main itself,
# with origin/main behind it, the merge base is origin/main, the published
# schema. When no base holds a schema there is nothing to compare and the
# check passes. A named ref that does not resolve, or that shares no history
# with HEAD, is an error.
set -eu

resolves() {
    git rev-parse --verify --quiet "$1^{commit}" >/dev/null
}

has_schema() {
    git cat-file -e "$1:buf.yaml" 2>/dev/null
}

ref="${1:-${PROTOBREAK_BASE:-}}"
case "$ref" in
    *[!0]*) ;;
    *) ref= ;;
esac

base=
if [ -z "$ref" ]; then
    for candidate in origin/main main HEAD; do
        resolves "$candidate" || continue
        mb=$(git merge-base HEAD "$candidate" 2>/dev/null) || continue
        if has_schema "$mb"; then
            ref=$candidate
            base=$mb
            break
        fi
    done
    if [ -z "$ref" ]; then
        echo "ok  no schema at the merge base of HEAD with origin/main, main or HEAD, nothing to compare"
        exit 0
    fi
elif ! resolves "$ref"; then
    echo "error: $ref is not a commit in this repository" >&2
    exit 1
elif ! base=$(git merge-base HEAD "$ref"); then
    echo "error: $ref shares no history with HEAD" >&2
    exit 1
elif ! has_schema "$base"; then
    echo "ok  no schema at $ref (merge base $(git rev-parse --short "$base")), nothing to compare"
    exit 0
fi

short=$(git rev-parse --short "$base")
echo "comparing the schema against $ref (merge base $short)"
buf breaking --against ".git#ref=$base"
echo "ok  no breaking change against $ref (merge base $short)"
