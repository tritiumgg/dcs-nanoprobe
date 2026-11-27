#!/bin/sh
# Check the archives tools/stage-release.sh wrote: each holds exactly the
# files spec/install.md lists, the programs are Windows executables, static
# Linux ones and a universal macOS one, the Windows and Linux archives carry
# the same payload, all three the same documents with every relative link
# resolving inside the archive, the texts are the repository's, the Linux
# scripts pass on their arguments and the program's exit code, the fake
# sidecar runs (on a Linux x86_64 host, and on a macOS host for the macOS
# build), the schema is staged (tools/stage-schema-test.sh), no PDB is in the
# release while the released DLL's is kept in dist/private/, and SHA256SUMS
# matches.
#
# Usage: sh tools/stage-release-test.sh [out-dir]
# POSIX sh. Needs unzip, tar, llvm-lipo, llvm-objdump, and sha256sum or
# shasum.
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
MAC=nanoprobe-$VERSION-macos-universal
DOCS_LIST=$ROOT/tools/release/docs.txt
REPO_URL=https://github.com/tritiumgg/dcs-nanoprobe
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
docs_list() {
    grep -v -e '^#' -e '^$' "$DOCS_LIST"
}

{
    docs_list | sed "s|^|$WIN/|"
    payload "$WIN"
    printf '%s\n' "$WIN/README.txt" "$WIN/install.cmd" "$WIN/nanoprobe-sidecar.exe" \
        "$WIN/nanoprobe-fake-sidecar.exe" "$WIN/nanoprobe.exe" "$WIN/sidecar.toml" "$WIN/uninstall.cmd"
} | LC_ALL=C sort >"$TMP/win.want"
{
    docs_list | sed "s|^|$LINUX/|"
    payload "$LINUX"
    printf '%s\n' "$LINUX/README.txt" "$LINUX/install.sh" "$LINUX/nanoprobe" \
        "$LINUX/nanoprobe-sidecar" "$LINUX/nanoprobe-fake-sidecar" "$LINUX/uninstall.sh"
    (cd "$ROOT" && git ls-files -- deploy) | sed "s|^|$LINUX/|"
} | LC_ALL=C sort >"$TMP/linux.want"
{
    docs_list | sed "s|^|$MAC/|"
    printf '%s\n' "$MAC/LICENSE" "$MAC/README.txt" "$MAC/nanoprobe-fake-sidecar"
} | LC_ALL=C sort >"$TMP/mac.want"

for f in "$WIN.zip" "$LINUX.tar.gz" "$MAC.tar.gz"; do
    [ -f "$OUT/$f" ] || { printf 'no %s. Run: mise run package\n' "$OUT/$f" >&2; exit 2; }
done

unzip -Z1 "$OUT/$WIN.zip" | grep -v '/$' | LC_ALL=C sort >"$TMP/win.got"
tar -tzf "$OUT/$LINUX.tar.gz" | grep -v '/$' | LC_ALL=C sort >"$TMP/linux.got"
tar -tzf "$OUT/$MAC.tar.gz" | grep -v '/$' | LC_ALL=C sort >"$TMP/mac.got"
for p in win linux mac; do
    if cmp -s "$TMP/$p.want" "$TMP/$p.got"; then
        pass "$p archive lists exactly the expected files"
    else
        fail "$p archive's file list differs (- expected, + found):"
        diff "$TMP/$p.want" "$TMP/$p.got" >&2 || true
    fi
done

mkdir "$TMP/win" "$TMP/linux" "$TMP/mac"
(cd "$TMP/win" && unzip -q "$OUT/$WIN.zip")
(cd "$TMP/linux" && tar -xzf "$OUT/$LINUX.tar.gz")
(cd "$TMP/mac" && tar -xzf "$OUT/$MAC.tar.gz")

magic() {
    od -An -tx1 -N4 "$1" | tr -d ' \n'
}
for name in nanoprobe nanoprobe-sidecar nanoprobe-fake-sidecar; do
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
for name in nanoprobe nanoprobe-sidecar nanoprobe-fake-sidecar install.sh uninstall.sh; do
    [ -x "$TMP/linux/$LINUX/$name" ] && pass "$name is executable" ||
        fail "$name has no execute permission"
done

# The released fake sidecar runs: its help is the user's, and it serves and
# runs a program. Only a Linux x86_64 host, such as the release runner, can
# run the Linux build.
if [ "$(uname -s)-$(uname -m)" = Linux-x86_64 ]; then
    fake=$TMP/linux/$LINUX/nanoprobe-fake-sidecar
    "$fake" --help 2>&1 | grep -q '^Usage: nanoprobe-fake-sidecar ' &&
        pass "nanoprobe-fake-sidecar --help prints its usage" ||
        fail "nanoprobe-fake-sidecar --help does not print its usage"
    if timeout 60 "$fake" --listen 127.0.0.1:0 --run true >"$TMP/fake.log" 2>&1; then
        pass "nanoprobe-fake-sidecar serves and runs a program"
    else
        fail "nanoprobe-fake-sidecar --run true failed:"
        cat "$TMP/fake.log" >&2
    fi
