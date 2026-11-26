#!/bin/sh
# Check tools/draft-release-notes.sh on a written history: a tag, then merged
# and squashed commits of every type, with and without `Release-note:`
# trailers. The expected draft is tools/testdata/draft-release-notes.golden.
#
# Runs in a scratch repository.
#
# Usage: sh tools/draft-release-notes-test.sh
# POSIX sh. Needs git and awk.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DRAFT=$ROOT/tools/draft-release-notes.sh
GOLDEN=$ROOT/tools/testdata/draft-release-notes.golden

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT INT TERM
R=$SCRATCH/repo

git init -q "$R"
git -C "$R" config user.name test
git -C "$R" config user.email test@example.invalid
git -C "$R" config commit.gpgsign false
git -C "$R" config tag.gpgsign false
mkdir -p "$R/docs/adr" "$R/docs/results"
printf 'x\n' >"$R/docs/adr/0044-heliports.md"
printf 'x\n' >"$R/docs/results/2026-11-25-farp.md"

n=0
commit() { # subject, body
    n=$((n + 1))
    printf '%s\n' "$n" >"$R/f"
    git -C "$R" add -A
    git -C "$R" commit -q -m "$1" -m "$2"
}

commit 'feat: before the tag' 'Release-note: Not in the draft.'
git -C "$R" tag v0.0.1

# Squashed commits on the first-parent line.
commit 'feat(api): add a call' 'Release-note: The `Call` RPC does a thing, as
  [ADR 0044](adr/0044-heliports.md) says.
Release-note: A second entry from the same commit.'
commit 'feat(api): add a call without a trailer' 'Names ADR 0044 and results/2026-11-25-farp.md, and ADR 0999 that has no file.'
commit 'fix(store): silent fix' 'Release-note: none'
commit 'fix(store): plain fix' 'Body text only.'
commit 'refactor(api)!: rename a field' 'Body.'
commit 'perf(store): faster write' 'BREAKING CHANGE: the store layout changed.'
commit 'docs(adr): number the thing ADR 0045' ''
commit 'docs: update a page' ''
commit 'test: add a test' ''
commit 'ci: change a job' ''
commit 'chore: bump' ''
commit 'wip: half done' ''
commit 'docs: a user-visible doc change' 'Release-note: The notes now say how to draft them.'
commit 'fix(store): noted none' 'Release-note: None.'
commit 'build!: raise the minimum Rust' ''
commit 'chore: drop a platform' 'BREAKING CHANGE: the platform is gone.'
commit 'Fix a crash on load' ''
commit 'Revert "feat: something"' ''

# A merged branch: its commits count, the merge and its numbering commit do not.
BASE=$(git -C "$R" symbolic-ref --short HEAD)
git -C "$R" checkout -q -b step
commit 'feat(sim): a branch feature' 'Release-note: A branch feature.'
commit 'test: a branch test' ''
git -C "$R" checkout -q "$BASE"
git -C "$R" merge -q --no-ff step -m "Merge branch 'step': a step"
commit 'docs(adr): number the step ADR 0046' ''

(cd "$R" && sh "$DRAFT" 0.2.0) >"$SCRATCH/out"
if diff -u "$GOLDEN" "$SCRATCH/out"; then
    printf 'ok    the draft matches the golden file\n'
else
    printf 'FAIL  the draft differs from %s\n' "$GOLDEN" >&2
    exit 1
fi

if (cd "$R" && sh "$DRAFT" 0.2.0 nosuchref) >/dev/null 2>&1; then
    printf 'FAIL  a bad from-ref was accepted\n' >&2
    exit 1
fi
printf 'ok    a bad from-ref is refused\n'

if (cd "$R" && sh "$DRAFT" 0.2 >/dev/null 2>&1); then
    printf 'FAIL  a version without three parts was accepted\n' >&2
    exit 1
fi
printf 'ok    a bad version is refused\n'

# The release check refuses the drafted TODO markers, and accepts the draft
# once they are reworded and the heading is final.
mkdir -p "$R/docs"
{ printf '# Release notes\n\n'; sed 's/^## 0.2.0, Draft/## 0.2.0/' "$SCRATCH/out"; } >"$R/docs/release-notes.md"
if (cd "$R" && sh "$ROOT/tools/release-notes.sh" 0.2.0 "$SCRATCH/final") >"$SCRATCH/refusal" 2>&1; then
    printf 'FAIL  the release check accepted a TODO marker\n' >&2
    exit 1
fi
if ! grep -q 'placeholders' "$SCRATCH/refusal"; then
    printf 'FAIL  the release check refused for another reason:\n' >&2
    cat "$SCRATCH/refusal" >&2
    exit 1
fi
printf 'ok    the release check refuses a TODO marker\n'
sed -i.bak 's/ TODO: reword for users//' "$R/docs/release-notes.md"
if ! (cd "$R" && sh "$ROOT/tools/release-notes.sh" 0.2.0 "$SCRATCH/final") >/dev/null 2>&1; then
    printf 'FAIL  the release check refused an edited draft\n' >&2
    exit 1
fi
printf 'ok    the release check accepts an edited draft\n'
