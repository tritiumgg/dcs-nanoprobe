# Rewrite the Markdown links of one repository file for a release.
# tools/release-notes.sh uses it for the release page, and
# tools/stage-release.sh for the documents the archives carry
# (spec/install.md, "Release").
#
# Variables, each optional:
#
#   base   With base set (such as https://github.com/<owner>/<repo>/blob/<tag>),
#          a relative link becomes <base>/<path>, where <path> is the link's
#          repository path. Unset, relative links stay as written.
#   dir    The repository folder of the file, such as `docs`; empty at the root.
#          A leading `../` of a link climbs out of it.
#   keep   A file of repository paths, one per line; blank lines and lines
#          that start with `#` are skipped. A relative link to one of
#          them, or to a folder that holds one, stays as written: the archive
#          holds it at the same path.
#   repo   With repo and ref set, each link to <repo>/blob/main/ or
#          <repo>/tree/main/ names <ref> in place of main, in fenced code
#          too.
#   ref    See repo.
#
# A link is the target of `](...)` outside a fenced code block. A target with
# a scheme, `mailto:`, a leading `#` or a leading `/` is not relative. A
# fragment stays with its link.
#
# Usage: awk -f tools/release-links.awk -v base=... -v dir=... -v keep=... \
#            -v repo=... -v ref=... <file>
# POSIX awk.

BEGIN {
    nkeep = 0
    if (keep != "") {
        while ((getline line < keep) > 0) {
            if (line == "" || line ~ /^#/) continue
            kept[line] = 1
            # Every folder that holds a kept file resolves too.
            p = line
            while (sub(/\/[^\/]*$/, "", p)) kept[p] = 1
        }
        close(keep)
    }
}

# The repository path a relative link names, from the file's folder.
function resolve(path,    d) {
    d = dir
    while (d != "" && substr(path, 1, 3) == "../") {
        path = substr(path, 4)
        if (!sub(/\/[^\/]*$/, "", d)) d = ""
    }
    return d == "" ? path : d "/" path
}

function fix(target,    path, frag, i, resolved, bare) {
    if (target ~ /^(https?:|mailto:|#|\/)/) return target
    i = index(target, "#")
    if (i > 0) {
        path = substr(target, 1, i - 1)
        frag = substr(target, i)
    } else {
        path = target
        frag = ""
    }
    resolved = resolve(path)
    bare = resolved
    sub(/\/$/, "", bare)
    if (bare in kept) return target
    if (base == "") return target
    return base "/" resolved frag
}

{
    line = $0
    if (repo != "" && ref != "") {
        gsub_main(repo "/blob/main/", repo "/blob/" ref "/")
        gsub_main(repo "/tree/main/", repo "/tree/" ref "/")
    }
    # A fenced code block holds code, not links: `f[k](x)` stays as written.
    if (line ~ /^[ \t]*(```|~~~)/) {
        fenced = !fenced
        print line
        next
    }
    if (fenced) {
        print line
        next
    }
    out = ""
    while (match(line, /\]\([^)]*\)/)) {
        target = substr(line, RSTART + 2, RLENGTH - 3)
        out = out substr(line, 1, RSTART + 1) fix(target) ")"
        line = substr(line, RSTART + RLENGTH)
    }
    print out line
}

# Replace each literal `from` in `line` with `to`; gsub would read `from` as a
# regular expression.
function gsub_main(from, to,    i, done) {
    done = ""
    while ((i = index(line, from)) > 0) {
        done = done substr(line, 1, i - 1) to
        line = substr(line, i + length(from))
    }
    line = done line
}
