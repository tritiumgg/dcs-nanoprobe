#!/bin/sh
# Check tools/ci-failures.sh: its log parser on written samples of a Windows
# `cargo test --no-fail-fast` log and a Linux `mise run check` log that
# interleaves libtest and tools/luatest.sh output, and its job loop, step
# fallback and exit status against a stub `gh`. The samples are made up. No
# network is used.
#
# Usage: sh tools/ci-failures-test.sh
# POSIX sh.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

fail() {
    printf 'ci-failures-test: %s\n' "$1" >&2
    exit 1
}

parse() {
    sh "$ROOT/tools/ci-failures.sh" --parse <"$1"
}

ESC=$(printf '\033')
TS=2026-01-02T03:04:05.1234567Z

# Windows: timestamps, CRLF, failing tests in two binaries, a failures list.
printf '%s\r\n' \
    "$TS      Running unittests src/lib.rs (target\\debug\\deps\\alpha-1.exe)" \
    "$TS test ring::tests::wraps ... ok" \
    "$TS test ring::tests::splits ... FAILED" \
    "$TS " \
    "$TS failures:" \
    "$TS " \
    "$TS ---- ring::tests::splits stdout ----" \
    "$TS thread panicked at src/ring.rs:9:5:" \
    "$TS " \
    "$TS failures:" \
    "$TS     ring::tests::splits" \
    "$TS " \
    "$TS test result: FAILED. 1 passed; 1 failed; 0 ignored" \
    "$TS error: test failed, to rerun pass \`-p alpha --lib\`" \
    "$TS test codec::tests::empty ... FAILED" \
    "$TS test codec::tests::full ... ok" \
    "$TS error: 2 targets failed:" \
    >"$SCRATCH/windows.log"

out=$(parse "$SCRATCH/windows.log")
want="test ring::tests::splits
test codec::tests::empty"
[ "$out" = "$want" ] || fail "windows log: got: $out"

# Linux: libtest with colors, a Lua script that passes, one that fails with a
# `lua:` error, one that exits 1 after printing FILE:LINE: MESSAGE.
{
    printf '%s %s[32mtest%s[0m a::b ... ok\n' "$TS" "$ESC" "$ESC"
    printf '%s test a::c ... %s[31mFAILED%s[0m\n' "$TS" "$ESC" "$ESC"
    printf '%s failures:\n' "$TS"
    printf '%s     a::c\n' "$TS"
    printf '%s \n' "$TS"
    printf '%s --- lua/tests/first.lua\n' "$TS"
    printf '%s ok  it loads\n' "$TS"
    printf '%s --- lua/tests/second.lua\n' "$TS"
    printf '%s ok  it starts\n' "$TS"
    printf '%s lua: lua/tests/second.lua:42: expected 3, got 4\n' "$TS"
    printf '%s stack traceback:\n' "$TS"
    printf '%s \t[C]: in function error\n' "$TS"
    printf '%s --- lua/tests/third.lua\n' "$TS"
    printf '%s ok  it counts\n' "$TS"
    printf '%s /work/repo/lua/tests/third.lua:7: the count is off\n' "$TS"
    printf '%s --- lua/tests/fourth.lua\n' "$TS"
    printf '%s ...ests/fourth.lua:9: the name is cut\n' "$TS"
}>"$SCRATCH/linux.log"

out=$(parse "$SCRATCH/linux.log")
want="test a::c
lua lua/tests/second.lua: lua/tests/second.lua:42: expected 3, got 4
lua lua/tests/third.lua: /work/repo/lua/tests/third.lua:7: the count is off
lua lua/tests/fourth.lua: ...ests/fourth.lua:9: the name is cut"
[ "$out" = "$want" ] || fail "linux log: got: $out"

# A failed step with no test name prints nothing from the parser.
printf '%s error: buf lint failed\n' "$TS" >"$SCRATCH/step.log"
out=$(parse "$SCRATCH/step.log")
[ -z "$out" ] || fail "step log: got: $out"

# A passing log prints nothing.
printf '%s\n' 'test a::b ... ok' 'test result: ok. 1 passed' >"$SCRATCH/pass.log"
out=$(parse "$SCRATCH/pass.log")
[ -z "$out" ] || fail "passing log: got: $out"

# A stub gh first on PATH. It writes a marker when called, so a usage error
# that reaches gh shows. Exit 99 marks a call that must not happen.
mkdir "$SCRATCH/usage-bin"
cat >"$SCRATCH/usage-bin/gh" <<EOF
#!/bin/sh
: >"$SCRATCH/gh-called"
exit 99
EOF
chmod +x "$SCRATCH/usage-bin/gh"

usage_error() {
    rc=0
    PATH="$SCRATCH/usage-bin:$PATH" sh "$ROOT/tools/ci-failures.sh" "$@" >/dev/null 2>&1 || rc=$?
    [ "$rc" -eq 2 ] || fail "usage error ($*): exit $rc, want 2"
    [ ! -e "$SCRATCH/gh-called" ] || fail "usage error ($*) reached gh"
}
usage_error
usage_error --wait
usage_error 1 2
usage_error --wait 1 2

