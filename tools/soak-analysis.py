"""Read a campaign soak's folder and print the sidecar's figures by window.

Usage: python3 soak-analysis.py <folder> [--from-minute M]

The folder is one `cargo xtask soak campaign` wrote. Memory is in MiB (the records'
`rss_kb` is KiB), CPU in percent of a core. The script prints:
- the sidecar's resident memory by half hour, with the event rows at each end;
- least-squares slopes of resident memory from minute M (default 100) to the end and
  over fixed windows the run covers, with one standard error widened for the
  residuals' lag-1 autocorrelation;
- the knee: the best two-segment fit of resident memory from minute 30, with the
  event rows at the break, and the minute the event sequence passes 16,384;
- the sidecar's and DCS's mean CPU over fixed windows the run covers, and the
  difference between minutes 70 to 120 and the last 50 minutes when the run is long
  enough for the two not to overlap;
- the sidecar's CPU slope from minute 90, alone and with DCS's CPU as a second
  regressor, and the correlation of the two per record and by 5-minute block medians;
- deaths on the event stream by hour and the records where they jump by more than 25;
- the median commit time by half hour and in 5-minute blocks, from the
  `commit_timings` lines of the sidecar's first boot in
  `nanoprobe-xtask-persistence.log`, one line every 10 s; a trailing block shorter
  than its length is marked "(part)".

Python 3 standard library only.
"""

import json
import math
import statistics as st
import sys

TIMINGS_S = 10
RING_SLOTS = 16384
RSS_WINDOWS = [(60, 90), (75, 120), (92, 120), (100, 120), (100, 170), (170, 240)]
CPU_WINDOWS = [(5, 45), (60, 90), (92, 120), (70, 120), (190, 240)]


def ols(rows, y):
    """Least squares. Returns coefficients, standard errors widened for lag-1
    autocorrelation, and that autocorrelation."""
    n, k = len(y), len(rows[0])
    a = [[sum(r[i] * r[j] for r in rows) for j in range(k)] + [float(i == j) for j in range(k)]
         for i in range(k)]
    xty = [sum(rows[r][i] * y[r] for r in range(n)) for i in range(k)]
    for c in range(k):
        p = max(range(c, k), key=lambda r: abs(a[r][c]))
        a[c], a[p] = a[p], a[c]
        a[c] = [v / a[c][c] for v in a[c]]
        for r in range(k):
            if r != c:
                f = a[r][c]
                a[r] = [u - f * v for u, v in zip(a[r], a[c])]
    inv = [row[k:] for row in a]
    b = [sum(inv[i][j] * xty[j] for j in range(k)) for i in range(k)]
    res = [y[r] - sum(b[i] * rows[r][i] for i in range(k)) for r in range(n)]
    s2 = sum(e * e for e in res) / (n - k)
    r1 = sum(res[i] * res[i - 1] for i in range(1, n)) / sum(e * e for e in res)
    widen = math.sqrt((1 + r1) / (1 - r1)) if 0 < r1 < 1 else 1.0
    return b, [math.sqrt(s2 * inv[i][i]) * widen for i in range(k)], r1


