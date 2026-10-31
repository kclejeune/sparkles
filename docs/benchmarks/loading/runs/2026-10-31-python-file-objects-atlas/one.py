import json, resource, sys, time
import sparkles
path, fmt, mode, db = sys.argv[1:5]
ds = sparkles.Dataset(db)
t = time.perf_counter()
if mode == "path":
    n = ds.load(path=path, format=fmt)
else:
    with open(path, "rb") as f:
        n = ds.load(f, fmt, parse_mode=mode)
dt = time.perf_counter() - t
rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss * 1024
print(json.dumps({"fmt": fmt, "mode": mode, "quads": n, "len": len(ds), "seconds": dt, "peak_rss": rss}))
