#!/usr/bin/env python3
"""The load of the long write runs (scripts/bench-longwrite.sh, docs/specs/X06 section 3.9).

    longrun.py run   --query-url URL --update-url URL --queries DIR --names "q1 q2 …"
                     --seconds S --out FILE [options]
    longrun.py check --query-url URL --state FILE [--field k=v]…
    longrun.py merge --into longwrite.json --engine E --run FILE [--check JSON]…
    longrun.py summary FILE…

`run` starts --readers reader processes and --writers writer processes against one server for
--seconds and writes FILE: a series with one entry per minute, the compaction events the
server reported, and the expected final state.

* Readers run the queries of --names (texts in DIR/<name>.rq) in rotation, each reader from
  its own offset, one request at a time with a new connection per request, and record the
  latency, errors and timeouts (--read-timeout). An answer without a header line counts as
  an error. The harness checks the answers of the read set before the run.
* Writers commit SPARQL updates of their own triples, `<http://example.org/lw/<w>/<n>>
  <http://example.org/lwValue> "<n>"`, so the expected final state follows from what each
  writer was told. Writer w draws its operations from a generator seeded with --seed and w:
  an INSERT DATA of one new triple or of --batch new triples, or a DELETE DATA of its oldest
  live triple or of its --batch oldest ones, with the weights 35, 35, 15 and 15, and only
  deletes while it holds more than --live-max triples. --write-rate paces all writers together
  (requests per second); without it each writer sends its next request when the last one is
  answered. A request that fails leaves its triples uncertain: the state check accepts them
  present or absent. A writer keeps at most --uncertain-max uncertain ids and only counts the
  rest, and a state with uncounted ids makes the check indeterminate.
* After a failed request a worker waits before the next one, 10 ms doubling up to 1 s while
  the failures continue, so a server that is down or refuses every request is not flooded.
  When the process listening on --port is gone for three samples in a row, the run stops
  early and records why.
* A monitor samples every --sample seconds the server's RSS (the process listening on
  --port and its children), the write requests completed by the block device that holds
  --store, the flushes completed by the whole disks under it (the kernel counts flushes only
  there), and with --compaction-url the `compaction` object of Sparkles' `/$/stats/<dataset>`,
  whose `last` entry gives each finished compaction's start, kind, duration and writer lock
  time. A separate thread measures the allocated bytes of --store once a minute.

The workers are forked children that receive SIGKILL when the coordinator dies
(PR_SET_PDEATHSIG) and also stop when their parent changes. SIGTERM to the coordinator ends
the run as Ctrl-C does.

Each minute of the series has the committed write requests and triples, write errors, write
p50 and p99, reads, read errors and timeouts, read p50 and p99, the last and largest RSS, the
disk use, the device writes and disk flushes, and whether a compaction ran. Latencies are
those of successful requests; failures are counted beside them. An engine that synchronizes
each commit makes at least one disk flush per commit or per group of commits, and one that
leaves its log in the page cache makes almost none.

`check` reads every `lwValue` triple from the server as SPARQL JSON results and compares
subjects and objects with the expected state in FILE. It prints a JSON object with the
expected, found, missing and extra counts and `ok`, and `indeterminate` when the state lost
track of some uncertain ids.

`merge` adds one engine's run and its checks to longwrite.json with a status (see
`run_status`). `summary` prints a Markdown table of one or more longwrite.json files, one row per engine.
"""

import argparse
import collections
import ctypes
import http.client
import json
import multiprocessing as mp
import os
import queue
import random
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
import urllib.parse

EX = "http://example.org/"
PRED = f"<{EX}lwValue>"
WEIGHTS = (("ins1", 35), ("insN", 35), ("del1", 15), ("delN", 15))
CTX = mp.get_context("fork")


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
    except (TimeoutError, socket.timeout):
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


