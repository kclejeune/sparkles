#!/usr/bin/env python3
"""Summarize bench-indexed-commit run directories: tps, latency, sync time by path,
commits per fence, and device writes and flushes from /proc/diskstats.
Usage: runsum.py CAMPAIGN_DIR... """
import json, pathlib, re, sys
M = re.compile(r'^(sparkles_commit_sync_seconds|sparkles_group_commit_size|sparkles_group_commit_wait_seconds)_(sum|count)\{([^}]*)\} ([0-9.eE+-]+)$')
def scrape(p):
    out = {}
    if not p.exists():
        return out
    for line in p.read_text().splitlines():
        m = M.match(line)
        if m:
            name, kind, labels, v = m.groups()
            path = re.search(r'path="([^"]+)"', labels)
            key = (name, path.group(1) if path else "", kind)
            out[key] = out.get(key, 0.0) + float(v)
    return out
def disk(p, dev="nvme0n1"):
    for line in p.read_text().splitlines():
        f = line.split()
        if f[2] == dev:
            # writes completed=f[7], sectors written=f[9], flushes=f[19] if present
            return int(f[7]), int(f[9]), int(f[18]) if len(f) > 18 else 0, int(f[10]), int(f[19]) if len(f) > 19 else 0
    return (0, 0, 0, 0, 0)
rows = []
for root in sys.argv[1:]:
    for res in sorted(pathlib.Path(root).glob("*/*/result.json")):
        d = res.parent
        r = json.loads(res.read_text())
        a, b = scrape(d / "metrics.txt"), scrape(d / "before" / "metrics.txt")
        delta = {k: a[k] - b.get(k, 0.0) for k in a}
        def mean(name, path):
            c = delta.get((name, path, "count"), 0)
            return (delta.get((name, path, "sum"), 0) / c, int(c)) if c else (None, 0)
        da, db = disk(d / "disk-after.txt"), disk(d / "disk-before.txt")
        commits = sum(r.get("per_client") or [0])
        w = r.get("writers") or {}
        rd = r.get("readers") or {}
        row = dict(run=str(d.relative_to(pathlib.Path(root).parent)), grouped=r["grouped"], tps=round(r["tps"], 1),
                   p50=w.get("p50_ms"), p90=w.get("p90_ms"),
                   direct=mean("sparkles_commit_sync_seconds", "direct"), buffered=mean("sparkles_commit_sync_seconds", "buffered"),
                   group=mean("sparkles_commit_sync_seconds", "group"), size=mean("sparkles_group_commit_size", ""),
                   wait=mean("sparkles_group_commit_wait_seconds", ""),
                   dev_writes_per_commit=round((da[0] - db[0]) / max(commits, 1), 2), flushes_per_commit=round((da[2] - db[2]) / max(commits, 1), 3),
                   lag=r.get("max_schedule_lag_ms"), rp50=rd.get("p50_ms"), rp99=rd.get("p99_ms"))
        fmt = lambda t: "-" if t[0] is None else f"{t[0]*1e3:.3f}ms/{t[1]}" if t is not row["size"] else f"{t[0]:.2f}/{t[1]}"
        probe = r.get("drive_probe") or {}
        pr = f' probe={probe["direct_p50_ms"]:.2f}/{probe["fdatasync_p50_ms"]:.2f}' if probe else ""
        print(f'{row["run"]:60s} {r.get("variant", "")[:1]} g={int(row["grouped"])} tps={row["tps"]:8.1f} p50={row["p50"]:.3f} p90={row["p90"]:.3f} '
              f'direct={fmt(row["direct"])} buf={fmt(row["buffered"])} group={fmt(row["group"])} size={fmt(row["size"])} '
              f'w/c={row["dev_writes_per_commit"]} fl/c={row["flushes_per_commit"]}' + pr + f' wms/w={(da[3]-db[3])/max(da[0]-db[0],1):.3f} fms/f={(da[4]-db[4])/max(da[2]-db[2],1):.3f}'
              + (f' lag={row["lag"]:.1f} rp50={row["rp50"]:.3f} rp99={row["rp99"]:.3f}' if row["rp50"] else ""))
