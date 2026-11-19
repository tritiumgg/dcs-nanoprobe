"""Count the deaths in a campaign's stored event history.

Usage: python3 soak-deaths.py <events.sqlite>

Opens a copy of the sidecar's `events.sqlite` read only and decodes each stored
event's Protobuf body by hand (no generated code). An event whose world event
(field 3) has DCS event ID 8, S_EVENT_DEAD, is a death (LIVE in
docs/results/2026-11-13-event-catalogue.md). Prints the number of
events and deaths, deaths without an initiator ID and initiator IDs seen twice,
the deaths by initiator type (field 13) and by event name (field 2), and the
deaths grouped into bursts 30 s apart.

Python 3 standard library only.
"""

import collections
import sqlite3
import sys


def varint(b, i):
    r = s = 0
    while True:
        c = b[i]
        i += 1
        r |= (c & 0x7F) << s
        s += 7
        if c < 0x80:
            return r, i


def fields(b):
    i, out = 0, []
    while i < len(b):
        k, i = varint(b, i)
        f, t = k >> 3, k & 7
        if t == 0:
            v, i = varint(b, i)
        elif t == 2:
            n, i = varint(b, i)
            v = b[i:i + n]
            i += n
        elif t == 1:
            v = b[i:i + 8]
            i += 8
        elif t == 5:
            v = b[i:i + 4]
            i += 4
        else:
            raise ValueError(t)
        out.append((f, v))
    return out


def main():
    db = sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True)
    rows = db.execute("SELECT seq, wall_ms, body FROM event ORDER BY seq").fetchall()
    deaths = []
    for seq, wall, body in rows:
        for f, v in fields(body):
            if f == 3:
                w = {}
                for g, x in fields(v):
                    w.setdefault(g, x)
                if w.get(1) == 8:
                    deaths.append((seq, wall, w.get(3), (w.get(13) or b"").decode(),
                                   (w.get(2) or b"").decode()))
    print("events", len(rows), "deaths", len(deaths))
    ids = collections.Counter(d[2] for d in deaths)
    print("without an initiator ID:", ids.get(None, 0),
          "distinct initiator IDs:", len([k for k in ids if k is not None]),
          "initiator IDs seen twice or more:", sum(1 for k, n in ids.items() if k is not None and n > 1))
    print("initiator types:", collections.Counter(d[3] for d in deaths).most_common(8))
    print("event names:", collections.Counter(d[4] for d in deaths).most_common(4))
    bursts, cur = [], []
    for d in deaths:
        if cur and d[1] - cur[-1][1] > 30_000:
            bursts.append(cur)
            cur = []
        cur.append(d)
    if cur:
        bursts.append(cur)
    print("bursts", len(bursts), "sizes", collections.Counter(len(b) for b in bursts).most_common())


if __name__ == "__main__":
    main()
