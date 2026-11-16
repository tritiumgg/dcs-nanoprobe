#!/bin/sh
# Check that the pre-commit hook blocks a secret and passes a clean change.
#
# The test runs in a scratch repository with a token generated at run time, so
# no key-shaped string is ever committed here, and the scanner never has to be
# told to ignore one.
#
# POSIX sh. Needs git and the gitleaks that mise.toml pins.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
HOOK="$ROOT/.githooks/pre-commit"

GITLEAKS=${GITLEAKS:-$(command -v gitleaks || true)}
[ -n "$GITLEAKS" ] || {
    printf 'no gitleaks on PATH. Run: mise exec -- sh tools/secretscan-test.sh\n' >&2
    exit 2
}
# The mise shim works only inside this repository, so resolve the real binary.
if command -v mise >/dev/null 2>&1; then
    GITLEAKS=$(cd "$ROOT" && mise which gitleaks 2>/dev/null || printf '%s' "$GITLEAKS")
fi
export GITLEAKS

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

git init -q "$SCRATCH"
cd "$SCRATCH"
git config user.name test
git config user.email test@invalid

# A clean change must pass, or the hook blocks everything and proves nothing.
printf 'hello\n' > clean.txt
git add clean.txt
sh "$HOOK" >/dev/null 2>&1 || {
    printf 'FAIL  the hook blocked a change with no secret\n' >&2
    exit 1
}
printf 'ok  a clean change passes\n'

# A GitHub token shape: ghp_ and 36 random letters and digits. The input is bounded:
# 512 random bytes give about 124 letters and digits, and tr ends at its input's end.
# An unbounded read of /dev/urandom spins forever where SIGPIPE is ignored.
TOKEN="ghp_$(head -c 512 /dev/urandom | LC_ALL=C tr -dc 'A-Za-z0-9' | head -c 36)"
printf 'token = "%s"\n' "$TOKEN" > leak.txt
git add leak.txt
if OUTPUT=$(sh "$HOOK" 2>&1); then
    printf 'FAIL  the hook passed a planted token\n' >&2
    exit 1
fi
# A hook that fails for another reason also blocks, and proves nothing.
case "$OUTPUT" in
    *"leaks found: 1"*) ;;
    *)
        printf 'FAIL  the hook blocked the commit without finding the token:\n%s\n' "$OUTPUT" >&2
        exit 1
        ;;
esac
printf 'ok  a planted token is blocked\n'