else
    printf 'skip  running nanoprobe-fake-sidecar: needs a Linux x86_64 host\n'
fi

# The macOS archive holds the fake sidecar alone: a macOS machine runs no DCS.
for name in nanoprobe nanoprobe-sidecar payload deploy install.sh uninstall.sh; do
    if [ -e "$TMP/mac/$MAC/$name" ]; then
        fail "the macOS archive holds $name"
    fi
done
[ -e "$TMP/mac/$MAC/nanoprobe" ] || [ -e "$TMP/mac/$MAC/nanoprobe-sidecar" ] ||
    [ -e "$TMP/mac/$MAC/payload" ] || pass "the macOS archive holds no nanoprobe, sidecar or payload"

# The macOS fake sidecar is one universal program of an arm64 and an x86_64
# build, read with LLVM's tools so a Linux host can check it. Each build is an
# executable for macOS 11 that loads only libraries every macOS has. The arm64
# build carries the signature the kernel needs to run it (spec/install.md,
# "Release").
MACFAKE=$TMP/mac/$MAC/nanoprobe-fake-sidecar
if [ "$(magic "$MACFAKE")" = cafebabe ]; then
    pass "nanoprobe-fake-sidecar (macOS) is a universal Mach-O file"
else
    fail "nanoprobe-fake-sidecar (macOS) is not a universal Mach-O file"
fi
archs=$(llvm-lipo -archs "$MACFAKE" 2>&1 | tr ' ' '\n' | grep -v '^$' | LC_ALL=C sort | tr '\n' ' ')
[ "$archs" = "arm64 x86_64 " ] && pass "nanoprobe-fake-sidecar (macOS) holds arm64 and x86_64" ||
    fail "nanoprobe-fake-sidecar (macOS) holds the architectures: $archs"
for arch in arm64 x86_64; do
    llvm-objdump --macho --arch "$arch" --private-headers "$MACFAKE" >"$TMP/mac.$arch.h" 2>&1 || true
    if grep -q ' EXECUTE ' "$TMP/mac.$arch.h" &&
        awk '/cmd LC_BUILD_VERSION/ { b = 1 } b && $1 == "platform" { p = $2 } b && $1 == "minos" { m = $2; exit }
            END { exit !(p == "macos" && m == "11.0") }' "$TMP/mac.$arch.h"; then
        pass "the $arch build is a macOS executable for macOS 11.0 and later"
    else
        fail "the $arch build is not an executable with platform macos and minos 11.0"
    fi
    llvm-objdump --macho --arch "$arch" --dylibs-used "$MACFAKE" 2>&1 | tail -n +2 |
        awk '{ print $1 }' | LC_ALL=C sort >"$TMP/mac.$arch.libs"
    if grep -qx /usr/lib/libSystem.B.dylib "$TMP/mac.$arch.libs" &&
        ! grep -vx -e /usr/lib/libSystem.B.dylib -e /usr/lib/libiconv.2.dylib "$TMP/mac.$arch.libs" >/dev/null; then
        pass "the $arch build loads only libSystem and libiconv"
    else
        fail "the $arch build loads: $(tr '\n' ' ' <"$TMP/mac.$arch.libs")"
    fi
done
grep -q 'cmd LC_CODE_SIGNATURE' "$TMP/mac.arm64.h" && pass "the arm64 build carries a code signature" ||
    fail "the arm64 build has no code signature: macOS will not run it"
# tar keeps the mode it was given; 0755 is what the stage script set.
[ "$(ls -l "$MACFAKE" | cut -c1-10)" = -rwxr-xr-x ] && pass "nanoprobe-fake-sidecar (macOS) has mode 0755" ||
    fail "nanoprobe-fake-sidecar (macOS) has mode $(ls -l "$MACFAKE" | cut -c1-10)"

