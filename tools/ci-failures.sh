#!/bin/sh
# Print what failed in a GitHub Actions run of this repository: each failed
# job, then its failed tests, or its failed step when no test name is in the log.
#
# It reads each job's log with the jobs API, which serves a job's log as soon as
# the job ends. `gh run view --log-failed` serves nothing until the whole run
# ends. The repository comes from `gh repo view`.
#
# Reads libtest output (`test NAME ... FAILED` and the `failures:` list) and the
# output of tools/luatest.sh, which prints `--- lua/tests/NAME.lua` before each
# script and stops at the first failure. A Lua failure has two shapes: a
# `lua: FILE:LINE: MESSAGE` error, and a bare `lua/tests/NAME.lua:LINE: MESSAGE`
# line from a script that exits with status 1. Both are reported under the last
# script header.
#
# Usage: sh tools/ci-failures.sh [--wait] RUN_ID_OR_BRANCH
#        sh tools/ci-failures.sh --parse < LOG
#   --wait   wait for the run to end first (gh run watch --exit-status)
#   --parse  print the failures found in a log on stdin; no network
#
# A branch names its latest run of the workflow called CI.
#
# Exit status:
#   0  every job succeeded or was skipped
#   1  a job failed, timed out, was canceled or did not start (also when
#      --wait saw the run fail); wins over 3
#   2  usage error, or a gh call failed
#   3  no job failed, and a job is still running
# POSIX sh. Needs gh, which mise.toml pins.
set -eu

# The failures in a job log on stdin, one per line, each once:
#   test NAME
#   lua SCRIPT: MESSAGE
parse() {
    awk '
    function emit(line) {
        if (!(line in seen)) { seen[line] = 1; print line }
    }
    {
        line = $0
        gsub(/\r/, "", line)
        gsub(/\033\[[0-9;]*[A-Za-z]/, "", line)
        sub(/^[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9:.]+Z ?/, "", line)

        if (line ~ /^--- .*\.lua$/) { script = substr(line, 5); in_list = 0; next }
        if (line ~ /^lua: /) {
            emit("lua " (script == "" ? "?" : script) ": " substr(line, 6))
            in_list = 0
            next
        }
        if (script != "" && index(line, script ":") == 1 &&
            substr(line, length(script) + 2) ~ /^[0-9]+: /) {
            emit("lua " script ": " line)
            in_list = 0
            next
        }
        if (line ~ /^test .* \.\.\. FAILED/) {
            name = line
            sub(/^test /, "", name)
            sub(/ \.\.\. FAILED.*$/, "", name)
            emit("test " name)
            in_list = 0
            next
        }
        if (line == "failures:") { in_list = 1; next }
        if (in_list && line ~ /^    [^ ]+$/) {
            name = line
            sub(/^ +/, "", name)
            emit("test " name)
            next
        }
        in_list = 0
    }'
}

die() {
    printf 'ci-failures: %s\n' "$1" >&2
    exit 2
}

if [ "${1:-}" = "--parse" ]; then
    parse
    exit 0
fi

WAIT=0
if [ "${1:-}" = "--wait" ]; then
    WAIT=1
    shift
fi
[ $# -eq 1 ] || die "usage: sh tools/ci-failures.sh [--wait] RUN_ID_OR_BRANCH"
TARGET=$1

command -v gh >/dev/null 2>&1 || die "no gh on PATH. Run: mise exec -- sh tools/ci-failures.sh ..."

ERRFILE=$(mktemp)
trap 'rm -f "$ERRFILE"' EXIT INT TERM

# gh with its stderr kept in $ERRFILE, apart from its output.
ghq() {
    gh "$@" 2>"$ERRFILE"
}

# The first line of what gh printed on stderr.
gherr() {
    sed -n 1p "$ERRFILE"
}

REPO=$(ghq repo view --json nameWithOwner --jq .nameWithOwner) ||
    die "gh repo view: $(gherr)"

case $TARGET in
    *[!0-9]*)
        RUN=$(ghq run list --repo "$REPO" --branch "$TARGET" --workflow CI --limit 1 \
            --json databaseId --jq '.[0].databaseId // empty') ||
            die "gh run list: $(gherr)"
        [ -n "$RUN" ] || die "no CI run on branch $TARGET"
        ;;
    *) RUN=$TARGET ;;
esac

WATCH=0
if [ "$WAIT" -eq 1 ]; then
    # The jobs below say why a run failed; the status says that it did.
    ghq run watch "$RUN" --repo "$REPO" --exit-status --compact >&2 || WATCH=$?
fi

printf 'run %s on %s\n' "$RUN" "$REPO"

# One row per job: ID, conclusion ("running" while it has none), name.
JOBS=$(ghq api "repos/$REPO/actions/runs/$RUN/jobs" --paginate \
    --jq '.jobs[] | [.id, (.conclusion // "running"), .name] | @tsv') ||
    die "gh api runs/$RUN/jobs: $(gherr)"
[ -n "$JOBS" ] || die "run $RUN has no jobs"

TAB=$(printf '\t')
FAILED=0
RUNNING=0
while IFS=$TAB read -r id conclusion name; do
    printf '%s: %s\n' "$conclusion" "$name"
    case $conclusion in
        success | skipped) continue ;;
        running)
            RUNNING=1
            continue
            ;;
    esac
    FAILED=1

    note=
    log=$(ghq api --allow-escape-sequences "repos/$REPO/actions/jobs/$id/logs") || {
        note="log unavailable: $(gherr)"
        log=
    }
    [ -z "$note" ] || printf '  %s\n' "$note"
    found=$(printf '%s\n' "$log" | parse)
    if [ -n "$found" ]; then
        printf '%s\n' "$found" | sed 's/^/  /'
    else
        steps=$(ghq api "repos/$REPO/actions/jobs/$id" \
            --jq '.steps[] | select(.conclusion != null and .conclusion != "success" and .conclusion != "skipped") | .name') ||
            die "gh api jobs/$id: $(gherr)"
        if [ -n "$steps" ]; then
            printf '%s\n' "$steps" | sed 's/^/  step: /'
        else
            printf '  no failed test or step found\n'
        fi
    fi
done <<EOF2
$JOBS
EOF2

[ "$FAILED" -eq 0 ] || exit 1
[ "$WATCH" -eq 0 ] || exit 1
[ "$RUNNING" -eq 0 ] || exit 3
exit 0
