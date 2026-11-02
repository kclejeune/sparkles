#!/usr/bin/env python3
"""warm.py <port> <dataset,...> <n> <query-file-or-text>...: median ms of n warm requests per dataset and query, interleaved"""
import http.client, sys, time, urllib.parse, statistics, os
port, dss, n = int(sys.argv[1]), sys.argv[2].split(","), int(sys.argv[3])
qs = []
for a in sys.argv[4:]:
    qs.append((os.path.basename(a), open(a).read()) if os.path.exists(a) else (a[:40], a))
P = "PREFIX foaf: <http://xmlns.com/foaf/0.1/> PREFIX ex: <http://example.org/> "
def run(ds, q):
    c = http.client.HTTPConnection("localhost", port)
    body = urllib.parse.urlencode({"query": q if q.lstrip().upper().startswith(("PREFIX","SELECT","ASK")) and "foaf:" not in q else P + q})
    t = time.perf_counter()
    c.request("POST", f"/{ds}/sparql", body, {"Content-Type": "application/x-www-form-urlencoded", "Accept": "text/tab-separated-values"})
    r = c.getresponse(); data = r.read()
    el = (time.perf_counter() - t) * 1000
    c.close()
    assert r.status == 200, (r.status, data[:300])
    return el, data
for name, q in qs:
    res = {d: [] for d in dss}; rows = {}
    for d in dss:
        for _ in range(3): run(d, q)
    for i in range(n):
        for d in dss:
            el, data = run(d, q); res[d].append(el); rows[d] = data.count(b"\n") - 1
    print(name, "  ".join(f"{d}: {statistics.median(v):.3f} ms ({rows[d]} rows)" for d, v in res.items()))
