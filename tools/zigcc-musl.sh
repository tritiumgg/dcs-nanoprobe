#!/bin/sh
# A C compiler for x86_64-unknown-linux-musl: zig's clang with its own musl
# headers. The sidecar's bundled SQLite needs one for the static Linux build.
# The cc crate passes Rust's target triple, which zig does not know; zig gets
# its own name for the target instead.
set -eu
for arg do
    shift
    case $arg in
        --target=*) ;;
        *) set -- "$@" "$arg" ;;
    esac
done
exec zig cc -target x86_64-linux-musl "$@"
