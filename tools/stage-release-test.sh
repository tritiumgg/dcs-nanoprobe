#!/bin/sh
# Check the archives tools/stage-release.sh wrote: each holds exactly the
# files spec/install.md lists, the programs are Windows executables and static
# Linux ones, both carry the same payload, the texts are the repository's, the
# Linux scripts pass on their arguments and the program's exit code, the
# schema is staged (tools/stage-schema-test.sh), no PDB is in the release
# while the released DLL's is kept in dist/private/, and SHA256SUMS matches.
#
# Usage: sh tools/stage-release-test.sh [out-dir]
# POSIX sh. Needs unzip, tar, and sha256sum or shasum.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUT=$(CDPATH= cd -- "${1:-$ROOT/dist}" && pwd)
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

{
    payload "$WIN"
    printf '%s\n' "$WIN/README.txt" "$WIN/install.cmd" "$WIN/nanoprobe-sidecar.exe" \
        "$WIN/nanoprobe.exe" "$WIN/sidecar.toml" "$WIN/uninstall.cmd"
} | LC_ALL=C sort >"$TMP/win.want"
{
    payload "$LINUX"
    printf '%s\n' "$LINUX/README.txt" "$LINUX/install.sh" "$LINUX/nanoprobe" \
        "$LINUX/nanoprobe-sidecar" "$LINUX/uninstall.sh"
    (cd "$ROOT" && git ls-files -- deploy) | sed "s|^|$LINUX/|"
} | LC_ALL=C sort >"$TMP/linux.want"

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
for name in nanoprobe nanoprobe-sidecar; do
    case $(magic "$TMP/win/$WIN/$name.exe") in
        4d5a*) pass "$name.exe is a Windows executable" ;;
        *) fail "$name.exe is not a Windows executable" ;;
    esac
    case $(magic "$TMP/linux/$LINUX/$name") in
        7f454c46) pass "$name is a Linux executable" ;;
        *) fail "$name is not an ELF executable" ;;
    esac
    # A static program names no loader. A dynamic one holds the loader's path,
    # ld-linux or ld-musl, in its interpreter header.
    if LC_ALL=C grep -qa -e '/ld-linux' -e '/ld-musl' "$TMP/linux/$LINUX/$name"; then
        fail "$name names a dynamic loader: it is not static"
    else
        pass "$name is static"
    fi
done
for name in nanoprobe nanoprobe-sidecar install.sh uninstall.sh; do
    [ -x "$TMP/linux/$LINUX/$name" ] && pass "$name is executable" ||
        fail "$name has no execute permission"
done

# The texts are the repository's: the Windows ones with Windows line ends.
uncr() {
    tr -d '\r' <"$1"
}
uncr "$TMP/win/$WIN/README.txt" | cmp -s - "$ROOT/tools/release/README.windows.txt" &&
    uncr "$TMP/win/$WIN/sidecar.toml" | cmp -s - "$ROOT/tools/release/sidecar.windows.toml" &&
    [ "$(LC_ALL=C grep -c "$(printf '\r')\$" "$TMP/win/$WIN/README.txt")" = \
        "$(wc -l <"$ROOT/tools/release/README.windows.txt" | tr -d ' ')" ] &&
    pass "the Windows README.txt and sidecar.toml are the repository's, with Windows line ends" ||
    fail "the Windows README.txt or sidecar.toml differs from tools/release/"
cmp -s "$TMP/linux/$LINUX/README.txt" "$ROOT/tools/release/README.linux.txt" &&
    pass "the Linux README.txt is the repository's" || fail "the Linux README.txt differs"
(cd "$ROOT" && git ls-files -- deploy) | while IFS= read -r f; do
    cmp -s "$ROOT/$f" "$TMP/linux/$LINUX/$f" || { printf 'FAIL  %s differs from the repository\n' "$f" >&2; exit 1; }
done && pass "deploy/ is the repository's" || FAILED=1

# The Linux scripts run the program beside them. A stand-in program shows
# that the arguments, spaces included, and the exit code pass through.
mkdir "$TMP/scripts"
cp "$TMP/linux/$LINUX/install.sh" "$TMP/linux/$LINUX/uninstall.sh" "$TMP/scripts/"
cat >"$TMP/scripts/nanoprobe" <<'STUB'
#!/bin/sh
for arg do printf '[%s]' "$arg"; done
exit 7
STUB
chmod 0755 "$TMP/scripts/nanoprobe"
for verb in install uninstall; do
    code=0
    got=$(cd / && "$TMP/scripts/$verb.sh" --wine-prefix "a b" --profile=c) || code=$?
    if [ "$got" = "[$verb][--wine-prefix][a b][--profile=c]" ] && [ "$code" = 7 ]; then
        pass "$verb.sh passes on its arguments and the program's exit code"
    else
        fail "$verb.sh ran \`$got\` and exited $code"
    fi
done

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

