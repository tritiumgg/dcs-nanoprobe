#!/bin/sh
# Check which base tools/protobreak.sh picks when origin/main has no schema.
#
# Runs in scratch repositories. Needs git and the buf that mise.toml pins.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
CHECK="$ROOT/tools/protobreak.sh"

BUF=${BUF:-$(command -v buf || true)}
if command -v mise >/dev/null 2>&1; then
    BUF=$(cd "$ROOT" && mise which buf 2>/dev/null || printf '%s' "$BUF")
fi
[ -n "$BUF" ] || {
    printf 'no buf on PATH. Run: mise exec -- sh tools/protobreak-test.sh\n' >&2
    exit 2
}
PATH="$(dirname "$BUF"):$PATH"
export PATH

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM

repo() {
    git init -q "$1"
    git -C "$1" config user.name test
    git -C "$1" config user.email test@example.invalid
    git -C "$1" config commit.gpgsign false
    printf 'scratch\n' >"$1/README"
    git -C "$1" add README
    git -C "$1" commit -q -m "no schema"
    # The published main branch, as it is before the schema is pushed.
    git -C "$1" update-ref refs/remotes/origin/main HEAD
}

fail() {
    printf 'FAIL %s\n' "$1" >&2
    exit 1
}

# 1. origin/main has no schema, main has one: the check compares against main.
A="$SCRATCH/a"
repo "$A"
git -C "$A" branch -q -M main
mkdir -p "$A/proto/t/v1"
printf 'version: v2\nmodules:\n  - path: proto\n' >"$A/buf.yaml"
printf 'syntax = "proto3";\npackage t.v1;\nmessage M { string a = 1; }\n' >"$A/proto/t/v1/m.proto"
git -C "$A" add buf.yaml proto
git -C "$A" commit -q -m "schema"
out=$(cd "$A" && sh "$CHECK")
printf '%s\n' "$out" | grep -q 'against main' || fail "case 1 did not compare against main: $out"
# A breaking change against main is caught.
printf 'syntax = "proto3";\npackage t.v1;\nmessage M { int32 a = 1; }\n' >"$A/proto/t/v1/m.proto"
if (cd "$A" && sh "$CHECK" >/dev/null 2>&1); then
    fail "case 1 passed a breaking change"
fi
echo "ok  a schema on main is the base when origin/main has none"

# 2. Neither origin/main, main nor HEAD has a schema: nothing to compare.
B="$SCRATCH/b"
repo "$B"
git -C "$B" branch -q -M main
out=$(cd "$B" && sh "$CHECK")
printf '%s\n' "$out" | grep -q 'nothing to compare' || fail "case 2: $out"
echo "ok  no schema anywhere passes with nothing to compare"

schema() {
    mkdir -p "$1/proto/t/v1"
    printf 'version: v2\nmodules:\n  - path: proto\n' >"$1/buf.yaml"
    printf 'syntax = "proto3";\npackage t.v1;\nmessage M { string a = 1; %s }\n' "$2" >"$1/proto/t/v1/m.proto"
    git -C "$1" add buf.yaml proto
    git -C "$1" commit -q -m "schema $2"
}

# 3. origin/main and main both hold a schema: origin/main is the base.
C="$SCRATCH/c"
repo "$C"
git -C "$C" branch -q -M main
schema "$C" ""
git -C "$C" update-ref refs/remotes/origin/main HEAD
schema "$C" "string b = 2;"
out=$(cd "$C" && sh "$CHECK")
printf '%s\n' "$out" | grep -q 'against origin/main' || fail "case 3: $out"
echo "ok  origin/main comes before main"

# 4. Only HEAD holds a schema: a new branch in a repository whose main has
# none.
D="$SCRATCH/d"
repo "$D"
git -C "$D" branch -q -M main
git -C "$D" checkout -q -b work
schema "$D" ""
out=$(cd "$D" && sh "$CHECK")
printf '%s\n' "$out" | grep -q 'against HEAD' || fail "case 4: $out"
echo "ok  HEAD is the base when only it has a schema"

# 5. A named ref that does not resolve fails; the all-zero commit of a new
# branch's push takes the default chain.
if (cd "$C" && sh "$CHECK" no-such-ref >/dev/null 2>&1); then
    fail "case 5 passed a ref that does not exist"
fi
out=$(cd "$C" && sh "$CHECK" 0000000000000000000000000000000000000000)
printf '%s\n' "$out" | grep -q 'against origin/main' || fail "case 5 zero commit: $out"
echo "ok  a missing ref fails and the zero commit takes the default"
