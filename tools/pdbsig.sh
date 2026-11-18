#!/bin/sh
# Print the PDB signature of a PE file as the agent's crash record writes it
# in `pdb` (spec/install.md, "Agent"): the GUID of its CodeView record in 32
# hex digits, then the age in hex, as crates/xtask/src/symbols.rs formats it.
# With --guid, print instead the GUID's 16 bytes in file order as hex, the
# form the matching PDB holds them in.
#
# Usage: sh tools/pdbsig.sh [--guid] <file.dll>
# POSIX sh. Needs od and awk. Exits 1 when the file has no CodeView record.
set -eu

MODE=sig
if [ "${1:-}" = --guid ]; then
    MODE=guid
    shift
fi
[ $# -eq 1 ] || { printf 'usage: sh tools/pdbsig.sh [--guid] <file.dll>\n' >&2; exit 2; }

# The CodeView record is found as crates/xtask/src/symbols.rs finds it: the
# optional header's debug directory, through the section table, to the entry
# of type 2 whose data starts "RSDS": 16 bytes of GUID, then a 4-byte
# little-endian age.
od -An -tx1 -v "$1" | awk -v mode="$MODE" '
    function le(at, k,    v, j) {
        v = 0
        for (j = k - 1; j >= 0; j--) v = v * 256 + hex[b[at + j]]
        return v
    }
    function offset(rva,    s, i, va, size, raw) {
        for (i = 0; i < sections; i++) {
            s = table + i * 40
            va = le(s + 12, 4); size = le(s + 16, 4); raw = le(s + 20, 4)
            if (rva >= va && rva < va + size) return raw + rva - va
        }
        return -1
    }
    BEGIN { for (i = 0; i < 256; i++) hex[sprintf("%02x", i)] = i }
    { for (i = 1; i <= NF; i++) b[n++] = $i }
    END {
        nt = le(60, 4)
        if (le(nt, 4) != 17744) exit 1
        sections = le(nt + 6, 2)
        optional = nt + 24
        if (le(optional, 2) != 523) exit 1
        table = optional + le(nt + 20, 2)
        start = offset(le(optional + 160, 4))
        size = le(optional + 164, 4)
        if (start < 0) exit 1
        for (e = start; e < start + size; e += 28) {
            if (le(e + 12, 4) != 2) continue
            d = le(e + 24, 4)
            if (b[d] != "52" || b[d+1] != "53" || b[d+2] != "44" || b[d+3] != "53") continue
            g = d + 4
            if (mode == "guid") {
                s = ""
                for (j = 0; j < 16; j++) s = s b[g+j]
                print s
                exit 0
            }
            s = b[g+3] b[g+2] b[g+1] b[g] b[g+5] b[g+4] b[g+7] b[g+6]
            for (j = 8; j < 16; j++) s = s b[g+j]
            print toupper(s) sprintf("%X", le(g + 16, 4))
            exit 0
        }
        exit 1
    }'