# Only a macOS host can verify the signature and run the program. The release
# runner is Linux; the maintainer runs these on the runner's build before a
# tag (docs/releasing.md).
if [ "$(uname -s)" = Darwin ]; then
    codesign -v --strict --arch arm64 "$MACFAKE" 2>"$TMP/codesign.log" &&
        pass "the arm64 build passes codesign -v --strict" ||
        fail "the arm64 build fails codesign -v --strict: $(cat "$TMP/codesign.log")"
    for arch in arm64 x86_64; do
        if ! arch -"$arch" /usr/bin/true 2>/dev/null; then
            printf 'skip  running the %s build: this Mac cannot run it\n' "$arch"
            continue
        fi
        arch -"$arch" "$MACFAKE" --help 2>&1 | grep -q '^Usage: nanoprobe-fake-sidecar ' &&
            pass "the $arch build's --help prints its usage" ||
            fail "the $arch build's --help does not print its usage"
        # macOS has no timeout(1); perl's alarm bounds the run.
        if perl -e 'alarm 60; exec @ARGV' arch -"$arch" "$MACFAKE" --listen 127.0.0.1:0 --run true >"$TMP/fake.log" 2>&1; then
            pass "the $arch build serves and runs a program"
        else
            fail "the $arch build's --run true failed:"
            cat "$TMP/fake.log" >&2
        fi
    done
else
    printf 'skip  verifying and running the macOS fake sidecar: needs a macOS host\n'
fi

# Every archive carries the same documents, each the repository's file with
# its links rewritten by tools/release-links.awk.
docs_list >"$TMP/keep"
printf 'LICENSE\n' >>"$TMP/keep"
docs_cat() {
    docs_list | while IFS= read -r f; do cat "$1/$f"; done
}
docs_cat "$TMP/win/$WIN" >"$TMP/docs.win"
docs_cat "$TMP/linux/$LINUX" >"$TMP/docs.linux"
docs_cat "$TMP/mac/$MAC" >"$TMP/docs.mac"
cmp -s "$TMP/docs.win" "$TMP/docs.linux" && cmp -s "$TMP/docs.win" "$TMP/docs.mac" &&
    pass "the three archives carry the same documents" || fail "the archives' documents differ"
DOCS_OK=1
for f in $(docs_list); do
    case $f in
        */*) fdir=${f%/*} ;;
        *) fdir= ;;
    esac
    awk -f "$ROOT/tools/release-links.awk" -v base="$REPO_URL/blob/v$VERSION" -v dir="$fdir" \
        -v keep="$TMP/keep" -v repo="$REPO_URL" -v ref="v$VERSION" "$ROOT/$f" |
        cmp -s - "$TMP/mac/$MAC/$f" || { fail "$f is not the repository's with its links rewritten"; DOCS_OK=0; }
done
[ "$DOCS_OK" = 1 ] && pass "each document is the repository's, its links rewritten for v$VERSION"

# Apart from the awk: each relative link of a document resolves to a file or
# folder inside the archive's folder, and none names the repository's main
# branch.
inside() {
    # inside <archive folder> <document folder> <link path>: the path exists
    # and, resolved, lies within the archive folder.
    top=$(cd "$1" && pwd -P)
    t=$2/$3
    if [ -d "$t" ]; then
        real=$(cd "$t" && pwd -P)
    elif [ -e "$t" ]; then
        real=$(cd "$(dirname "$t")" && pwd -P)
    else
        return 1
    fi
    case $real/ in
        "$top"/*) return 0 ;;
        *) return 1 ;;
    esac
}
tracked() {
    # tracked <repository path>: a tracked file, or a folder holding one.
    [ -n "$(cd "$ROOT" && git ls-files -- "$1" | head -n 1)" ]
}
held() {
    # held <repository path>: a listed document or LICENSE, or a folder that
    # holds one.
    awk -v p="$1" '$0 == p || index($0, p "/") == 1 { found = 1 } END { exit !found }' "$TMP/keep"
}
links_outside_code() {
    awk '/^[ \t]*(```|~~~)/ { code = !code; next }
        !code { while (match($0, /\]\([^)]*\)/)) { print substr($0, RSTART + 2, RLENGTH - 3); $0 = substr($0, RSTART + RLENGTH) } }' "$1"
}
LINKS_OK=1
for f in $(docs_list); do
    doc=$TMP/mac/$MAC/$f
    links_outside_code "$doc" | sed 's/#.*$//' |
        grep -v -e '^$' -e '^[a-z][a-z0-9+.-]*:' -e '^/' >"$TMP/links" || true
    while IFS= read -r link; do
        inside "$TMP/mac/$MAC" "$(dirname "$doc")" "$link" || {
            printf 'FAIL  %s links %s, which is not in the archive\n' "$f" "$link" >&2
            LINKS_OK=0
        }
    done <"$TMP/links"
    if LC_ALL=C grep -n -F -e "$REPO_URL/blob/main/" -e "$REPO_URL/tree/main/" "$doc" >&2; then
        printf 'FAIL  %s links the main branch\n' "$f" >&2
        LINKS_OK=0
    fi
done
[ "$LINKS_OK" = 1 ] && pass "every relative link of the documents resolves inside the archive, and none names main" ||
    FAILED=1

# The rewrite turns a broken link into a link to the tag, which the check
# above does not open. So, again apart from the awk: each relative link of a
# listed document, outside fenced code, names a file git tracks or a folder
# that holds one, and stays as written in the archive's copy when the archive
# holds that path; and each link to the tag in a copy names a tracked path.
SRC_OK=1
for f in $(docs_list); do
    fdir=$(dirname "$ROOT/$f")
    links_outside_code "$ROOT/$f" | sed 's/#.*$//' |
        grep -v -e '^$' -e '^[a-z][a-z0-9+.-]*:' -e '^/' >"$TMP/srclinks" || true
    while IFS= read -r link; do
        if [ -d "$fdir/$link" ]; then
            real=$(cd "$fdir/$link" && pwd -P)
        elif [ -e "$fdir/$link" ]; then
            real=$(cd "$(dirname "$fdir/$link")" && pwd -P)/$(basename "$link")
        else
            real=
        fi
        rootreal=$(cd "$ROOT" && pwd -P)
        rel=${real#"$rootreal"/}
        if [ -z "$real" ] || [ "$rel" = "$real" ] || ! tracked "$rel"; then
            printf 'FAIL  %s links %s, which git does not track\n' "$f" "$link" >&2
            SRC_OK=0
        elif held "${rel%/}" && ! LC_ALL=C grep -qF -- "]($link" "$TMP/mac/$MAC/$f"; then
            printf 'FAIL  %s links %s, which the archive holds, but its copy does not\n' "$f" "$link" >&2
            SRC_OK=0
        fi
    done <"$TMP/srclinks"
    LC_ALL=C grep -o -e "$REPO_URL/blob/v$VERSION/[^)#> |\`]*" -e "$REPO_URL/tree/v$VERSION/[^)#> |\`]*" \
        "$TMP/mac/$MAC/$f" | sed "s|^$REPO_URL/[a-z]*/v$VERSION/||; s|/\$||" >"$TMP/taglinks" || true
    while IFS= read -r p; do
        tracked "$p" || {
            printf 'FAIL  %s links the tag at %s, which git does not track\n' "$f" "$p" >&2
            SRC_OK=0
        }
    done <"$TMP/taglinks"
