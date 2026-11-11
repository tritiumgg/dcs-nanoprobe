"""The native peer for experiment E24, run by `cargo xtask mapped`.

It maps the file the agent maps inside DCS under Wine, and for a number of
seconds reads the eight payload words the agent writes under a sequence lock,
counting torn reads, and writes back the last whole value it read (word 16)
and its own counter (word 17). It prints one JSON object.

Usage: python3 mapped-peer.py <path> <seconds>
Words are little-endian u64: 0 magic, 1 sequence, 2..9 payload, 16 echo,
17 peer sequence. Python 3 standard library only.
"""

import json
import mmap
import os
import struct
import sys
import time

MAGIC = 0x6E706D6170706564
SIZE = 512


def main():
    path, seconds = sys.argv[1], float(sys.argv[2])
    report = {"path": path, "seconds": seconds}
    deadline = time.monotonic() + 60
    while not os.path.exists(path) or os.path.getsize(path) < SIZE:
        if time.monotonic() > deadline:
            report["error"] = "the file did not appear within 60 s"
            print(json.dumps(report))
            return 1
        time.sleep(0.05)
    with open(path, "r+b") as f:
        m = mmap.mmap(f.fileno(), SIZE)

    def word(i):
        return struct.unpack_from("<Q", m, i * 8)[0]

    def store(i, v):
        struct.pack_into("<Q", m, i * 8, v)

    while word(0) != MAGIC:
        if time.monotonic() > deadline:
            report["error"] = "no magic within 60 s"
            print(json.dumps(report))
            return 1
        time.sleep(0.01)

    reads = torn = busy = changes = 0
    last = None
    first_value = None
    peer = 0
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        s1 = word(1)
        if s1 & 1:
            busy += 1
            continue
        payload = [word(i) for i in range(2, 10)]
        s2 = word(1)
        reads += 1
        if s1 != s2:
            busy += 1
            continue
        if len(set(payload)) != 1:
            torn += 1
            continue
        value = payload[0]
        if value != last:
            changes += 1
            last = value
            if first_value is None:
                first_value = value
            store(16, value)
        peer += 1
        store(17, peer)
        time.sleep(0.0005)
    report.update(
        reads=reads,
        torn=torn,
        retried=busy,
        values_seen=changes,
        first_value=first_value,
        last_value=last,
        peer_sequence=peer,
    )
    m.close()
    print(json.dumps(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())
