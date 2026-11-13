#!/bin/sh
# Check that every agent entry point owns nothing, in the product build.
#
# An entry can raise a Lua error, and a raise is a longjmp out of lua.dll. On
# x64 Windows a longjmp unwinds every frame it crosses and runs the cleanup of
# each value with a destructor, in the middle of Lua's error handling; DCS
# exits. The entry shape in crates/agent/src/entries.rs keeps the work in an
# #[inline(never)] callee that returns, so the entry holds nothing when it
# calls lua_error. The optimizer may still undo that by inlining, so this reads
# the assembly of the shipped DLL: MSVC-style exception handling names every
# destructor funclet after the function it belongs to, and an entry that owns
# nothing has none.
#
# The entries are read from the `entries!` block in entries.rs, so a new entry
# is checked without editing this file. Each is found by the tail of its v0
# mangled name: the module `7entries`, then the name with its length.
#
# With no argument, builds the agent for x86_64-pc-windows-msvc in release with
# its default features, as the cdylib that ships, and reads the assembly beside
# it. FEATURES adds features to that build: FEATURES=faults checks the --faults
# build, which has fault injection. That build goes to target/faults, so the
# release path keeps the shipping DLL. With one argument, reads that assembly
# file instead.
#
# POSIX sh and awk. Exits 1 on a finding, 2 when nothing could be read.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=x86_64-pc-windows-msvc
SOURCE=$ROOT/crates/agent/src/entries.rs

if [ $# -ge 1 ]; then
    ASM=$1
else
    case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN* | Windows_NT) CARGO='cargo rustc' ;;
        *) CARGO='cargo xwin rustc' ;;
    esac
    DIR=$ROOT/target
    [ -z "${FEATURES:-}" ] || DIR=$ROOT/target/faults
    ASM=$DIR/$TARGET/release/deps/nanoprobe.s
    # Cargo emits assembly only when it compiles, so the old file is removed
    # and the crate root touched to force a compile.
    rm -f "$ASM"
    touch "$ROOT/crates/agent/src/lib.rs"
    (cd "$ROOT" && $CARGO -p nanoprobe-agent --release --target "$TARGET" \
        --target-dir "$DIR" ${FEATURES:+--features "$FEATURES"} -- --emit=asm)
fi

[ -f "$ASM" ] || {
    printf 'no assembly to read at %s\n' "$ASM" >&2
    exit 2
}

# `name => work...;` lines inside `entries! { ... }`.
ENTRIES=$(awk '
    /^entries! \{/ { inside = 1; next }
    inside && /^\}/ { exit }
    inside && /=>/ { print $1 }
' "$SOURCE")

[ -n "$ENTRIES" ] || {
    printf 'no entries found in %s\n' "$SOURCE" >&2
    exit 2
}

status=0
count=0
for entry in $ENTRIES; do
    count=$((count + 1))
    tail="7entries${#entry}${entry}"
    alloc=$(awk -v s="$tail" '
        $0 ~ "^_RNv[A-Za-z0-9_]*" s ":$" { found = 1 }
        found && /\.seh_stackalloc/ { print $2; exit }
        found && /\.seh_endproc/ { print "0"; exit }
    ' "$ASM")
    if [ -z "$alloc" ]; then
        printf 'MISSING  %-14s not in %s\n' "$entry" "$ASM"
        status=1
        continue
    fi
    dtors=$(awk -v s="$tail" '
        $0 ~ "^\"\\?dtor\\$[0-9]+@\\?0\\?_RNv[A-Za-z0-9_]*" s "@4HA\":$" { n++ }
        END { print n + 0 }
    ' "$ASM")
    if [ "$dtors" -eq 0 ]; then
        printf 'ok       %-14s stack %-4s destructors 0\n' "$entry" "$alloc"
    else
        printf 'OWNS     %-14s stack %-4s destructors %s\n' "$entry" "$alloc" "$dtors"
        status=1
    fi
done

if [ "$status" -ne 0 ]; then
    printf '\nAn entry owns a value with a destructor, or was not found. Keep the work\n'
    printf 'in the worker that guarded() calls; see crates/agent/src/entries.rs.\n'
    exit 1
fi
printf 'all %s entries own nothing\n' "$count"