def main():
    folder = sys.argv[1]
    start = float(sys.argv[sys.argv.index("--from-minute") + 1]) if "--from-minute" in sys.argv else 100.0
    recs = [json.loads(line) for line in open(folder + "/records.jsonl")]
    n = len(recs)
    t = [r["t_s"] / 60 for r in recs]
    rss = [r["sidecar"]["rss_kb"] / 1024 for r in recs]
    cpu = [r["sidecar"].get("cpu_percent") for r in recs]
    dcs = [r["dcs"].get("cpu_percent") for r in recs]
    rows = [r["store_size"]["event_rows"] for r in recs]
    seq = [r["streams"]["event_sequence"] for r in recs]
    deaths = [r["streams"]["deaths"] for r in recs]
    kills = [r["churn"].get("kills", 0) for r in recs]
    end = t[-1]

    def among(a, z, *series):
        return [i for i in range(n) if a <= t[i] <= z and all(s[i] is not None for s in series)]

    def slope(idx, y):
        b, se, r1 = ols([[1, t[i] / 60] for i in idx], [y[i] for i in idx])
        return b[1], se[1], r1

    print("Resident memory by half hour (mean MiB, end MiB, event rows at end):")
    for h in range(int(math.ceil(end / 30))):
        idx = [i for i in range(n) if h * 30 < t[i] <= (h + 1) * 30]
        if idx:
            part = " (part)" if (h + 1) * 30 > end else ""
            print(f"  {h * 30:3d}-{(h + 1) * 30:3d}: {st.mean(rss[i] for i in idx):.2f} "
                  f"{rss[idx[-1]]:.2f} {rows[idx[-1]]}{part}")

    print("Resident memory slopes (MiB an hour ± one widened standard error):")
    for a, z in dict.fromkeys([(start, end)] + [w for w in RSS_WINDOWS if w[1] <= end + 1]):
        idx = among(a, z)
        if len(idx) > 5:
            s, e, r1 = slope(idx, rss)
            print(f"  minutes {a:.0f} to {z:.0f}: {s:.3f} ± {e:.3f} (lag-1 r {r1:.2f})")

    idx = [i for i in range(n) if t[i] >= 30]
    best = None
    for k in range(10, len(idx) - 10):
        tk = t[idx[k]]
        x = [[1, t[i] / 60, max(0, t[i] - tk) / 60] for i in idx]
        c, _, _ = ols(x, [rss[i] for i in idx])
        sse = sum((rss[i] - sum(c[j] * xi[j] for j in range(3))) ** 2 for i, xi in zip(idx, x))
        if best is None or sse < best[0]:
            best = (sse, tk, c, rows[idx[k]])
    if best:
        print(f"Knee: minute {best[1]:.0f}, {best[3]} event rows; {best[2][1]:.2f} MiB an hour "
              f"before, {best[2][1] + best[2][2]:.2f} after")
    fill = next((i for i in range(n) if seq[i] >= RING_SLOTS), None)
    if fill is not None:
        print(f"Event sequence passes {RING_SLOTS}: minute {t[fill]:.0f} ({seq[fill]})")

    print("Mean CPU, percent of a core (sidecar, DCS):")
    for a, z in dict.fromkeys(CPU_WINDOWS + [(end - 50, end)]):
        idx = among(a, z, cpu, dcs)
        if z <= end + 1 and len(idx) > 5:
            print(f"  minutes {a:.0f} to {z:.0f}: {st.mean(cpu[i] for i in idx):.2f} "
                  f"(sd {st.stdev(cpu[i] for i in idx):.2f}), {st.mean(dcs[i] for i in idx):.2f}")
    early, late = among(70, 120, cpu), among(end - 50, end, cpu)
    if end - 50 > 120 and len(early) > 5 and len(late) > 5:
        e1, e2 = [cpu[i] for i in early], [cpu[i] for i in late]
        print(f"  difference, last 50 minutes less 70 to 120: {st.mean(e2) - st.mean(e1):+.2f} ± "
              f"{math.sqrt(st.variance(e1) / len(e1) + st.variance(e2) / len(e2)):.2f}")
    else:
        print("  difference, last 50 minutes less 70 to 120: the windows overlap; not computed")

    idx = among(90, end, cpu, dcs)
    if len(idx) > 10:
        b, se, _ = ols([[1, t[i] / 60, dcs[i]] for i in idx], [cpu[i] for i in idx])
        s0, e0, _ = slope(idx, cpu)
        print(f"Sidecar CPU from minute 90: {b[1]:+.3f} ± {se[1]:.3f} points an hour with DCS CPU "
              f"({b[2]:.3f} ± {se[2]:.3f} per DCS point); alone {s0:+.3f} ± {e0:.3f}")

    def blocks(a):
        out = []
        for m in range(int(a // 5) * 5, int(end), 5):
            idx = among(m, m + 5 - 1e-9, cpu, dcs)
            if idx:
                out.append((st.median(cpu[i] for i in idx), st.median(dcs[i] for i in idx)))
        return out

    for a in (0, 90):
        idx = among(a, end, cpu, dcs)
        bl = blocks(a)
        if len(idx) > 10 and len(bl) > 3:
            print(f"Correlation of sidecar and DCS CPU from minute {a}: per record "
                  f"{st.correlation([cpu[i] for i in idx], [dcs[i] for i in idx]):.2f}, "
                  f"by 5-minute block {st.correlation([x for x, _ in bl], [y for _, y in bl]):.2f}")

    print("Deaths on the event stream by hour:")
    prev = 0
    for h in range(int(math.ceil(end / 60))):
        idx = [i for i in range(n) if t[i] <= (h + 1) * 60]
        last = deaths[idx[-1]]
        part = " (part)" if (h + 1) * 60 > end else ""
        print(f"  hour {h + 1}: {last - prev}{part}")
        prev = last
    print("Records where deaths jump by more than 25 (minute, jump, kills so far):")
    print("  " + "; ".join(f"{t[i]:.0f} +{deaths[i] - deaths[i - 1]} {kills[i]:.0f}"
                           for i in range(1, n) if deaths[i] - deaths[i - 1] > 25))

    log = open(folder + "/nanoprobe-xtask-persistence.log").read()
    boot1 = log.split("--- boot 2")[0].splitlines()
    lines = [json.loads(x) for x in boot1 if '"commit_timings"' in x]
    per_half = 1800 // TIMINGS_S
    print("Median commit by half hour (µs; p99; commits a line):")
    for h in range(0, len(lines), per_half):
        seg = lines[h:h + per_half]
        part = " (part)" if len(seg) < per_half else ""
        print(f"  {h // per_half * 30:3d}: {st.median(c['median_us'] for c in seg):.0f} "
              f"{st.median(c['p99_us'] for c in seg):.0f} {st.median(c['commits'] for c in seg):.0f}{part}")
    per5 = 300 // TIMINGS_S
    print("Median commit in 5-minute blocks (µs):")
    print("  " + " ".join(
        f"{h // per5 * 5}:{st.median(c['median_us'] for c in lines[h:h + per5]):.0f}"
        + ("(part)" if len(lines[h:h + per5]) < per5 else "")
        for h in range(0, len(lines), per5)))


if __name__ == "__main__":
    main()
