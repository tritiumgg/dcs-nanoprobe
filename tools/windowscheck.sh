#!/bin/sh
# Check every crate for x86_64-pc-windows-msvc with clippy, one crate at a time.
#
# One crate at a time, because cargo builds a crate with the union of the
# features every crate in the same build asks of a dependency. In a workspace
# build, a feature one crate turns on hides that another crate forgot it.
#
# Each crate is checked with its default features, as it ships. For the agent
# that is dcs-lua: without it, the agent's Windows code compiles away. Its
# build.rs then makes the lua.dll import library with LLVM.
#
# On macOS and Linux, cargo-xwin supplies the MSVC CRT and SDK; on Windows, plain
# cargo does. The output goes to target/windows-check, apart from the DLL's
# cross-build, whose folder may hold output of another toolchain.
#
# POSIX sh. Needs LLVM, and off Windows cargo-xwin: mise.toml pins both off
# Windows, and a Windows host needs LLVM on PATH.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
export CARGO_TARGET_DIR="$ROOT/target/windows-check"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN* | Windows_NT) clippy="cargo clippy" ;;
    *) clippy="cargo xwin clippy" ;;
esac

for manifest in "$ROOT"/crates/*/Cargo.toml; do
    printf 'windows-check: %s\n' "${manifest#"$ROOT"/}"
    $clippy --manifest-path "$manifest" --all-targets \
        --target x86_64-pc-windows-msvc --quiet -- -D warnings
done
