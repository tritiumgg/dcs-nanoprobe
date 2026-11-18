#!/bin/sh
# Check that a pushed tag may build a release (.github/workflows/release.yml):
# the tag is `v` and the workspace version in Cargo.toml, and the commit it
# names is on origin/main. Prints the version.
#
# Run in the repository's checkout, with origin/main fetched.
#
# Usage: sh tools/release-tag-check.sh <tag>
# POSIX sh. Needs git and awk.
set -eu

[ $# -eq 1 ] || { printf 'usage: sh tools/release-tag-check.sh <tag>\n' >&2; exit 2; }
tag=$1
ROOT=$(git rev-parse --show-toplevel)

VERSION=$(awk '
    /^\[workspace\.package\]/ { in_sec = 1; next }
    /^\[/ { in_sec = 0 }
    in_sec && /^version[[:space:]]*=/ {
        sub(/^version[[:space:]]*=[[:space:]]*"/, "")
        sub(/".*$/, "")
        print; exit
    }
' "$ROOT/Cargo.toml")
[ -n "$VERSION" ] || { printf 'no version under [workspace.package] in Cargo.toml\n' >&2; exit 2; }

if [ "$tag" != "v$VERSION" ]; then
    printf 'the tag %s is not v%s, the workspace version in Cargo.toml\n' "$tag" "$VERSION" >&2
    exit 1
fi
commit=$(git -C "$ROOT" rev-parse --verify --quiet "refs/tags/$tag^{commit}") || {
    printf 'no tag %s in this repository\n' "$tag" >&2
    exit 1
}
git -C "$ROOT" rev-parse --verify --quiet 'refs/remotes/origin/main^{commit}' >/dev/null || {
    printf 'no origin/main to check the tag against\n' >&2
    exit 1
}
if ! git -C "$ROOT" merge-base --is-ancestor "$commit" refs/remotes/origin/main; then
    printf 'the tag %s names %s, which is not on main\n' "$tag" "$commit" >&2
    exit 1
fi
printf '%s\n' "$VERSION"