done
[ "$SRC_OK" = 1 ] && pass "every link of the documents names a tracked file, and stays relative where the archive holds it" ||
    FAILED=1

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
cmp -s "$TMP/mac/$MAC/README.txt" "$ROOT/tools/release/README.macos.txt" &&
    cmp -s "$TMP/mac/$MAC/LICENSE" "$ROOT/LICENSE" &&
    pass "the macOS README.txt and LICENSE are the repository's" ||
    fail "the macOS README.txt or LICENSE differs"
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
PDBS=$( (find "$OUT" -maxdepth 1 -type f -iname '*.pdb'; find "$TMP/win" "$TMP/linux" "$TMP/mac" -type f -iname '*.pdb') | LC_ALL=C sort)
if [ -z "$PDBS" ]; then
    pass "no PDB is in the release"
else
    fail "a PDB is in the release: $PDBS"
fi

# A release is public: no staged file may name the machine that built it. The
# builder's home folder holds the checkout's, cargo's and the toolchain's
# paths, which rustc and the linker would write into the programs, the DLL
# and the PDB (tools/remap.sh). The home folders of macOS and Linux, and
# rustup's folder, are checked as well, so a path of another machine, such as
# the one that built a slice, is caught too.
LEAKS=0
for f in $(find "$TMP/win" "$TMP/linux" "$TMP/mac" -type f | LC_ALL=C sort); do
    for path in "$ROOT" "$HOME" /Users/ /home/ .rustup; do
        n=$(LC_ALL=C grep -acF -- "$path" "$f" || true)
        if [ "$n" != 0 ]; then
            fail "${f#"$TMP"/} names the build path $path on $n lines"
            LEAKS=1
        fi
    done
done
[ "$LEAKS" = 0 ] && pass "no file in an archive names a home folder, rustup's folder or the checkout"
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
printf '%s\n' "$WIN.zip" "$LINUX.tar.gz" "$MAC.tar.gz" \
    "nanoprobe-$VERSION-schema.tar.gz" "nanoprobe-$VERSION-schema.binpb" | LC_ALL=C sort >"$TMP/sums.want"
awk '{ print $2 }' "$OUT/SHA256SUMS" | LC_ALL=C sort >"$TMP/sums.got"
if cmp -s "$TMP/sums.want" "$TMP/sums.got"; then
    pass "SHA256SUMS lists the three archives and the schema's two files"
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
