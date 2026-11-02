import json, statistics, sys
passes = sys.argv[1:] or ["base-first", "new-first", "new-second"]
recs = {p: json.load(open(f"bsbm-{p}/record.json")) for p in passes}
qs = sorted({r["query"] for r in recs[passes[0]]["results"]})
print("query".ljust(10) + "".join(f"{p}/{e[:2]}".rjust(16) for p in passes for e in ("sparkles", "qlever")))
for q in qs:
    row = q.ljust(10)
    for p in passes:
        for e in ("sparkles", "qlever"):
            m = [r for r in recs[p]["results"] if r["query"] == q and r["engine"] == e]
            row += (f"{m[0]['median_ms']:.2f}{'' if m[0]['status'] == 'ok' else '*'}" if m else "-").rjust(16)
    print(row)
r0 = recs[passes[0]]["results"][0]
print({k: r0[k] for k in r0 if k not in ("samples_ms",)})
