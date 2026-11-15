#!/bin/sh
# Check the archives tools/stage-release.sh wrote: each holds exactly the
# expected files, the programs are a Windows and a Linux executable, both
# carry the same payload, and SHA256SUMS matches.
#
# Usage: sh tools/stage-release-test.sh [out-dir]
# POSIX sh. Needs unzip, tar, and sha256sum or shasum.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT=${1:-$ROOT/dist}
VERSION=$(awk '
    /^\[workspace\.package\]/ { in_sec = 1; next }
    /^\[/ { in_sec = 0 }
    in_sec && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print; exit
    }
' "$ROOT/Cargo.toml")
WIN=nanoprobe-$VERSION-windows-x86_64
LINUX=nanoprobe-$VERSION-linux-x86_64
FAILED=0
fail() {
    printf 'FAIL  %s\n' "$1" >&2
    FAILED=1
}
pass() {
    printf 'ok    %s\n' "$1"
}

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

payload() {
    printf '%s\n' "$1/LICENSE" "$1/payload/lua/hook.lua" "$1/payload/lua/log.lua" \
        "$1/payload/lua/shim.lua" "$1/payload/nanoprobe-hook.lua" "$1/payload/nanoprobe_agent.dll"
}

{ payload "$WIN"; printf '%s\n' "$WIN/install.cmd" "$WIN/nanoprobe.exe" "$WIN/uninstall.cmd"; } |
    LC_ALL=C sort >"$TMP/win.want"
{ payload "$LINUX"; printf '%s\n' "$LINUX/nanoprobe"; } | LC_ALL=C sort >"$TMP/linux.want"

[ -f "$OUT/$WIN.zip" ] || { printf 'no %s. Run: mise run package\n' "$OUT/$WIN.zip" >&2; exit 2; }
[ -f "$OUT/$LINUX.tar.gz" ] || { printf 'no %s. Run: mise run package\n' "$OUT/$LINUX.tar.gz" >&2; exit 2; }

unzip -Z1 "$OUT/$WIN.zip" | grep -v '/$' | LC_ALL=C sort >"$TMP/win.got"
tar -tzf "$OUT/$LINUX.tar.gz" | grep -v '/$' | LC_ALL=C sort >"$TMP/linux.got"
for p in win linux; do
    if cmp -s "$TMP/$p.want" "$TMP/$p.got"; then
        pass "$p archive lists exactly the expected files"
    else
        fail "$p archive's file list differs (- expected, + found):"
        diff "$TMP/$p.want" "$TMP/$p.got" >&2 || true
    fi
done

mkdir "$TMP/win" "$TMP/linux"
(cd "$TMP/win" && unzip -q "$OUT/$WIN.zip")
(cd "$TMP/linux" && tar -xzf "$OUT/$LINUX.tar.gz")

magic() {
    od -An -tx1 -N4 "$1" | tr -d ' \n'
}
case $(magic "$TMP/win/$WIN/nanoprobe.exe") in
    4d5a*) pass "nanoprobe.exe is a Windows executable" ;;
    *) fail "nanoprobe.exe is not a Windows executable" ;;
esac
case $(magic "$TMP/linux/$LINUX/nanoprobe") in
    7f454c46) pass "nanoprobe is a Linux executable" ;;
    *) fail "nanoprobe is not an ELF executable" ;;
esac
[ -x "$TMP/linux/$LINUX/nanoprobe" ] && pass "nanoprobe is executable" ||
    fail "nanoprobe has no execute permission"

if (cd "$TMP/win/$WIN" && find payload LICENSE -type f | LC_ALL=C sort | xargs cat) >"$TMP/w" &&
    (cd "$TMP/linux/$LINUX" && find payload LICENSE -type f | LC_ALL=C sort | xargs cat) >"$TMP/l" &&
    cmp -s "$TMP/w" "$TMP/l"; then
    pass "both archives carry the same payload"
else
    fail "the payloads differ"
fi
for name in hook.lua log.lua shim.lua; do
    cmp -s "$ROOT/lua/agent/$name" "$TMP/linux/$LINUX/payload/lua/$name" ||
        fail "payload/lua/$name is not lua/agent/$name"
done
DLL=$ROOT/target/x86_64-pc-windows-msvc/release/nanoprobe.dll
PAYLOAD_DLL=$TMP/linux/$LINUX/payload/nanoprobe_agent.dll
cmp -s "$DLL" "$PAYLOAD_DLL" && pass "the payload DLL is the checked build" ||
    fail "payload/nanoprobe_agent.dll is not $DLL"
sh "$ROOT/tools/checkdll.sh" "$PAYLOAD_DLL" >/dev/null && pass "the payload DLL passes tools/checkdll.sh" ||
    fail "the payload DLL fails tools/checkdll.sh"

# The PDB beside the archives symbolizes the agent's crash records, so it must
# be the agent's: it names the opener the agent exports.
PDB=$OUT/nanoprobe-$VERSION-agent.pdb
if [ -f "$PDB" ] && LC_ALL=C grep -qa luaopen_nanoprobe "$PDB"; then
    pass "the staged PDB is the agent's"
else
    fail "no agent PDB at $PDB"
fi

cmp -s "$ROOT/lua/hook/nanoprobe-hook.lua" "$TMP/linux/$LINUX/payload/nanoprobe-hook.lua" ||
    fail "payload/nanoprobe-hook.lua is not lua/hook/nanoprobe-hook.lua"

# Install the unpacked Linux payload into a made-up Wine profile with the
# host's build of the same program, check it, and uninstall it again.
PROFILE="$TMP/prefix/drive_c/users/abc/Saved Games/DCS.dcs_serverrelease"
mkdir -p "$PROFILE/Config"
printf 'options.example = 1\n' >"$PROFILE/Config/autoexec.cfg"
# Each path whole, with its mode and size: paths here hold spaces.
snapshot() {
    (cd "$TMP/prefix" && find . -print | LC_ALL=C sort | while IFS= read -r p; do
        printf '%s %s %s\n' "$(ls -ld "$p" | awk '{print $1, $5}')" "$(cksum <"$p" 2>/dev/null || true)" "$p"
    done)
}
snapshot >"$TMP/before"
NANOPROBE="cargo run --quiet --release -p nanoprobe-cli --"
if (cd "$ROOT" && $NANOPROBE install --wine-prefix "$TMP/prefix" --payload "$TMP/linux/$LINUX/payload") >"$TMP/log" 2>&1 &&
    (cd "$ROOT" && $NANOPROBE doctor --wine-prefix "$TMP/prefix") >>"$TMP/log" 2>&1 &&
    (cd "$ROOT" && $NANOPROBE uninstall --wine-prefix "$TMP/prefix") >>"$TMP/log" 2>&1; then
    pass "the payload installs, passes doctor and uninstalls"
else
    fail "install, doctor or uninstall of the payload failed:"
    cat "$TMP/log" >&2
fi
snapshot >"$TMP/after"
if cmp -s "$TMP/before" "$TMP/after" && [ "$(cat "$PROFILE/Config/autoexec.cfg")" = "options.example = 1" ]; then
    pass "uninstall left the profile as it was"
else
    fail "uninstall left the profile changed"
    diff "$TMP/before" "$TMP/after" >&2 || true
fi

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$OUT" && sha256sum -c --quiet SHA256SUMS) && pass "SHA256SUMS matches" || fail "SHA256SUMS does not match"
else
    (cd "$OUT" && shasum -a 256 -c --quiet SHA256SUMS) && pass "SHA256SUMS matches" || fail "SHA256SUMS does not match"
fi

exit "$FAILED"
