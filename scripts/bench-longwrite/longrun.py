#!/usr/bin/env python3
"""The load of the long write runs (scripts/bench-longwrite.sh, docs/specs/X06 section 3.9).

    longrun.py run   --query-url URL --update-url URL --queries DIR --names "q1 q2 …"
                     --seconds S --out FILE [options]
    longrun.py check --query-url URL --state FILE [--field k=v]…
    longrun.py summary FILE…

`run` starts --readers reader processes and --writers writer processes against one server for
--seconds and writes FILE: a series with one entry per minute, the compaction events the
server reported, and the expected final state.

* Readers run the queries of --names (texts in DIR/<name>.rq) in rotation, each reader from
  its own offset, one request at a time with a new connection per request, and record the
  latency, errors and timeouts (--read-timeout).
* Writers commit SPARQL updates of their own triples, `<http://example.org/lw/<w>/<n>>
  <http://example.org/lwValue> "<n>"`, so the expected final state follows from what each
  writer was told. Writer w draws its operations from a generator seeded with --seed and w:
  an INSERT DATA of one new triple or of --batch new triples, or a DELETE DATA of its oldest
  live triple or of its --batch oldest ones, with the weights 35, 35, 15 and 15, and only
  deletes while it holds more than --live-max triples. --write-rate paces all writers together
  (requests per second); without it each writer sends its next request when the last one is
  answered. A request that fails leaves its triples uncertain: the state check accepts them
  present or absent.
* A monitor samples every --sample seconds the server's RSS (the process listening on
  --port), the disk use of --store, the write requests and flushes completed by the block
  device that holds --store (/proc/diskstats), and with --compaction-url the `compaction` object of Sparkles'
  `/$/stats/<dataset>`, whose `last` entry gives each finished compaction's start, kind,
  duration and writer lock time.

Each minute of the series has the committed write requests and triples, write errors, write
p50 and p99, reads, read errors and timeouts, read p50 and p99, the last and largest RSS, the
disk use, the device's completed writes and flushes, and whether a compaction ran. The
device counters tell the durability classes apart: an engine that synchronizes each commit
makes at least one device write per commit (a journal commit, which the kernel counts as a
write with a flush flag, not always as a flush), and one that leaves its log in the page
cache makes far fewer.

`check` reads every `lwValue` triple from the server and compares it with the expected state
in FILE. It prints a JSON object with the expected, found, missing and extra counts and `ok`.

`summary` prints a Markdown table of one or more run files, one row per engine.
"""

import argparse
import collections
import http.client
import json
import multiprocessing as mp
import os
import queue
import random
import signal
import statistics
import subprocess
import sys
import time
import urllib.parse

EX = "http://example.org/"
PRED = f"<{EX}lwValue>"
WEIGHTS = (("ins1", 35), ("insN", 35), ("del1", 15), ("delN", 15))


def triple(w, n):
    return f'<{EX}lw/{w}/{n}> {PRED} "{n}"'


def endpoint(url):
    u = urllib.parse.urlsplit(url if "://" in url else "http://" + url)
    return u.hostname, u.port or 80, (u.path or "/") + (("?" + u.query) if u.query else "")


def post(url, fields, timeout, accept=None):
    """(ok, timed_out, body): one form POST on a new connection, as the rest of the harness
    sends requests (curl; QLever delays answers on a reused connection)."""
    host, port, path = endpoint(url)
    body = urllib.parse.urlencode(fields)
    headers = {"Content-Type": "application/x-www-form-urlencoded", "Connection": "close"}
    if accept:
        headers["Accept"] = accept
    conn = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        conn.request("POST", path, body, headers)
        r = conn.getresponse()
        data = r.read()
        return 200 <= r.status < 300, False, data
    except TimeoutError:
        return False, True, b""
    except (http.client.HTTPException, OSError):
        return False, False, b""
    finally:
        conn.close()


def get_json(url, timeout=10):
    host, port, path = endpoint(url)
    conn = http.client.HTTPConnection(host, port, timeout=timeout)
    try:
        conn.request("GET", path, headers={"Accept": "application/json", "Connection": "close"})
        r = conn.getresponse()
        data = r.read()
        return json.loads(data) if r.status == 200 else None
    except (http.client.HTTPException, OSError, ValueError):
        return None
    finally:
        conn.close()