class Backoff:
    """Waits after consecutive failures: 10 ms, doubling, at most 1 s."""

    def __init__(self):
        self.fails = 0

    def ok(self):
        self.fails = 0

    def failed(self):
        self.fails += 1
        time.sleep(min(1.0, 0.01 * 2 ** min(self.fails - 1, 7)))


def worker_start(parent, t0):
    """Ties a forked worker's life to the coordinator's."""
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_DFL)
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        libc.prctl(1, signal.SIGKILL, 0, 0, 0)  # PR_SET_PDEATHSIG
    except (OSError, AttributeError):
        pass
    if os.getppid() != parent:
        os._exit(1)
    time.sleep(max(0.0, t0 - time.time()))  # every worker starts at t0


def reader(i, a, t0, q, stop, parent):
    worker_start(parent, t0)
    texts = []
    for n in a.names.split():
        with open(os.path.join(a.queries, n + ".rq"), encoding="utf-8") as f:
            texts.append(f.read())
    b = Bucket(q, "read", t0)
    back = Backoff()
    k = i
    end = t0 + a.seconds
    while not stop.is_set() and os.getppid() == parent:
        now = time.time()
        if now >= end:
            break
        b.at(now)
        s = time.perf_counter()
        ok, to, body = post(a.query_url, [("query", texts[k % len(texts)])] + a.fields, a.read_timeout,
                            "text/tab-separated-values")
        took = time.perf_counter() - s
        k += 1
        if ok and body.strip():
            b.lat.append(took * 1000)
            back.ok()
            continue
        if to:
            b.timeouts += 1
        else:
            b.errors += 1
        back.failed()
    b.flush()


def writer(w, a, t0, q, stop, state_path, parent):
    worker_start(parent, t0)
    rnd = random.Random(a.seed * 1000 + w)
    live = collections.deque()
    uncertain, overflow = [], 0
    n = 0
    b = Bucket(q, "write", t0)
    back = Backoff()
    rate = a.write_rate / a.writers if a.write_rate else None
    end = t0 + a.seconds
    sent = 0
    kinds, weights = zip(*WEIGHTS)
    while not stop.is_set() and os.getppid() == parent:
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
            back.ok()
            continue
        if to:
            b.timeouts += 1
        else:
            b.errors += 1
        if len(uncertain) + len(ids) <= a.uncertain_max:
            uncertain.extend(ids)
        else:
            overflow += len(ids)
        back.failed()
    b.flush()
    with open(state_path, "w") as f:
        json.dump({"writer": w, "live": list(live), "uncertain": uncertain, "uncertain_overflow": overflow,
                   "next": n}, f)


# ----------------------------------------------------------------------------- monitor
def pid_on(port):
    try:
        out = subprocess.run(["ss", "-ltnpH", f"sport = :{port}"], capture_output=True, text=True, timeout=10,
                             check=False).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for part in out.split("pid=")[1:]:
        p = part.split(",", 1)[0]
        if p.isdigit():
            return int(p)
    return None


def listening(port):
    try:
        socket.create_connection(("127.0.0.1", port), timeout=2).close()
        return True
    except OSError:
        return False


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
            with open(f"/proc/{p}/status") as f:
                for line in f:
                    if line.startswith("VmRSS:"):
                        total += int(line.split()[1])
            for t in os.listdir(f"/proc/{p}/task"):
                with open(f"/proc/{p}/task/{t}/children") as f:
                    todo += [int(c) for c in f.read().split()]
        except OSError:
            continue
    return round(total / 1024, 1)


def disk_mib(path):
    """the bytes allocated to path, as bench-lib.sh records index sizes (TDB2's files are sparse)"""
    try:
        out = subprocess.run(["du", "-s", "--block-size=1", path], capture_output=True, text=True, timeout=600,
                             check=False).stdout
        return round(int(out.split()[0]) / 2**20, 1)
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        return None


