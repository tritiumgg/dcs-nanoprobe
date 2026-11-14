#!/bin/sh
# Checks the public API schema for breaking changes against a git ref
# (CLAUDE.md, "Conventions"). With no argument, or the all-zero commit a push
# event gives for a new branch, the base is the first of origin/main, main
# and HEAD that holds a schema, and the check says which. When none does,
# there is nothing to compare and it passes. A named ref that does not
# resolve is an error.
set -eu

resolves() {
    git rev-parse --verify --quiet "$1^{commit}" >/dev/null
}

has_schema() {
    resolves "$1" && git cat-file -e "$1:buf.yaml" 2>/dev/null
}

ref="${1:-}"
case "$ref" in
    *[!0]*) ;;
    *) ref= ;;
esac

if [ -z "$ref" ]; then
    for candidate in origin/main main HEAD; do
        if has_schema "$candidate"; then
            ref=$candidate
            break
        fi
    done
    if [ -z "$ref" ]; then
        echo "ok  no schema at origin/main, main or HEAD, nothing to compare"
        exit 0
    fi
elif ! resolves "$ref"; then
    echo "error: $ref is not a commit in this repository" >&2
    exit 1
elif ! has_schema "$ref"; then
    echo "ok  no schema at $ref, nothing to compare"
    exit 0
fi

echo "comparing the schema against $ref"
buf breaking --against ".git#ref=$ref"
echo "ok  no breaking change against $ref"
