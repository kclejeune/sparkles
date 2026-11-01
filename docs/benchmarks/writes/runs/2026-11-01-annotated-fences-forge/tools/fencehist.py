#!/usr/bin/env python3
"""Per-process distribution of group commit fence synchronization times from the
server's sparkles_commit_sync_seconds histogram (path="group"), the counts after the
process less those before it, in the buckets up to 0.5 ms, 0.5 to 1 ms, 1 to 2.5 ms and
over 2.5 ms. Usage: fencehist.py CAMPAIGN_DIR"""
import pathlib, re, sys
B = re.compile(r'^sparkles_commit_sync_seconds_bucket\{([^}]*)\} (\S+)$')
def buckets(p):
    out = {}
    for line in p.read_text().splitlines():
        m = B.match(line)
        if m and 'path="group"' in m.group(1) and 'dataset="bench"' in m.group(1):
            le = re.search(r'le="([^"]+)"', m.group(1)).group(1)
            out[le] = out.get(le, 0) + float(m.group(2))
    return out
root = pathlib.Path(sys.argv[1])
for d in sorted(root.glob("*/b*-*")):
    a, b = d / "metrics.txt", d / "before" / "metrics.txt"
    if not a.exists() or not b.exists():
        continue
    x, y = buckets(a), buckets(b)
    c = {k: x.get(k, 0) - y.get(k, 0) for k in x}
    total = c.get("+Inf", 0)
    if not total:
        continue
    edges = [("0.0005", "<=0.5"), ("0.001", "0.5-1"), ("0.0025", "1-2.5"), ("+Inf", ">2.5")]
    prev, parts = 0, []
    for le, name in edges:
        parts.append(f"{name}={100 * (c[le] - prev) / total:5.1f}%")
        prev = c[le]
    print(f"{d.parent.name}/{d.name:16} fences={int(total):6} " + " ".join(parts))