def block_device(path):
    """the kernel name (dm-0, nvme0n1p2) of the device that holds path, or None when the source
    is not a block device node (overlay, ZFS, a btrfs subvolume)"""
    try:
        src = subprocess.run(["findmnt", "-T", path, "-no", "SOURCE"], capture_output=True, text=True,
                             timeout=10, check=False).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None
    if not src.startswith("/dev/") or "[" in src:
        return None
    name = os.path.basename(os.path.realpath(src))
    return name if os.path.exists(f"/sys/class/block/{name}") else None


def whole_disks(dev, sys_block="/sys/class/block"):
    """the whole disks under a device: device-mapper and md nodes are followed through their
    `slaves`, and a partition gives its parent disk"""
    out, todo, seen = [], [dev], set()
    while todo:
        d = todo.pop()
        if d in seen:
            continue
        seen.add(d)
        base = os.path.join(sys_block, d)
        try:
            slaves = os.listdir(os.path.join(base, "slaves"))
        except OSError:
            slaves = []
        if slaves:
            todo += slaves
        elif os.path.exists(os.path.join(base, "partition")):
            todo.append(os.path.basename(os.path.dirname(os.path.realpath(base))))
        else:
            out.append(d)
    return sorted(out)


def parse_diskstats(text):
    """{device: (writes completed, flushes completed)} from /proc/diskstats. After the split a
    line is major, minor, name and the counters of Documentation/admin-guide/iostats.rst:
    writes completed is field 5 (f[7]) and flush requests completed field 16 (f[18], Linux 5.5
    and later, None before)."""
    d = {}
    for line in text.splitlines():
        f = line.split()
        if len(f) < 14:
            continue
        d[f[2]] = (int(f[7]), int(f[18]) if len(f) >= 19 else None)
    return d


def devstats(dev, disks):
    """(writes of dev, flushes summed over disks), or None"""
    if not dev:
        return None
    try:
        with open("/proc/diskstats") as f:
            st = parse_diskstats(f.read())
    except OSError:
        return None
    if dev not in st:
        return None
    fl = [st[d][1] for d in disks if d in st]
    return st[dev][0], (sum(fl) if fl and None not in fl else None)


class DiskSampler(threading.Thread):
    """du of the store once a minute, off the sampling loop"""

    def __init__(self, path, every=60.0):
        super().__init__(daemon=True)
        self.path, self.every, self.value, self.halt = path, every, None, threading.Event()

    def run(self):
        while not self.halt.is_set():
            self.value = disk_mib(self.path)
            self.halt.wait(self.every)


def pct(xs, q):
    if not xs:
        return None
    s = sorted(xs)
    return round(s[min(len(s) - 1, int(q * len(s)))], 3)


def delta(a, b):
    return None if a is None or b is None else a - b


