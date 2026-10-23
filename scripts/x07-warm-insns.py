#!/usr/bin/env python3
"""Warm instruction counts and warm read hints per query, for the X07 warm gate.

The X07 spec (Remaining measurements) defines the gate: start
`sparkles --result-cache-mb 0 serve --timeout 600` pinned to one core, send 200 warm-up
requests per query, then count `instructions:u` with `perf stat` over 2,000 further
requests and divide. A separate warm pass of every query under `strace` counts the
`madvise` calls, by advice.

    x07-warm-insns.py --bin target/release/sparkles --store sparkles.db \
        --queries queries/ --out results/A [--label A]

The script writes one JSON object per query to `<out>/insns.jsonl`, the strace log and a
`madvise.json` summary, and the SHA-256 of every response body, so two builds can be
checked for identical answers. It needs `perf` and `strace` on PATH and
`kernel.perf_event_paranoid` at 2 or less. It starts and stops its own server and leaves
no process behind.
"""

import argparse
import hashlib
import http.client
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import urllib.parse


def request(conn, path):
    conn.request("GET", path, headers={"Accept": "application/sparql-results+json"})
    r = conn.getresponse()
    body = r.read()
    if r.status != 200:
        raise RuntimeError(f"HTTP {r.status}: {body[:300]!r}")
    return body


def drive(port, path, n):
    """Send n sequential requests on one keep-alive connection, return the body hashes."""
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
    hashes = set()
    for _ in range(n):
        hashes.add(hashlib.sha256(request(conn, path)).hexdigest())
    conn.close()
    return hashes


def client_main(args):
    hashes = drive(args.port, args.path, args.n)
    print(json.dumps(sorted(hashes)))


def wait_ready(port, proc, timeout=300):
    t0 = time.time()
    while time.time() - t0 < timeout:
        if proc.poll() is not None:
            raise RuntimeError(f"server exited with {proc.returncode}")
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            c.request("GET", "/$/ping")
            if c.getresponse().status == 200:
                return
        except OSError:
            pass
        time.sleep(0.2)
    raise RuntimeError("server did not become ready")


def perf_counts(path):
    """Sum each event over the hybrid PMUs from perf's CSV output."""
    out = {}
    for line in open(path):
        f = line.strip().split(",")
        if len(f) < 3 or not f[0].replace(".", "").isdigit():
            continue
        ev = re.sub(r"^cpu_(core|atom)/|/u?$|:u$", "", f[2])
        out[ev] = out.get(ev, 0) + int(float(f[0]))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--bin")
    ap.add_argument("--store")
    ap.add_argument("--queries", help="a directory of NAME.rq files")
    ap.add_argument("--names", default="", help="space-separated subset, in order")
    ap.add_argument("--out")
    ap.add_argument("--label", default="")
    ap.add_argument("--port", type=int, default=7461)
    ap.add_argument("--server-cpu", default="4")
    ap.add_argument("--client-cpu", default="8")
    ap.add_argument("--warmup", type=int, default=200)
    ap.add_argument("--requests", type=int, default=2000)
    ap.add_argument("--perf", default="perf")
    ap.add_argument("--strace", default="strace")
    ap.add_argument("--skip-madvise", action="store_true")
    ap.add_argument("--client", action="store_true", help=argparse.SUPPRESS)
    ap.add_argument("--path", help=argparse.SUPPRESS)
    ap.add_argument("--n", type=int, help=argparse.SUPPRESS)
    args = ap.parse_args()
    if args.client:
        return client_main(args)

    os.makedirs(args.out, exist_ok=True)
    names = args.names.split() or sorted(
        f[:-3] for f in os.listdir(args.queries) if f.endswith(".rq") and not f.startswith("_")
    )
    paths = {}
    for n in names:
        q = open(os.path.join(args.queries, n + ".rq")).read()
        paths[n] = "/bench/sparql?" + urllib.parse.urlencode({"query": q})

    data = tempfile.mkdtemp(prefix="x07-warm-server-")
    log = open(os.path.join(args.out, "server.log"), "w")
    server = subprocess.Popen(
        ["taskset", "-c", args.server_cpu, args.bin, "--result-cache-mb", "0", "serve",
         "--data", data, "--loc", f"bench={args.store}", "--port", str(args.port),
         "--timeout", "600"],
        stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
    )
    me = os.path.abspath(__file__)
    client = ["taskset", "-c", args.client_cpu, sys.executable, me, "--client",
              "--port", str(args.port)]
    try:
        wait_ready(args.port, server)
        with open(os.path.join(args.out, "insns.jsonl"), "w") as rows:
            for n in names:
                warm = drive(args.port, paths[n], args.warmup)
                pf = os.path.join(args.out, f"perf-{n}.csv")
                cmd = [args.perf, "stat", "-x,", "-o", pf, "-e", "instructions:u,cycles:u",
                       "-p", str(server.pid), "--"] + client + [
                           "--path", paths[n], "--n", str(args.requests)]
                t0 = time.time()
                res = subprocess.run(cmd, capture_output=True, text=True, check=True)
                wall = time.time() - t0
                hashes = set(json.loads(res.stdout)) | warm
                c = perf_counts(pf)
                row = {
                    "label": args.label, "query": n, "requests": args.requests,
                    "instructions": c.get("instructions"), "cycles": c.get("cycles"),
                    "insns_per_req": c.get("instructions", 0) / args.requests,
                    "cycles_per_req": c.get("cycles", 0) / args.requests,
                    "wall_s": round(wall, 3), "answers": sorted(hashes),
                }
                rows.write(json.dumps(row) + "\n")
                rows.flush()
                print(f"{args.label} {n}: {row['insns_per_req']:.0f} insns/req,"
                      f" {len(hashes)} distinct answer(s)", flush=True)
        if not args.skip_madvise:
            # one warm pass of every query under strace, after the measured runs
            sl = os.path.join(args.out, "madvise.strace")
            st = subprocess.Popen([args.strace, "-f", "-qq", "-e", "trace=madvise", "-o", sl,
                                   "-p", str(server.pid)], start_new_session=True)
            time.sleep(1.0)
            marks = {}
            for n in names:
                before = sum(1 for _ in open(sl)) if os.path.exists(sl) else 0
                drive(args.port, paths[n], 1)
                time.sleep(0.05)
                marks[n] = (before, sum(1 for _ in open(sl)))
            time.sleep(0.5)
            st.send_signal(signal.SIGINT)
            st.wait(timeout=30)
            lines = open(sl).read().splitlines()
            per = {}
            for n, (a, b) in marks.items():
                adv = {}
                for l in lines[a:b]:
                    m = re.search(r"MADV_[A-Z_]+", l)
                    k = m.group(0) if m else "other"
                    adv[k] = adv.get(k, 0) + 1
                per[n] = adv
            total = {}
            for l in lines:
                m = re.search(r"MADV_[A-Z_]+", l)
                k = m.group(0) if m else "other"
                total[k] = total.get(k, 0) + 1
            json.dump({"per_query": per, "total": total, "lines": len(lines)},
                      open(os.path.join(args.out, "madvise.json"), "w"), indent=1)
            print(f"{args.label} madvise over one warm pass: {total}", flush=True)
    finally:
        os.killpg(server.pid, signal.SIGTERM)
        try:
            server.wait(timeout=60)
        except subprocess.TimeoutExpired:
            os.killpg(server.pid, signal.SIGKILL)
            server.wait()
        subprocess.run(["rm", "-rf", data])


if __name__ == "__main__":
    main()