# A stub gh that answers from files in $STUB_DIR, as the real gh would after
# its --jq filter: tab-separated job rows, a log, a step list.
mkdir "$SCRATCH/bin" "$SCRATCH/stub"
cat >"$SCRATCH/bin/gh" <<'EOF'
#!/bin/sh
D=${STUB_DIR:?}
case "$*" in
    "repo view"*) echo owner/repo ;;
    "run list"*) echo 7 ;;
    "run watch"*) exit "${STUB_WATCH:-0}" ;;
    *actions/runs/*/jobs*)
        n=$(printf '%s\n' "$*" | sed 's|.*actions/runs/\([0-9]*\)/jobs.*|\1|')
        [ -f "$D/jobs-$n" ] || { echo "gh: Not Found (HTTP 404)" >&2; exit 1; }
        cat "$D/jobs-$n"
        ;;
    *actions/jobs/*/logs*)
        n=$(printf '%s\n' "$*" | sed 's|.*actions/jobs/\([0-9]*\)/logs.*|\1|')
        [ -f "$D/log-$n" ] || { echo "gh: log expired (HTTP 410)" >&2; echo "second line" >&2; exit 1; }
        cat "$D/log-$n"
        ;;
    *actions/jobs/*)
        n=$(printf '%s\n' "$*" | sed 's|.*actions/jobs/\([0-9]*\).*|\1|')
        cat "$D/steps-$n"
        ;;
    *) echo "stub gh: unexpected: $*" >&2; exit 99 ;;
esac
EOF
chmod +x "$SCRATCH/bin/gh"

TAB=$(printf '\t')
{
    printf '11%ssuccess%sBuild\n' "$TAB" "$TAB"
    printf '12%sfailure%sLinux\n' "$TAB" "$TAB"
    printf '13%sfailure%sLint\n' "$TAB" "$TAB"
    printf '14%scancelled%sWindows\n' "$TAB" "$TAB"
} >"$SCRATCH/stub/jobs-7"
cp "$SCRATCH/linux.log" "$SCRATCH/stub/log-12"
cp "$SCRATCH/step.log" "$SCRATCH/stub/log-13"
printf '%s\n' 'Run lint' >"$SCRATCH/stub/steps-13"
# No log-14: it expired.
printf '%s\n' 'Test' >"$SCRATCH/stub/steps-14"
printf '12%ssuccess%sLinux\n13%sskipped%sLint\n' "$TAB" "$TAB" "$TAB" "$TAB" >"$SCRATCH/stub/jobs-8"
printf '12%ssuccess%sLinux\n13%srunning%sLint\n' "$TAB" "$TAB" "$TAB" "$TAB" >"$SCRATCH/stub/jobs-9"

run_tool() {
    rc=0
    out=$(STUB_WATCH=${WATCH_STATUS:-0} STUB_DIR="$SCRATCH/stub" PATH="$SCRATCH/bin:$PATH" sh "$ROOT/tools/ci-failures.sh" "$@" 2>&1) || rc=$?
}

expect() {
    printf '%s\n' "$out" | grep -qxF -- "$1" || fail "missing line '$1' in: $out"
}

# A failed run, by ID: each failed job, its tests, its step, its expired log.
run_tool 7
[ "$rc" -eq 1 ] || fail "failed run: exit $rc, want 1"
expect "run 7 on owner/repo"
expect "success: Build"
expect "failure: Linux"
expect "  test a::c"
expect "  lua lua/tests/third.lua: /work/repo/lua/tests/third.lua:7: the count is off"
expect "failure: Lint"
expect "  step: Run lint"
expect "cancelled: Windows"
expect "  log unavailable: gh: log expired (HTTP 410)"
expect "  step: Test"

# By branch: the latest run is 7.
run_tool some-branch
[ "$rc" -eq 1 ] || fail "branch: exit $rc, want 1"
expect "run 7 on owner/repo"

# --wait with a failed watch and a failed run.
WATCH_STATUS=1
run_tool --wait 7
[ "$rc" -eq 1 ] || fail "--wait: exit $rc, want 1"
WATCH_STATUS=0

# Success and skipped: exit 0, also when --wait saw a pass.
run_tool 8
[ "$rc" -eq 0 ] || fail "passing run: exit $rc, want 0: $out"
run_tool --wait 8
[ "$rc" -eq 0 ] || fail "--wait passing run: exit $rc, want 0: $out"

# The watch failed but every job passed: the run did not pass.
WATCH_STATUS=1
run_tool --wait 8
[ "$rc" -eq 1 ] || fail "--wait with a failed watch: exit $rc, want 1"
WATCH_STATUS=0

# A job still running: exit 3.
run_tool 9
[ "$rc" -eq 3 ] || fail "running job: exit $rc, want 3: $out"
expect "running: Lint"

# A gh call that fails: exit 2 with gh's message, not gh's status.
run_tool 99
[ "$rc" -eq 2 ] || fail "gh failure: exit $rc, want 2: $out"
expect "ci-failures: gh api runs/99/jobs: gh: Not Found (HTTP 404)"

printf 'ci-failures-test: ok\n'