# The PDB the builder keeps symbolizes the released DLL's crash records, so
# it is kept beside that DLL under the signature its crash records carry, it
# names the opener the agent exports, and it holds the DLL's PDB GUID. It is
# not released.
SIG=$(sh "$ROOT/tools/pdbsig.sh" "$PAYLOAD_DLL") || fail "the payload DLL has no CodeView record"
KEPT=$OUT/private/$SIG
GUID=$(sh "$ROOT/tools/pdbsig.sh" --guid "$PAYLOAD_DLL" || true)
if cmp -s "$PAYLOAD_DLL" "$KEPT/nanoprobe.dll" &&
    LC_ALL=C grep -qa luaopen_nanoprobe "$KEPT/nanoprobe.pdb" &&
    [ -n "$GUID" ] && od -An -tx1 -v "$KEPT/nanoprobe.pdb" | tr -d ' \n' | grep -q "$GUID"; then
    pass "the kept PDB is the released DLL's, under its signature $SIG"
else
    fail "no PDB of the released DLL at $KEPT"
fi
# No PDB in the release: none beside the archives and none in an archive.
PDBS=$( (find "$OUT" -maxdepth 1 -type f -iname '*.pdb'; find "$TMP/win" "$TMP/linux" -type f -iname '*.pdb') | LC_ALL=C sort)
if [ -z "$PDBS" ]; then
    pass "no PDB is in the release"
else
    fail "a PDB is in the release: $PDBS"
fi

# A release is public: no staged file may name the machine that built it. The
# builder's home folder holds the checkout's, cargo's and the toolchain's
# paths, which rustc and the linker would write into the programs, the DLL
# and the PDB (tools/remap.sh).
LEAKS=0
for f in $(find "$TMP/win" "$TMP/linux" -type f | LC_ALL=C sort); do
    for path in "$ROOT" "$HOME"; do
        n=$(LC_ALL=C grep -acF -- "$path" "$f" || true)
        if [ "$n" != 0 ]; then
            fail "${f#"$TMP"/} names this machine's path $path on $n lines"
            LEAKS=1
        fi
    done
done
[ "$LEAKS" = 0 ] && pass "no file in an archive names this machine's home folder or checkout"
# The kept PDB may name them: the linker writes the absolute path of each
# object file and library it read, and of its own command line, and rustc's
# remapping does not reach those. That is why it is not released.

cmp -s "$ROOT/lua/hook/nanoprobe-hook.lua" "$TMP/linux/$LINUX/payload/nanoprobe-hook.lua" ||
    fail "payload/nanoprobe-hook.lua is not lua/hook/nanoprobe-hook.lua"

# Install the unpacked Linux payload into a made-up Wine profile with the
# host's build of the same program, check it, and uninstall it again.
PROFILE="$TMP/prefix/drive_c/users/abc/Saved Games/DCS.dcs_serverrelease"
mkdir -p "$PROFILE/Config"
printf 'options.example = 1\n' >"$PROFILE/Config/autoexec.cfg"
# A sidecar's folder beside the profile: install and uninstall leave it alone.
mkdir -p "$TMP/prefix/drive_c/users/abc/Saved Games/sidecar/campaigns"
printf 'store\n' >"$TMP/prefix/drive_c/users/abc/Saved Games/sidecar/campaigns/store.sqlite"
cp "$ROOT/deploy/sidecar.toml" "$TMP/prefix/drive_c/users/abc/Saved Games/sidecar/sidecar.toml"
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
    pass "uninstall left the profile, and a sidecar's folder beside it, as they were"
else
    fail "uninstall left the profile or the sidecar's folder changed"
    diff "$TMP/before" "$TMP/after" >&2 || true
fi

sh "$ROOT/tools/stage-schema-test.sh" "$OUT" "$VERSION" || FAILED=1
printf '%s\n' "$WIN.zip" "$LINUX.tar.gz" \
    "nanoprobe-$VERSION-schema.tar.gz" "nanoprobe-$VERSION-schema.binpb" | LC_ALL=C sort >"$TMP/sums.want"
awk '{ print $2 }' "$OUT/SHA256SUMS" | LC_ALL=C sort >"$TMP/sums.got"
if cmp -s "$TMP/sums.want" "$TMP/sums.got"; then
    pass "SHA256SUMS lists the two archives and the schema's two files"
else
    fail "SHA256SUMS lists other files (- expected, + found):"
    diff "$TMP/sums.want" "$TMP/sums.got" >&2 || true
fi

if command -v sha256sum >/dev/null 2>&1; then
    (cd "$OUT" && sha256sum -c --quiet SHA256SUMS) && pass "SHA256SUMS matches" || fail "SHA256SUMS does not match"
else
    (cd "$OUT" && shasum -a 256 -c --quiet SHA256SUMS) && pass "SHA256SUMS matches" || fail "SHA256SUMS does not match"
fi

exit "$FAILED"
