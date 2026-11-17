#!/bin/sh
# Check the schema tools/stage-schema.sh stages: the archive holds exactly
# the tracked buf.yaml, LICENSE and proto/ files with their bytes, every
# schema file sets the Java options docs/api-clients.md promises, and the
# descriptor set holds every schema file with its imports and comments. With
# no out-dir, stage into a temporary folder first.
#
# Usage: sh tools/stage-schema-test.sh [out-dir version]
# POSIX sh. Needs buf, git, tar and gzip.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
if [ $# -eq 2 ]; then
    OUT=$(CDPATH= cd -- "$1" && pwd)
    VERSION=$2
else
    OUT=$TMP/out
    VERSION=0.0.0-test
    sh "$ROOT/tools/stage-schema.sh" "$OUT" "$VERSION" >/dev/null
fi
NAME=nanoprobe-$VERSION-schema
FAILED=0
fail() {
    printf 'FAIL  %s\n' "$1" >&2
    FAILED=1
}
pass() {
    printf 'ok    %s\n' "$1"
}

[ -f "$OUT/$NAME.tar.gz" ] || { printf 'no %s\n' "$OUT/$NAME.tar.gz" >&2; exit 2; }
[ -f "$OUT/$NAME.binpb" ] || { printf 'no %s\n' "$OUT/$NAME.binpb" >&2; exit 2; }

cd "$ROOT"
git ls-files -- proto buf.yaml LICENSE | sed "s|^|$NAME/|" | LC_ALL=C sort >"$TMP/want"
tar -tzf "$OUT/$NAME.tar.gz" | grep -v '/$' | LC_ALL=C sort >"$TMP/got"
if cmp -s "$TMP/want" "$TMP/got"; then
    pass "the schema archive lists exactly buf.yaml, LICENSE and proto/"
else
    fail "the schema archive's file list differs (- expected, + found):"
    diff "$TMP/want" "$TMP/got" >&2 || true
fi

mkdir "$TMP/x"
(cd "$TMP/x" && tar -xzf "$OUT/$NAME.tar.gz")
same=1
while IFS= read -r f; do
    cmp -s "$f" "$TMP/x/$NAME/$f" || { same=0; fail "$f differs in the archive"; }
done <<EOF
$(git ls-files -- proto buf.yaml LICENSE)
EOF
[ "$same" -eq 1 ] && pass "the archive's files match the repository's"

# Java and Kotlin code lands in one package, one class per message.
lacking=
for f in $(git ls-files -- 'proto/*.proto'); do
    grep -qx 'option java_multiple_files = true;' "$f" &&
        grep -qx 'option java_package = "nanoprobe.v1alpha1";' "$f" || lacking="$lacking $f"
done
if [ -z "$lacking" ]; then
    pass "every schema file sets java_package and java_multiple_files"
else
    fail "these schema files lack java_package or java_multiple_files:$lacking"
fi

# The descriptor set names each schema file, and the well-known types they
# import, so a reflection-free client can decode every message.
buf build "$OUT/$NAME.binpb" -o "$TMP/files.json#format=json" 2>"$TMP/err" ||
    { fail "buf cannot read the descriptor set: $(cat "$TMP/err")"; }
missing=
for f in $(git ls-files -- proto | sed 's|^proto/||'); do
    grep -q "\"name\": *\"$f\"" "$TMP/files.json" || missing="$missing $f"
done
grep -q '"name": *"google/protobuf/' "$TMP/files.json" || missing="$missing google/protobuf/*"
if [ -z "$missing" ]; then
    pass "the descriptor set holds every schema file and its imports"
else
    fail "the descriptor set lacks:$missing"
fi
# Comments, which documentation generators and reflection clients show.
if grep -q '"sourceCodeInfo"' "$TMP/files.json"; then
    pass "the descriptor set keeps the schema's comments"
else
    fail "the descriptor set has no source info, so no comments"
fi

exit "$FAILED"
