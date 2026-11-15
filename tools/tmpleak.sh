#!/bin/sh
# Run the workspace tests with a private temp folder and fail when the run
# leaves anything in it or a process that names it.
#
# Tests that create folders under $TMPDIR must remove them, pass or fail, and
# the chaos tests must leave no child process. A leak in one run would skew the
# next and fill the shared temp folder.
#
# The build runs under the normal temp folder, because the linker and xcrun
# keep their own caches there. Only the test run gets the private folder.
#
# POSIX sh, for Linux and macOS hosts: it needs pgrep and pkill. Extra arguments go to `cargo test` after `--workspace`.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

PRIVATE=$(mktemp -d "${TMPDIR:-/tmp}/nanoprobe-tmpleak.XXXXXX")
# `pgrep -f` and `pkill -f` match the whole command line; every process a test
# starts names its folder there. The sweep runs however the script exits.
trap 'pkill -KILL -f "$PRIVATE" 2>/dev/null || true; rm -rf "$PRIVATE"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

status=0
(cd "$ROOT" && cargo test --workspace --no-default-features --no-run "$@") ||
    status=$?
if [ "$status" -eq 0 ]; then
    (cd "$ROOT" && TMPDIR=$PRIVATE cargo test --workspace --no-default-features "$@") ||
        status=$?
fi

left=$(ls -A "$PRIVATE")
if [ -n "$left" ]; then
    printf 'tests left files in the temp folder:\n%s\n' "$left" >&2
    status=1
fi

# pgrep exits 1 when nothing matches; any other failure is an error.
rc=0
running=$(pgrep -fl "$PRIVATE") || rc=$?
if [ "$rc" -gt 1 ]; then
    printf 'pgrep failed with status %s\n' "$rc" >&2
    status=1
elif [ -n "$running" ]; then
    printf 'tests left processes running:\n%s\n' "$running" >&2
    status=1
fi

exit "$status"
