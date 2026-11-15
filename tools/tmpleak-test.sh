#!/bin/sh
# Check that tools/tmpleak.sh fails on a planted file and on a planted process,
# and passes when the run leaves nothing. A stand-in `cargo` replaces the real
# one, so no test runs.
#
# POSIX sh.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

# `cargo test ... --no-run` does nothing. The real run plants what $PLANT names.
cat >"$SCRATCH/cargo" <<'EOF'
#!/bin/sh
case " $* " in *" --no-run "*) exit 0 ;; esac
case ${PLANT:-} in
    file) mkdir "$TMPDIR/nanoprobe-planted" ;;
    process) sh -c "sleep 30; :" "$TMPDIR" >/dev/null 2>&1 & ;;
esac
exit 0
EOF
chmod +x "$SCRATCH/cargo"

run() {
    PLANT=$1 PATH="$SCRATCH:$PATH" sh "$ROOT/tools/tmpleak.sh" 2>&1
}

fail() {
    printf 'tmpleak-test: %s\n' "$1" >&2
    exit 1
}

out=$(run none) || fail "a clean run failed: $out"

if out=$(run file); then fail "a planted file passed"; fi
case $out in *nanoprobe-planted*) ;; *) fail "the planted file is not named: $out" ;; esac

if out=$(run process); then fail "a planted process passed"; fi
case $out in *"left processes"*) ;; *) fail "the planted process is not named: $out" ;; esac

# The sweep killed it.
if pgrep -f "sh -c sleep 30; : .*nanoprobe-tmpleak" >/dev/null 2>&1; then
    pkill -KILL -f "sh -c sleep 30; : .*nanoprobe-tmpleak" || true
    fail "a planted process survived the script"
fi

printf 'tmpleak-test: ok\n'