# ----------------------------------------------------------------------------- workers
class Bucket:
    """Per-minute measurements of one worker, sent to the main process at each minute's end."""

    def __init__(self, q, kind, t0):
        self.q, self.kind, self.t0 = q, kind, t0
        self.minute = 0
        self.reset()

    def reset(self):
        self.lat, self.errors, self.timeouts, self.triples = [], 0, 0, 0

    def flush(self):
        self.q.put((self.kind, self.minute, self.lat, self.errors, self.timeouts, self.triples))
        self.reset()

    def at(self, t):
        m = int((t - self.t0) // 60)
        if m != self.minute:
            self.flush()
            self.minute = m


def reader(i, a, t0, q, stop):
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    time.sleep(max(0.0, t0 - time.time()))  # every worker starts at t0
    texts = []
    for n in a.names.split():
        with open(os.path.join(a.queries, n + ".rq"), encoding="utf-8") as f:
            texts.append(f.read())
    b = Bucket(q, "read", t0)
    k = i
    end = t0 + a.seconds
    while not stop.is_set():
        now = time.time()
        if now >= end:
            break
        b.at(now)
        s = time.perf_counter()
        ok, to, _ = post(a.query_url, [("query", texts[k % len(texts)])] + a.fields, a.read_timeout,
                         "text/tab-separated-values")
        took = time.perf_counter() - s
        if ok:
            b.lat.append(took * 1000)
        elif to:
            b.timeouts += 1
        else:
            b.errors += 1
        k += 1
    b.flush()


def writer(w, a, t0, q, stop, state_path):
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    time.sleep(max(0.0, t0 - time.time()))  # every worker starts at t0
    rnd = random.Random(a.seed * 1000 + w)
    live = collections.deque()
    uncertain = []
    n = 0
    b = Bucket(q, "write", t0)
    rate = a.write_rate / a.writers if a.write_rate else None
    end = t0 + a.seconds
    sent = 0
    kinds, weights = zip(*WEIGHTS)
    while not stop.is_set():
        now = time.time()
        if now >= end:
            break
        if rate:
            wait = t0 + sent / rate - now
            if wait > 0:
                time.sleep(min(wait, end - now))
                continue
        b.at(now)
        kind = rnd.choices(kinds, weights)[0]
        if len(live) > a.live_max:
            kind = "delN" if kind in ("insN", "delN") else "del1"
        size = 1 if kind in ("ins1", "del1") else a.batch
        if kind.startswith("del") and len(live) < size:
            kind = "ins1" if size == 1 else "insN"
        if kind.startswith("ins"):
            ids = list(range(n, n + size))
            n += size
            update = "INSERT DATA { " + " . ".join(triple(w, x) for x in ids) + " . }"
        else:
            ids = [live.popleft() for _ in range(size)]
            update = "DELETE DATA { " + " . ".join(triple(w, x) for x in ids) + " . }"
        s = time.perf_counter()
        ok, to, _ = post(a.update_url, [("update", update)] + a.fields, a.write_timeout)
        took = time.perf_counter() - s
        sent += 1
        if ok:
            b.lat.append(took * 1000)
            b.triples += size
            if kind.startswith("ins"):
                live.extend(ids)
        else:
            if to:
                b.timeouts += 1
            else:
                b.errors += 1
            uncertain.extend(ids)
    b.flush()
    with open(state_path, "w") as f:
        json.dump({"writer": w, "live": list(live), "uncertain": uncertain, "next": n}, f)


# ----------------------------------------------------------------------------- monitor
def pid_on(port):
    try:
        out = subprocess.run(["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for part in out.split("pid=")[1:]:
        p = part.split(",", 1)[0]
        if p.isdigit():
            return int(p)
    return None


def rss_mib(pid):
    """the RSS of the process and of its children (Fuseki's wrapper script starts the JVM)"""
    if not pid:
        return None
    total, todo, seen = 0, [pid], set()
    while todo:
        p = todo.pop()
        if p in seen:
            continue
        seen.add(p)
        try:
            for line in open(f"/proc/{p}/status"):
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/children") as f:
                    todo += [int(c) for c in f.read().split()]
        except OSError:
            continue
    return round(total / 1024, 1)


def disk_mib(path):
    try:
        out = subprocess.run(["du", "-sb", path], capture_output=True, text=True, timeout=120).stdout
        return round(int(out.split()[0]) / 2**20, 1)
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return None


def block_device(path):
    """the kernel name (dm-0, nvme0n1p2) of the device that holds path"""
    try:
        src = subprocess.run(["findmnt", "-T", path, "-no", "SOURCE"], capture_output=True, text=True,
                             timeout=10).stdout.strip()
        return os.path.basename(os.path.realpath(src)) if src.startswith("/dev/") else None
    except (OSError, subprocess.SubprocessError):
        return None


def devstats(dev):
    """(writes, flushes) completed by the device: fields 5 and 15 of /proc/diskstats (the
    flush count needs Linux 5.5 or later)"""
    if not dev:
        return None
    try:
        for line in open("/proc/diskstats"):
            f = line.split()
            if len(f) >= 18 and f[2] == dev:
                return int(f[7]), int(f[17])
    except (OSError, ValueError):
        pass
    return None


def pct(xs, q):
    if not xs:
        return None
    s = sorted(xs)
    return round(s[min(len(s) - 1, int(q * len(s)))], 3)


def cmd_run(a):
    a.fields = [tuple(f.split("=", 1)) for f in a.field]
    os.makedirs(os.path.dirname(os.path.abspath(a.out)) or ".", exist_ok=True)
    stop = mp.Event()
    q = mp.Queue()
    t0 = time.time() + 3  # time for the workers to start
    states = [f"{a.out}.writer-{w}.json" for w in range(a.writers)]
    procs = [mp.Process(target=reader, args=(i, a, t0, q, stop)) for i in range(a.readers)]
    procs += [mp.Process(target=writer, args=(w, a, t0, q, stop, states[w])) for w in range(a.writers)]
    for p in procs:
        p.start()
    minutes = collections.defaultdict(lambda: {"read": [[], 0, 0, 0], "write": [[], 0, 0, 0], "samples": []})
    events, seen_last = [], None
    dev = block_device(a.store) if a.store else None
    f0 = devstats(dev)
    first_compaction = last_compaction = None

    def drain(block=False):
        while True:
            try:
                kind, m, lat, err, to, tr = q.get(timeout=0.5) if block else q.get_nowait()
            except queue.Empty:
                return
            x = minutes[m][kind]
            x[0].extend(lat)
            x[1] += err
            x[2] += to
            x[3] += tr

    try:
        time.sleep(max(0, t0 - time.time()))
        while time.time() < t0 + a.seconds:
            now = time.time()
            m = int((now - t0) // 60)
            pid = pid_on(a.port) if a.port else None
            s = {"t": round(now - t0, 1), "rss_mib": rss_mib(pid), "dev": devstats(dev)}
            if a.store and (len(minutes[m]["samples"]) == 0 or a.disk_every_sample):
                s["disk_mib"] = disk_mib(a.store)
            if a.compaction_url:
                st = get_json(a.compaction_url)
                c = (st or {}).get("compaction") or {}
                s["compaction"] = c.get("state")
                last = c.get("last") or {}
                if st is not None and first_compaction is None:
                    # what the server held before the run is not an event of the run
                    first_compaction = c
                    seen_last = last.get("startedAt")
                elif last and last.get("startedAt") != seen_last:
                    seen_last = last.get("startedAt")
                    events.append({"seen_at_s": s["t"], **last})
                if st is not None:
                    last_compaction = c
            minutes[m]["samples"].append(s)
            drain()
            time.sleep(max(0.0, a.sample - (time.time() - now)))
    except KeyboardInterrupt:
        stop.set()
    # A worker exits only once the queue has taken everything it put, so the queue is
    # drained while the workers finish their last request.
    give_up = time.time() + max(a.read_timeout, a.write_timeout) + 30
    while any(p.is_alive() for p in procs) and time.time() < give_up:
        drain(block=True)
    for p in procs:
        if p.is_alive():
            p.terminate()
        p.join(timeout=5)
    drain()
    f_end = devstats(dev)
    series = []
    prev_dev = f0
    for m in sorted(minutes):
        x = minutes[m]
        r, w, smp = x["read"], x["write"], x["samples"]
        rss = [s["rss_mib"] for s in smp if s.get("rss_mib") is not None]
        dv = [s["dev"] for s in smp if s.get("dev") is not None]
        disk = [s["disk_mib"] for s in smp if s.get("disk_mib") is not None]
        series.append({
            "minute": m,
            "writes": len(w[0]), "triples_written": w[3], "write_errors": w[1], "write_timeouts": w[2],
            "write_p50_ms": pct(w[0], 0.5), "write_p99_ms": pct(w[0], 0.99),
            "reads": len(r[0]), "read_errors": r[1], "read_timeouts": r[2],
            "read_p50_ms": pct(r[0], 0.5), "read_p99_ms": pct(r[0], 0.99),
            "rss_mib": rss[-1] if rss else None, "rss_max_mib": max(rss) if rss else None,
            "disk_mib": disk[-1] if disk else None,
            "device_writes": (dv[-1][0] - prev_dev[0]) if dv and prev_dev else None,
            "device_flushes": (dv[-1][1] - prev_dev[1]) if dv and prev_dev else None,
            "compacting": any(s.get("compaction") == "running" for s in smp),
        })
        if dv:
            prev_dev = dv[-1]
    # the final partial minute after the deadline holds only the stragglers
    series = [s for s in series if s["minute"] < max(1, a.seconds // 60) or s["writes"] or s["reads"]]
    live, uncertain = [], []
    for w, path in enumerate(states):
        try:
            st = json.load(open(path))
        except (OSError, ValueError):
            sys.exit(f"writer {w} left no state in {path}")
        live += [[w, x] for x in st["live"]]
        uncertain += [[w, x] for x in st["uncertain"]]
        os.remove(path)
    all_w = [s for s in series]
    out = {
        "settings": {k: getattr(a, k) for k in ("seconds", "readers", "writers", "seed", "batch", "live_max",
                                                "write_rate", "read_timeout", "write_timeout", "sample", "names")},
        "device": dev,
        "device_writes": (f_end[0] - f0[0]) if f0 and f_end else None,
        "device_flushes": (f_end[1] - f0[1]) if f0 and f_end else None,
        "totals": {
            "writes": sum(s["writes"] for s in all_w), "triples_written": sum(s["triples_written"] for s in all_w),
            "write_errors": sum(s["write_errors"] + s["write_timeouts"] for s in all_w),
            "reads": sum(s["reads"] for s in all_w),
            "read_errors": sum(s["read_errors"] for s in all_w), "read_timeouts": sum(s["read_timeouts"] for s in all_w),
        },
        "series": series,
        # the finished compactions seen between two samples; two that finish within one
        # sample interval show as one, and the counters below count both
        "compactions": events,
        "compaction_counters": None if first_compaction is None else {
            k: (last_compaction or {}).get(k, 0) - first_compaction.get(k, 0) for k in ("automaticRuns", "failures")},
        "state": {"live": live, "uncertain": uncertain},
    }
    with open(a.out, "w") as f:
        json.dump(out, f)
    t = out["totals"]
    print(json.dumps({"writes": t["writes"], "triples": t["triples_written"], "reads": t["reads"],
                      "write_errors": t["write_errors"], "read_errors": t["read_errors"],
                      "compactions": len(events), "device_writes": out["device_writes"],
                      "device_flushes": out["device_flushes"]}))


def cmd_check(a):
    fields = [tuple(f.split("=", 1)) for f in a.field]
    st = json.load(open(a.state))["state"]
    want = {tuple(x) for x in st["live"]}
    maybe = {tuple(x) for x in st["uncertain"]}
    q = f"SELECT ?s WHERE {{ ?s {PRED} ?o }}"
    ok, to, body = post(a.query_url, [("query", q)] + fields, a.timeout, "text/tab-separated-values")
    if not ok:
        print(json.dumps({"ok": False, "error": "timeout" if to else "request failed"}))
        sys.exit(1)
    found, other = set(), 0
    prefix = f"<{EX}lw/"
    for line in body.decode("utf-8").splitlines()[1:]:
        s = line.strip()
        if s.startswith(prefix) and s.endswith(">"):
            w, _, n = s[len(prefix):-1].partition("/")
            found.add((int(w), int(n)))
        elif s:
            other += 1
    missing = want - found - maybe
    extra = found - want - maybe
    r = {"ok": not missing and not extra and not other, "expected": len(want), "found": len(found),
         "uncertain": len(maybe), "missing": len(missing), "extra": len(extra) + other}
    print(json.dumps(r))
    sys.exit(0 if r["ok"] else 1)


def cmd_summary(a):
    print("| Engine | Durability | Writes/s | Triples/s | Write p50 / p99 (ms) | Reads/s | Read p50 / p99 (ms) "
          "| Errors (w / r) | RSS end / max (MiB) | Disk start / end (MiB) | Device writes / flushes per write "
          "| Compactions | State | After reopen |")
    print("|---|---|---:|---:|---|---:|---|---|---|---|---:|---:|---|---|")
    for path in a.files:
        d = json.load(open(path))
        for e, v in d.items():
            s, t = v["series"], v["totals"]
            secs = v["settings"]["seconds"]
            med = lambda k: statistics.median([x[k] for x in s if x.get(k) is not None] or [0])
            rss = [x["rss_mib"] for x in s if x.get("rss_mib") is not None]
            rmax = [x["rss_max_mib"] for x in s if x.get("rss_max_mib") is not None]
            disk = [x["disk_mib"] for x in s if x.get("disk_mib") is not None]
            per = lambda k: f"{v[k] / t['writes']:.2f}" if v.get(k) is not None and t["writes"] else "?"
            fpw = f"{per('device_writes')} / {per('device_flushes')}"
            chk = lambda c: "ok" if c and c.get("ok") else ("failed" if c else "")
            runs = (v.get("compaction_counters") or {}).get("automaticRuns", len(v.get("compactions", [])))
            print(f"| {e} | {v.get('durability') or ''} | {t['writes'] / secs:.1f} | {t['triples_written'] / secs:.1f} "
                  f"| {med('write_p50_ms'):.1f} / {med('write_p99_ms'):.1f} | {t['reads'] / secs:.1f} "
                  f"| {med('read_p50_ms'):.1f} / {med('read_p99_ms'):.1f} | {t['write_errors']} / "
                  f"{t['read_errors'] + t['read_timeouts']} | {rss[-1] if rss else ''} / {max(rmax) if rmax else ''} "
                  f"| {disk[0] if disk else ''} / {disk[-1] if disk else ''} | {fpw} | {runs if v.get('compaction_counters') else ''} "
                  f"| {chk(v.get('check'))} | {chk(v.get('check_reopen'))} |")
    print("\nThe write and read percentiles are the medians of the per-minute values. The device columns are the "
          "write requests and flushes the store's block device completed during the run, divided by the committed "
          "write requests. Other processes' I/O on the same device counts too.")


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--query-url", required=True)
    r.add_argument("--update-url", required=True)
    r.add_argument("--field", action="append", default=[], help="k=v added to every request (QLever's token)")
    r.add_argument("--queries", required=True, help="directory of <name>.rq")
    r.add_argument("--names", required=True, help="the read queries, space separated")
    r.add_argument("--seconds", type=int, required=True)
    r.add_argument("--readers", type=int, default=16)
    r.add_argument("--writers", type=int, default=1)
    r.add_argument("--seed", type=int, default=20261018)
    r.add_argument("--batch", type=int, default=100)
    r.add_argument("--live-max", type=int, default=200000, help="a writer only deletes above this many live triples")
    r.add_argument("--write-rate", type=float, default=0.0, help="write requests per second, all writers together")
    r.add_argument("--read-timeout", type=float, default=60.0)
    r.add_argument("--write-timeout", type=float, default=120.0)
    r.add_argument("--sample", type=float, default=5.0, help="seconds between monitor samples")
    r.add_argument("--disk-every-sample", action="store_true", help="du at every sample, not once a minute")
    r.add_argument("--port", type=int, default=0, help="the server's port, for its RSS")
    r.add_argument("--store", default="", help="the server's store, for disk use and device counters")
    r.add_argument("--compaction-url", default="", help="Sparkles' /$/stats/<dataset>")
    r.add_argument("--out", required=True)
    r.set_defaults(f=cmd_run)
    c = sub.add_parser("check")
    c.add_argument("--query-url", required=True)
    c.add_argument("--state", required=True, help="the file of `run`")
    c.add_argument("--field", action="append", default=[])
    c.add_argument("--timeout", type=float, default=600.0)
    c.set_defaults(f=cmd_check)
    s = sub.add_parser("summary")
    s.add_argument("files", nargs="+", help="longwrite.json files: {engine: run file contents with checks}")
    s.set_defaults(f=cmd_summary)
    a = p.parse_args()
    a.f(a)


if __name__ == "__main__":
    main()
