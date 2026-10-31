#!/usr/bin/env python3
"""Interleaved A/B of `sparkles load` on files: wall seconds, peak RSS and peak
temporary-directory use. usage: run.py ROUNDS BASE_BIN NEW_BIN FILE..."""
import os, resource, shutil, subprocess, sys, threading, time, statistics

rounds, base, new, *files = sys.argv[1:]
rounds = int(rounds)
work = os.path.abspath("work")

def du(p):
    t = 0
    for root, _, fs in os.walk(p):
        for f in fs:
            try: t += os.path.getsize(os.path.join(root, f))
            except OSError: pass
    return t

def once(binary, f):
    shutil.rmtree(work, ignore_errors=True)
    os.makedirs(work + "/tmp")
    stop = threading.Event(); peak = [0]
    def sample():
        while not stop.is_set():
            peak[0] = max(peak[0], du(work + "/tmp")); time.sleep(0.02)
    th = threading.Thread(target=sample); th.start()
    t = time.time()
    p = subprocess.Popen([binary, "load", "--loc", work + "/db", f], env=dict(os.environ, TMPDIR=work + "/tmp"),
                         stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
    err = p.stderr.read()
    _, status, usage = os.wait4(p.pid, 0)
    p.returncode = 0 if status == 0 else 1
    secs = time.time() - t
    stop.set(); th.join()
    assert status == 0, err
    return secs, peak[0] / 2**20, usage.ru_maxrss / 1024, err.strip().splitlines()[-1]

res = {}
for f in files:
    for r in range(rounds):
        for label, b in (("base", base), ("new", new)):
            secs, tmp, rss, line = once(b, f)
            res.setdefault((f, label), []).append((secs, tmp, rss))
            print(f"{label} {f} round {r+1}: {secs:.2f} s, peak temp {tmp:.0f} MiB, peak RSS {rss:.0f} MiB | {line[:90]}", flush=True)
print()
for f in files:
    for label in ("base", "new"):
        v = res[(f, label)]
        print(f"{f:10} {label:5} median {statistics.median(x[0] for x in v):6.2f} s  min {min(x[0] for x in v):6.2f} s  peak temp {max(x[1] for x in v):6.0f} MiB  peak RSS {max(x[2] for x in v):6.0f} MiB")
