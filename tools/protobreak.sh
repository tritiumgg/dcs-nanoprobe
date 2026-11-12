#!/bin/sh
# Checks the public API schema for breaking changes against a git ref
# (CLAUDE.md, "Conventions"). The default ref is the published main branch,
# origin/main, or HEAD where there is none. A ref without a schema is skipped.
set -eu

ref="${1:-}"
if [ -z "$ref" ]; then
    if git rev-parse --verify --quiet origin/main >/dev/null; then
        ref=origin/main
    else
        ref=HEAD
    fi
fi

if ! git cat-file -e "$ref:buf.yaml" 2>/dev/null; then
    echo "ok  no schema at $ref, nothing to compare"
    exit 0
fi

buf breaking --against ".git#ref=$ref"
echo "ok  no breaking change against $ref"
