#!/usr/bin/env python3
"""Cold point lookups alone and next to a dense export, for X07 acceptance example A6.

    x07-concurrent-cold.py --bin target/release/sparkles --db sparkles.db --out a6.json

Each sample evicts the store's files from the page cache (posix_fadvise DONTNEED, as
bench-regressions.py --cold does), starts a fresh server and sends 16 concurrent point
lookups on 16 different people. In the `alone` samples nothing else runs. In the
`with-export` samples a dense `SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 500000` starts
first, and the lookups start --delay-ms later while it runs. Samples alternate between
the two modes. The output keeps every lookup's latency, the export's latency and the
answer digests. With --strace, one extra sample of each mode runs under
`strace -e trace=madvise`, and the advice used after the server is ready is counted, so a
change of mapping advice (MADV_RANDOM, MADV_NORMAL or MADV_SEQUENTIAL) during the queries
shows up.
"""

import argparse
import hashlib
import json
import os
import re
import socket
import statistics
import subprocess
import tempfile
import threading
import time
import urllib.request

P = "PREFIX ex: <http://example.org/> PREFIX foaf: <http://xmlns.com/foaf/0.1/> "


def lookup(i):
    return P + f"SELECT ?n ?a ?s WHERE {{ <http://example.org/person/{i}> foaf:name ?n ; foaf:age ?a . OPTIONAL {{ <http://example.org/person/{i}> ex:salary ?s }} }}"


EXPORT = "SELECT ?s ?p ?o WHERE { ?s ?p ?o } LIMIT 500000"


def evict(db):
    for root, _, files in os.walk(db):
        for name in files:
            fd = os.open(os.path.join(root, name), os.O_RDONLY)
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)


def query(port, q):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}/bench/sparql", data=q.encode(),
        headers={"Content-Type": "application/sparql-query",
                 "Accept": "text/tab-separated-values"})
    t0 = time.perf_counter()
    with urllib.request.urlopen(req, timeout=600) as r:
        body = r.read()
    ms = (time.perf_counter() - t0) * 1000
    return ms, hashlib.sha256(b"\n".join(sorted(body.splitlines()))).hexdigest()


def start(a, prefix=()):
    data = tempfile.mkdtemp(prefix="x07-a6-")
    try:
        socket.create_connection(("127.0.0.1", a.port), timeout=0.25).close()
        raise RuntimeError(f"port {a.port} is in use")
    except OSError:
        pass
    log = open(os.path.join(os.path.dirname(os.path.abspath(a.out)), "a6-server.log"), "ab")
    proc = subprocess.Popen(
        [*prefix, a.bin, "--result-cache-mb", "0", "serve", "--data", data,
         "--loc", f"bench={a.db}", "--port", str(a.port), "--timeout", "600", "--read-only"],
        stdout=log, stderr=log, start_new_session=True)
    for _ in range(1200):
        if proc.poll() is not None:
            raise RuntimeError("server exited")
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{a.port}/$/ping", timeout=1).close()
            return proc, data
        except Exception:
            time.sleep(0.05)
    raise RuntimeError("server did not start")


def stop(proc, data):
    os.killpg(proc.pid, 15)
    try:
        proc.wait(timeout=30)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, 9)
        proc.wait()
    subprocess.run(["rm", "-rf", data])


def sample(a, mode, people, prefix=()):
    evict(a.db)
    proc, data = start(a, prefix)
    ready = time.time()
    try:
        results = {}

        def one(key, q):
            results[key] = query(a.port, q)

        threads = []
        if mode == "with-export":
            t = threading.Thread(target=one, args=("export", P + EXPORT))
            t.start()
            threads.append(t)
            time.sleep(a.delay_ms / 1000)
        for i in people:
            t = threading.Thread(target=one, args=(i, lookup(i)))
            threads.append(t)
        for t in threads[1 if mode == "with-export" else 0:]:
            t.start()
        for t in threads:
            t.join()
    finally:
        stop(proc, data)
    return ready, results


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin", required=True)
    ap.add_argument("--db", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--samples", type=int, default=7)
    ap.add_argument("--port", type=int, default=5671)
    ap.add_argument("--delay-ms", type=float, default=20)
    ap.add_argument("--people", type=int, default=100000, help="people in the dataset")
    ap.add_argument("--strace", default="")
    a = ap.parse_args()
    out = {"bin": a.bin, "db": a.db, "delay_ms": a.delay_ms, "samples": []}
    for s in range(a.samples):
        for mode in (("alone", "with-export") if s % 2 == 0 else ("with-export", "alone")):
            # 16 people spread over the id range, shifted per sample
            people = [(s * 7919 + k * (a.people // 16)) % a.people for k in range(16)]
            _, r = sample(a, mode, people)
            lk = [r[i][0] for i in people]
            row = {"sample": s, "mode": mode, "lookups_ms": lk,
                   "lookup_median_ms": statistics.median(lk),
                   "digests": {str(i): r[i][1] for i in people}}
            if "export" in r:
                row["export_ms"], row["export_digest"] = r["export"]
            out["samples"].append(row)
            print(mode, s, f"median {row['lookup_median_ms']:.2f} ms",
                  f"export {row.get('export_ms', 0):.1f} ms", flush=True)
    for mode in ("alone", "with-export"):
        meds = [x["lookup_median_ms"] for x in out["samples"] if x["mode"] == mode]
        allk = [v for x in out["samples"] if x["mode"] == mode for v in x["lookups_ms"]]
        out[mode] = {"median_of_sample_medians": statistics.median(meds),
                     "median_of_all_lookups": statistics.median(allk)}
    out["ratio_with_export_over_alone"] = (out["with-export"]["median_of_all_lookups"]
                                           / out["alone"]["median_of_all_lookups"])
    if a.strace:
        adv = {}
        for mode in ("alone", "with-export"):
            log = os.path.join(os.path.dirname(os.path.abspath(a.out)), f"a6-{mode}.strace")
            ready, _ = sample(a, mode, [k * (a.people // 16) for k in range(16)],
                              prefix=(a.strace, "-f", "-qq", "-ttt", "-e", "trace=madvise", "-o", log))
            counts = {}
            for l in open(log):
                f = l.split(None, 2)
                if len(f) == 3 and float(f[1]) >= ready:
                    m = re.search(r"MADV_[A-Z_]+", f[2])
                    k = m.group(0) if m else "other"
                    counts[k] = counts.get(k, 0) + 1
            adv[mode] = counts
        out["madvise_after_ready"] = adv
    json.dump(out, open(a.out, "w"), indent=1)
    print(json.dumps({k: out[k] for k in ("alone", "with-export", "ratio_with_export_over_alone")}
                     | ({"madvise_after_ready": out["madvise_after_ready"]} if a.strace else {})))


if __name__ == "__main__":
    main()