def cmd_run(a):
    a.fields = [tuple(f.split("=", 1)) for f in a.field]
    os.makedirs(os.path.dirname(os.path.abspath(a.out)) or ".", exist_ok=True)
    stop = CTX.Event()
    q = CTX.Queue()
    dev = block_device(a.store) if a.store else None
    disks = whole_disks(dev) if dev else []
    # The device and its disks carry every process's I/O, so their rate with the server idle
    # is measured first and subtracted from the run's counts (device_*_net).
    baseline = None
    if a.baseline > 0 and dev:
        b0, tb0 = devstats(dev, disks), time.time()
        time.sleep(a.baseline)
        b1, tb1 = devstats(dev, disks), time.time()
        if b0 and b1:
            baseline = {"seconds": round(tb1 - tb0, 1), "device_writes": delta(b1[0], b0[0]),
                        "device_flushes": delta(b1[1], b0[1])}
    t0 = time.time() + 3  # time for the workers to start
    me = os.getpid()
    states = [f"{a.out}.writer-{w}.json" for w in range(a.writers)]
    procs = [CTX.Process(target=reader, args=(i, a, t0, q, stop, me)) for i in range(a.readers)]
    procs += [CTX.Process(target=writer, args=(w, a, t0, q, stop, states[w], me)) for w in range(a.writers)]
    stopped = {"reason": None}

    def on_term(signum, frame):
        stopped["reason"] = stopped["reason"] or f"signal {signum}"
        # only a flag: setting the shared Event here could deadlock with a wait in progress

    signal.signal(signal.SIGTERM, on_term)
    signal.signal(signal.SIGINT, on_term)
    for p in procs:
        p.start()
    minutes = collections.defaultdict(lambda: {"read": [[], 0, 0, 0], "write": [[], 0, 0, 0], "samples": []})
    events, seen_last = [], None
    f0, tf0 = devstats(dev, disks), time.time()
    first_compaction = last_compaction = None
    du = DiskSampler(a.store, a.disk_every) if a.store else None
    if du:
        du.start()
    gone = 0

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

    time.sleep(max(0, t0 - time.time()))
    while time.time() < t0 + a.seconds and not stopped["reason"]:
        now = time.time()
        m = int((now - t0) // 60)
        pid = pid_on(a.port) if a.port else None
        s = {"t": round(now - t0, 1), "rss_mib": rss_mib(pid), "dev": devstats(dev, disks),
             "disk_mib": du.value if du else None}
        if a.port:
            gone = 0 if (pid or listening(a.port)) else gone + 1
            if gone >= 3:
                stopped["reason"] = f"the server on port {a.port} stopped listening"
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
    stop.set()
    # A worker exits only once the queue has taken everything it put, so the queue is
    # drained while the workers finish their last request.
    give_up = time.time() + max(a.read_timeout, a.write_timeout) + 30
    while any(p.is_alive() for p in procs) and time.time() < give_up:
        drain(block=True)
    for p in procs:
        if p.is_alive():
            p.kill()
        p.join(timeout=5)
    drain()
    if du:
        du.halt.set()
    f_end, tf_end = devstats(dev, disks), time.time()

    def net(i):
        # the run's count less the idle rate over the same time, or None
        if not (f0 and f_end and baseline) or delta(f_end[i], f0[i]) is None:
            return None
        idle = baseline["device_writes" if i == 0 else "device_flushes"]
        if idle is None or not baseline["seconds"]:
            return None
        return round(delta(f_end[i], f0[i]) - idle / baseline["seconds"] * (tf_end - tf0), 1)

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
            "device_writes": delta(dv[-1][0], prev_dev[0]) if dv and prev_dev else None,
            "device_flushes": delta(dv[-1][1], prev_dev[1]) if dv and prev_dev else None,
            "compacting": any(s.get("compaction") == "running" for s in smp),
        })
        if dv:
            prev_dev = dv[-1]
    # the final partial minute after the deadline holds only the stragglers
    series = [s for s in series if s["minute"] < max(1, a.seconds // 60) or s["writes"] or s["reads"]]
    live, uncertain, overflow = [], [], 0
    for w, path in enumerate(states):
        try:
            with open(path) as f:
                st = json.load(f)
        except (OSError, ValueError):
            sys.exit(f"writer {w} left no state in {path}")
        live += [[w, x] for x in st["live"]]
        uncertain += [[w, x] for x in st["uncertain"]]
        overflow += st.get("uncertain_overflow", 0)
        os.remove(path)
    out = {
        "settings": {k: getattr(a, k) for k in ("seconds", "readers", "writers", "seed", "batch", "live_max",
                                                "write_rate", "read_timeout", "write_timeout", "sample", "names")},
        "stopped_early": stopped["reason"],
        "device": dev, "flush_disks": disks,
        "device_writes": delta(f_end[0], f0[0]) if f0 and f_end else None,
        "device_flushes": delta(f_end[1], f0[1]) if f0 and f_end else None,
        "device_seconds": round(tf_end - tf0, 1),
        "device_baseline": baseline,
        "device_writes_net": net(0),
        "device_flushes_net": net(1),
        "totals": {
            "writes": sum(s["writes"] for s in series), "triples_written": sum(s["triples_written"] for s in series),
            "write_errors": sum(s["write_errors"] + s["write_timeouts"] for s in series),
            "reads": sum(s["reads"] for s in series),
            "read_errors": sum(s["read_errors"] for s in series),
            "read_timeouts": sum(s["read_timeouts"] for s in series),
        },
        "series": series,
        # the finished compactions seen between two samples; two that finish within one
        # sample interval show as one, and the counters below count both
        "compactions": events,
        "compaction_counters": None if first_compaction is None else {
            k: (last_compaction or {}).get(k, 0) - first_compaction.get(k, 0) for k in ("automaticRuns", "failures")},
        "state": {"live": live, "uncertain": uncertain, "uncertain_overflow": overflow},
    }
    with open(a.out, "w") as f:
        json.dump(out, f)
    t = out["totals"]
    print(json.dumps({"writes": t["writes"], "triples": t["triples_written"], "reads": t["reads"],
                      "write_errors": t["write_errors"], "read_errors": t["read_errors"] + t["read_timeouts"],
                      "compactions": len(events), "device_writes": out["device_writes"],
                      "disk_flushes": out["device_flushes"], "device_writes_net": out["device_writes_net"],
                      "disk_flushes_net": out["device_flushes_net"], "stopped_early": stopped["reason"]}))


def cmd_check(a):
    fields = [tuple(f.split("=", 1)) for f in a.field]
    with open(a.state) as f:
        st = json.load(f)["state"]
    want = {tuple(x) for x in st["live"]}
    maybe = {tuple(x) for x in st["uncertain"]}
    q = f"SELECT ?s ?o WHERE {{ ?s {PRED} ?o }}"
    ok, to, body = post(a.query_url, [("query", q)] + fields, a.timeout, "application/sparql-results+json")
    try:
        rows = json.loads(body)["results"]["bindings"] if ok else None
    except (ValueError, KeyError, TypeError):
        rows = None
    if rows is None:
        print(json.dumps({"ok": False, "error": "timeout" if to else "request failed or not SPARQL JSON results"}))
        sys.exit(1)
    found, other = set(), 0
    prefix = f"{EX}lw/"
    for b in rows:
        s, o = b.get("s", {}), b.get("o", {})
        sv = s.get("value", "")
        w, _, n = sv[len(prefix):].partition("/") if sv.startswith(prefix) else ("", "", "")
        if s.get("type") == "uri" and w.isdigit() and n.isdigit() and o.get("type") in ("literal", "typed-literal") \
                and o.get("value") == n and o.get("datatype") in (None, "http://www.w3.org/2001/XMLSchema#string"):
            found.add((int(w), int(n)))
        else:
            other += 1
    missing = want - found - maybe
    extra = found - want - maybe
    over = st.get("uncertain_overflow", 0)
    r = {"ok": not missing and not extra and not other and not over, "expected": len(want), "found": len(found),
         "uncertain": len(maybe), "missing": len(missing), "extra": len(extra), "malformed": other}
    if over:
        r["indeterminate"] = True
        r["uncertain_overflow"] = over
    print(json.dumps(r))
    sys.exit(0 if r["ok"] else 1)


DURABLE = ("synced", "written")
FAILURE_LIMIT = 0.01


def run_status(v):
    """(status, reasons) of one engine's run: `error` when the state check failed or was
    indeterminate, when no write succeeded, when more than 1% of the write or read requests
    failed, when the run stopped early, or when an engine of the synced or written class lost
    acknowledged writes in a restart, after SIGKILL or after a normal stop. An engine of the
    memory class loses its writes in a restart by design, so its restart checks are only
    reported."""
    why = []
    t = v.get("totals", {})
    c = v.get("check") or {}
    if not c.get("ok"):
        why.append("state check " + check_text(c) if c else "no state check")
    if not t.get("writes"):
        why.append("no write succeeded")
    for kind, fails in (("write", t.get("write_errors", 0)),
                        ("read", t.get("read_errors", 0) + t.get("read_timeouts", 0))):
        done = t.get(kind + "s", 0)
        if fails and fails > FAILURE_LIMIT * (done + fails):
            why.append(f"{fails} of {done + fails} {kind} requests failed")
    if v.get("stopped_early"):
        why.append("stopped early: " + v["stopped_early"])
    if v.get("durability") in DURABLE:
        for k, label in (("check_crash", "after SIGKILL"), ("check_reopen", "after a restart")):
            r = v.get(k)
            if not (r and r.get("ok")):
                why.append(f"state {label} " + (check_text(r) if r else "not checked"))
    return ("error" if why else "ok"), why


def cmd_merge(a):
    """adds one engine's run and checks to longwrite.json, with its status"""
    dur = {"sparkles": "synced", "jena": "synced", "fluree": "synced", "oxigraph": "written", "qlever": "memory"}
    d = {}
    if os.path.exists(a.into):
        with open(a.into) as f:
            d = json.load(f)
    if a.run and os.path.exists(a.run):
        with open(a.run) as f:
            r = json.load(f)
        r.pop("state", None)
    else:
        r = {"settings": {"seconds": 0}, "totals": {}, "series": [], "stopped_early": "the load generator failed"}

    def load(x):
        return json.loads(x) if x and x != "null" else None

    r.update({"durability": dur.get(a.engine), "check": load(a.check), "check_crash": load(a.check_crash),
              "check_reopen": load(a.check_reopen), "restart_s": float(a.restart_s) if a.restart_s else None,
              "crash_restart_s": float(a.crash_restart_s) if a.crash_restart_s else None})
    r["status"], r["status_reasons"] = run_status(r)
    if a.answers in ("wrong-answer", "answer-disagreement", "error"):
        why = a.answers_reason or ("differs from the majority" if a.answers in ("wrong-answer", "answer-disagreement") else "failed a query")
        r["status"], r["status_reasons"] = a.answers, [f"not run: it {why} of the read set"]
    d[a.engine] = r
    with open(a.into, "w") as f:
        json.dump(d, f, indent=1)
    print(f"{a.engine}: {r['status']}" + (": " + "; ".join(r["status_reasons"]) if r["status_reasons"] else ""))


def check_text(c):
    if not c:
        return ""
    if c.get("ok"):
        return "ok"
    if c.get("indeterminate"):
        return "indeterminate"
    if "missing" in c:
        return f"failed ({c['missing']} missing, {c['extra'] + c.get('malformed', 0)} extra)"
    return f"failed ({c.get('error', 'unknown')})"


def opt(x):
    return "" if x is None else x


def cmd_summary(a):
    print("| Engine | Durability | Status | Writes/s | Triples/s | Write p50 / p99 (ms) | Reads/s "
          "| Read p50 / p99 (ms) | Failed requests (w / r) | RSS end / max (MiB) | Disk start / end (MiB) "
          "| Device writes / disk flushes per write, net of idle | Compactions | State | After SIGKILL "
          "| After restart | Recovery after SIGKILL (s) | Restart (s) |")
    print("|---|---|---|---:|---:|---|---:|---|---|---|---|---:|---:|---|---|---|---:|---:|")
    for path in a.files:
        with open(path) as f:
            d = json.load(f)
        for e, v in d.items():
            s, t = v["series"], v["totals"]
            secs = v["settings"]["seconds"]
            warm = [x for x in s if x["minute"] >= 1] or s

            def med(k, rows=warm):
                xs = [x[k] for x in rows if x.get(k) is not None]
                return f"{statistics.median(xs):.1f}" if xs else "?"

            rss = [x["rss_mib"] for x in s if x.get("rss_mib") is not None]
            rmax = [x["rss_max_mib"] for x in s if x.get("rss_max_mib") is not None]
            disk = [x["disk_mib"] for x in s if x.get("disk_mib") is not None]
            def per_write(k):
                return f"{v[k] / t['writes']:.2f}" if v.get(k) is not None and t["writes"] else "?"

            fpw = f"{per_write('device_writes_net')} / {per_write('device_flushes_net')}"
            runs = (v.get("compaction_counters") or {}).get("automaticRuns")
            print(f"| {e} | {v.get('durability') or ''} | {v.get('status', '')} | {t['writes'] / secs:.1f} "
                  f"| {t['triples_written'] / secs:.1f} | {med('write_p50_ms')} / {med('write_p99_ms')} "
                  f"| {t['reads'] / secs:.1f} | {med('read_p50_ms')} / {med('read_p99_ms')} "
                  f"| {t['write_errors']} / {t['read_errors'] + t['read_timeouts']} "
                  f"| {rss[-1] if rss else ''} / {max(rmax) if rmax else ''} "
                  f"| {disk[0] if disk else ''} / {disk[-1] if disk else ''} | {fpw} | {'' if runs is None else runs} "
                  f"| {check_text(v.get('check'))} | {check_text(v.get('check_crash'))} "
                  f"| {check_text(v.get('check_reopen'))} | {opt(v.get('crash_restart_s'))} "
                  f"| {opt(v.get('restart_s'))} |")
    print("\nThe percentiles are the medians of the per-minute values from the second minute on, over "
          "successful requests; failed requests are counted apart. Disk use is the allocated bytes of the "
          "store. Device writes are those of the device that holds the store and disk flushes those of the "
          "whole disks under it, during the run, less the rate measured with the server idle before the "
          "run, divided by the committed write requests. A write with forced unit access, such as an "
          "O_DIRECT and O_DSYNC write, is durable without a flush, and group commit lets several writers "
          "share one write, so the ratios show the class only with one writer. The state is "
          "checked at the end of the run, after the server was killed with SIGKILL and started again, and "
          "after a normal stop and start. Recovery and restart are the times from a start to the first "
          "answered query, after the SIGKILL and after the normal stop.")


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
    r.add_argument("--uncertain-max", type=int, default=100000, help="uncertain ids a writer keeps")
    r.add_argument("--write-rate", type=float, default=0.0, help="write requests per second, all writers together")
    r.add_argument("--read-timeout", type=float, default=60.0)
    r.add_argument("--write-timeout", type=float, default=120.0)
    r.add_argument("--sample", type=float, default=5.0, help="seconds between monitor samples")
    r.add_argument("--baseline", type=float, default=0.0,
                   help="seconds of idle device counts before the run, subtracted from the run's")
    r.add_argument("--disk-every", type=float, default=60.0, help="seconds between measurements of the store's size")
    r.add_argument("--port", type=int, default=0, help="the server's port, for its RSS and liveness")
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
    m = sub.add_parser("merge")
    m.add_argument("--into", required=True, help="longwrite.json")
    m.add_argument("--engine", required=True)
    m.add_argument("--run", default="", help="the file of `run`")
    m.add_argument("--check", default="")
    m.add_argument("--check-crash", default="")
    m.add_argument("--check-reopen", default="")
    m.add_argument("--restart-s", default="", help="seconds to the first answer after a normal restart")
    m.add_argument("--crash-restart-s", default="", help="seconds to the first answer after the SIGKILL")
    m.add_argument("--answers", default="", help="ok, answer-disagreement, wrong-answer or error: the read set's answer check")
    m.add_argument("--answers-reason", default="", help="the query that failed or differed")
    m.set_defaults(f=cmd_merge)
    s = sub.add_parser("summary")
    s.add_argument("files", nargs="+", help="longwrite.json files: {engine: run file contents with checks}")
    s.set_defaults(f=cmd_summary)
    a = p.parse_args()
    a.f(a)


if __name__ == "__main__":
    main()
