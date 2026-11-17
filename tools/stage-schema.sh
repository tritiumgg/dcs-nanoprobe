#!/bin/sh
# Stage the release's API schema, which users generate client code from with
# their own language's tools (docs/api-clients.md). The GitHub release is the
# only place it is published.
#
#   <out>/nanoprobe-<v>-schema.tar.gz   one folder, nanoprobe-<v>-schema/, with
#                                       buf.yaml, LICENSE and proto/
#   <out>/nanoprobe-<v>-schema.binpb    a FileDescriptorSet of proto/ with its
#                                       imports and comments (`buf build`)
#
# tools/stage-release.sh runs it; tools/stage-schema-test.sh checks the result.
#
# Usage: sh tools/stage-schema.sh <out-dir> <version>
# POSIX sh. Needs buf, git, tar and gzip.
set -eu

[ $# -eq 2 ] || { printf 'usage: sh tools/stage-schema.sh <out-dir> <version>\n' >&2; exit 2; }
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$1"
OUT=$(CDPATH= cd -- "$1" && pwd)
NAME=nanoprobe-$2-schema
cd "$ROOT"

rm -rf "$OUT/$NAME" "$OUT/$NAME.tar.gz" "$OUT/$NAME.binpb"
mkdir "$OUT/$NAME"
# The files git tracks: a stray file in proto/ is not part of a release.
git ls-files -- proto buf.yaml LICENSE | while IFS= read -r f; do
    mkdir -p "$OUT/$NAME/$(dirname -- "$f")"
    cp "$f" "$OUT/$NAME/$f"
    chmod 0644 "$OUT/$NAME/$f"
done

# Entries in a fixed order, without macOS metadata files.
(
    cd "$OUT"
    find "$NAME" | LC_ALL=C sort >.schema-list
    COPYFILE_DISABLE=1 tar --numeric-owner --no-recursion -cf - -T .schema-list | gzip -n -9 >"$NAME.tar.gz"
    rm .schema-list
)

# Built from the staged copy, so the descriptor set and the archive match.
(cd "$OUT/$NAME" && buf build --as-file-descriptor-set -o "$OUT/$NAME.binpb")
rm -rf "$OUT/$NAME"
printf 'staged %s.tar.gz and %s.binpb in %s\n' "$NAME" "$NAME" "$OUT"
