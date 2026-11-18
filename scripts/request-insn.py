#!/usr/bin/env python3
"""Per-request instruction counts of two server builds, as the first performance screen.

usage: request-insn.py run --a BIN --b BIN (--store DIR | --data FILE) --queries DIR
                           [--execution streaming,auto] [--rounds 6] [--reps 50] ...
       request-insn.py summarize OUT [--threshold PCT] [--large-threshold PCT]

`run` starts each build as a server in a fresh process for every epoch, in rounds of
A,B,B,A, with the server pinned to one CPU, the client to another, one Rayon thread and
idle-priority spinners on both CPUs. Before the server binary runs, the harness opens
performance counters on its process with `inherit`, so they cover every thread the
server ever starts, and it reads them around each query's window of timed requests.
The counters are user-space instructions and cycles on each core type of a hybrid CPU,
the threads' CPU time (task-clock, which includes kernel time), context switches and
page faults. Kernel instructions are counted too when `kernel.perf_event_paranoid` is 1
or less, and are reported as unavailable otherwise. Each epoch also counts an idle window
of the same length as a query's timed window so the background work of an idle server
can be compared with the per-request counts.

The counts are for the server process only. The client opens a new connection for each
request, as curl does. Each variant's sorted TSV answer to each query is compared once.

`summarize` prints, for each query and mode, the instructions per request of A and B, the
median over rounds of the per-round ratio of B's to A's median per process, a 95%
bootstrap interval over rounds, the spread of the per-process counts of each side and a
bimodality flag, and the same ratio for the CPU time and context switches. A query is
flagged when the interval does not lie within 1 +- threshold, which is 1% for queries under
LARGE_INSTRUCTIONS per request and 0.25% for larger ones by default, the floors that
A-against-A runs on stanley support (see docs/BENCHMARKING.md). A side whose processes
split, as automatic execution does when it picks a different executor in some processes,
is marked and its flag needs a rerun under an explicit mode.
"""

import argparse
import ctypes
import hashlib
import http.client
import json
import os
import random
import re
import shutil
import signal
import statistics as st
import subprocess
import sys
import time
import urllib.parse

# perf_event_open
PERF_TYPE_HARDWARE = 0
PERF_TYPE_SOFTWARE = 1
HW_CPU_CYCLES = 0
HW_INSTRUCTIONS = 1
SW_TASK_CLOCK = 1
SW_PAGE_FAULTS = 2
SW_CONTEXT_SWITCHES = 3
SW_CPU_MIGRATIONS = 4
FORMAT_TOTAL_TIME_ENABLED = 1
FORMAT_TOTAL_TIME_RUNNING = 2
FLAG_DISABLED = 1 << 0
FLAG_INHERIT = 1 << 1
FLAG_EXCLUDE_USER = 1 << 4
FLAG_EXCLUDE_KERNEL = 1 << 5
FLAG_EXCLUDE_HV = 1 << 6
FLAG_ENABLE_ON_EXEC = 1 << 12
MIN_RUN_FRACTION = 0.999
LARGE_INSTRUCTIONS = 2_000_000
SPLIT_SPREAD = 0.05
NR_PERF_EVENT_OPEN = {"x86_64": 298, "aarch64": 241}


class PerfEventAttr(ctypes.Structure):
    # PERF_ATTR_SIZE_VER0, which every kernel accepts
    _fields_ = [
        ("type", ctypes.c_uint32),
        ("size", ctypes.c_uint32),
        ("config", ctypes.c_uint64),
        ("sample_period", ctypes.c_uint64),
        ("sample_type", ctypes.c_uint64),
        ("read_format", ctypes.c_uint64),
        ("flags", ctypes.c_uint64),
        ("wakeup_events", ctypes.c_uint32),
        ("bp_type", ctypes.c_uint32),
        ("config1", ctypes.c_uint64),
    ]


_libc = ctypes.CDLL(None, use_errno=True)


def perf_open(pid, type_, config, flags):
    attr = PerfEventAttr()
    attr.type = type_
    attr.size = ctypes.sizeof(PerfEventAttr)
    attr.config = config
    attr.read_format = FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING
    attr.flags = flags | FLAG_INHERIT | FLAG_EXCLUDE_HV
    nr = NR_PERF_EVENT_OPEN[os.uname().machine]
    fd = _libc.syscall(nr, ctypes.byref(attr), pid, -1, -1, 0)
    if fd < 0:
        return -ctypes.get_errno()
    return fd


def core_pmus():
    """(name, PMU type, CPUs) of each core PMU of a hybrid CPU, or one entry with type 0
    for an ordinary CPU, where the generic hardware events apply."""
    out = []
    for name in ("cpu_core", "cpu_atom"):
        d = f"/sys/devices/{name}"
        if os.path.exists(f"{d}/type"):
            out.append(
                (
                    name,
                    int(open(f"{d}/type").read()),
                    parse_cpus(open(f"{d}/cpus").read()),
                )
            )
    return out or [("cpu", 0, set(range(os.cpu_count())))]


def parse_cpus(s):
    cpus = set()
    for part in s.strip().split(","):
        if not part:
            continue
        a, _, b = part.partition("-")
        cpus.update(range(int(a), int(b or a) + 1))
    return cpus


class Counters:
    """Counters on one process and every thread it starts after they are opened."""

    def __init__(self, pid, kernel):
        self.fds = {}
        self.pid = pid
        self.pmus = core_pmus()
        self.kernel = kernel
        user_only = FLAG_EXCLUDE_KERNEL
        for name, ptype, _ in self.pmus:
            ext = ptype << 32
            self._open(f"instr_u/{name}", pid, PERF_TYPE_HARDWARE, ext | HW_INSTRUCTIONS, user_only)
            self._open(f"cycles_u/{name}", pid, PERF_TYPE_HARDWARE, ext | HW_CPU_CYCLES, user_only)
            if kernel:
                self._open(
                    f"instr_k/{name}", pid, PERF_TYPE_HARDWARE, ext | HW_INSTRUCTIONS, FLAG_EXCLUDE_USER
                )
        for key, cfg in (
            ("task_clock_ns", SW_TASK_CLOCK),
            ("page_faults", SW_PAGE_FAULTS),
            ("migrations", SW_CPU_MIGRATIONS),
        ):
            self._open(key, pid, PERF_TYPE_SOFTWARE, cfg, user_only)

    def _open(self, key, pid, type_, config, flags):
        fd = perf_open(pid, type_, config, flags)
        if fd < 0:
            raise OSError(-fd, f"perf_event_open {key}: {os.strerror(-fd)}")
        self.fds[key] = fd

    def read(self):
        out = {"ctx_switches": proc_ctx_switches(self.pid), "sched_ns": proc_sched_ns(self.pid)}
        for key, fd in self.fds.items():
            v, enabled, running = (int.from_bytes(os.read(fd, 24)[i : i + 8], "little") for i in (0, 8, 16))
            out[key] = (v, enabled, running)
        return out

    def close(self):
        for fd in self.fds.values():
            os.close(fd)
        self.fds = {}


def proc_ctx_switches(pid):
    """Voluntary and involuntary context switches of the process's live threads, from
    /proc, since a software counter of switches needs kernel counting. A thread that
    exits during a window drops out, which the harness reports as a negative count."""
    total = 0
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return (0, 0, 0)
    for t in tids:
        try:
            with open(f"/proc/{pid}/task/{t}/status") as f:
                total += sum(int(line.split()[-1]) for line in f if "ctxt_switches" in line)
        except OSError:
            pass
    return (total, 0, 0)


def proc_sched_ns(pid):
    """The CPU time of the process's live threads from /proc schedstat, which the
    harness compares with the counters' task-clock to check that they cover every
    thread."""
    total = 0
    try:
        tids = os.listdir(f"/proc/{pid}/task")
    except OSError:
        return (0, 0, 0)
    for t in tids:
        try:
            with open(f"/proc/{pid}/task/{t}/schedstat") as f:
                total += int(f.read().split()[0])
        except (OSError, ValueError, IndexError):
            pass
    return (total, 0, 0)


def kernel_counting_allowed():
    try:
        return int(open("/proc/sys/kernel/perf_event_paranoid").read()) <= 1
    except OSError:
        return False


def delta(a, b, server_cpu_pmu):
    """Folds two counter reads into per-key differences. The per-PMU hardware counts are
    summed. `run_frac` is the smallest share of the window that a counter of the PMU of
    the server's CPU ran for, and `covered` is false when it shows multiplexing or when
    the counters' CPU time falls short of schedstat's."""
    out = {}
    covered = True
    for key, (v1, e1, r1) in b.items():
        v0, e0, r0 = a[key]
        name, _, pmu = key.partition("/")
        out[name] = out.get(name, 0) + (v1 - v0)
        if pmu == server_cpu_pmu and e1 > e0:
            out["run_frac"] = min(out.get("run_frac", 1.0), (r1 - r0) / (e1 - e0))
    # A counter that ran for less of the window than it was enabled was multiplexed
    # with other events, and its count is an estimate.
    out.setdefault("run_frac", 1.0)
    if out["run_frac"] < MIN_RUN_FRACTION:
        covered = False
    # The counters miss a thread when they were opened after it started. Its CPU time
    # then shows in schedstat and not in task-clock. A thread that exits inside the
    # window drops out of schedstat, so only a shortfall of task-clock counts.
    # Windows of less than a millisecond of CPU time are left out, since schedstat's
    # accounting of a running thread lags by up to a scheduler tick.
    if out.get("sched_ns", 0) > 1_000_000 and out["task_clock_ns"] < 0.98 * out["sched_ns"]:
        covered = False
    out["covered"] = covered
    return out


# the server


def server_cmd(bin_, cpu, port, store, srv, extra):
    return [
        "taskset",
        "-c",
        str(cpu),
        bin_,
        "--result-cache-mb",
        "0",
        "serve",
        "--data",
        srv,
        "--loc",
        f"bench={store}",
        "--port",
        str(port),
        "--timeout",
        "600",
        "--no-access-log",
        *extra,
    ]


class Server:
    def __init__(self, cmd, env, log, kernel):
        # The child blocks on a pipe until the counters are open, then execs the server,
        # so the counters' inheritance covers every thread the server starts.
        r, w = os.pipe()
        self.proc = subprocess.Popen(
            ["sh", "-c", 'read _ || exit 1; exec "$@" < /dev/null', "sh", *cmd],
            env=env,
            stdout=log,
            stderr=subprocess.STDOUT,
            stdin=r,
        )
        os.close(r)
        try:
            self.counters = Counters(self.proc.pid, kernel)
        except OSError:
            self.proc.kill()
            os.close(w)
            raise
        os.write(w, b"go\n")
        os.close(w)

    def stop(self):
        self.counters.close()
        self.proc.send_signal(signal.SIGTERM)
        try:
            self.proc.wait(30)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()


EXEC_RE = re.compile(rb'"execMs":([0-9.eE+-]+)')


def request(port, query, mode, accept="application/x-sparkles+json"):
    fields = {"query": query}
    if mode:
        fields["execution"] = mode
    body = urllib.parse.urlencode(fields)
    t0 = time.perf_counter()
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=600)
    conn.request(
        "POST",
        "/bench/sparql",
        body,
        {"Accept": accept, "Content-Type": "application/x-www-form-urlencoded"},
    )
    r = conn.getresponse()
    data = r.read()
    conn.close()
    wall = (time.perf_counter() - t0) * 1000
    if r.status != 200:
        raise RuntimeError(f"HTTP {r.status}: {data[:300]!r}")
    m = EXEC_RE.search(data)
    return wall, float(m.group(1)) if m else float("nan"), data


def wait_ready(port, proc, timeout=60):
    end = time.time() + timeout
    while time.time() < end:
        if proc.poll() is not None:
            raise RuntimeError("the server exited, see its log")
        try:
            c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
            c.request("GET", "/$/ready")
            if c.getresponse().status == 200:
                c.close()
                return
        except OSError:
            pass
        time.sleep(0.05)
    raise RuntimeError("the server did not become ready")


def spinners(cpus):
    out = []
    for c in cpus:
        out.append(
            subprocess.Popen(
                ["taskset", "-c", str(c), "chrt", "-i", "0", "sh", "-c", "while :; do :; done"],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        )
    return out


def answer_digest(data):
    lines = data.decode("utf-8", "replace").split("\n")
    head, rows = lines[0], sorted(lines[1:])
    return len([r for r in rows if r]), hashlib.sha256((head + "\n" + "\n".join(rows)).encode()).hexdigest()[:16]


def load_store(bin_, data, store, log):
    os.makedirs(os.path.dirname(store) or ".", exist_ok=True)
    src = data
    tmp = None
    if data.endswith(".zst"):
        tmp = store + ".nt"
        subprocess.run(["zstd", "-q", "-d", "-f", data, "-o", tmp], check=True)
        src = tmp
    subprocess.run([bin_, "load", "--loc", store, src], check=True, stdout=log, stderr=subprocess.STDOUT)
    if tmp:
        os.remove(tmp)


def compilers(bin_):
    try:
        out = subprocess.run(["readelf", "-p", ".comment", bin_], capture_output=True, text=True).stdout
    except OSError:
        return "unknown"
    names = sorted({re.sub(r"^\s*\[\s*[0-9a-f]+\]\s*", "", l).strip() for l in out.splitlines() if "]" in l})
    return "; ".join(n for n in names if n)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def cmd_run(a):
    out = os.path.realpath(a.out)
    if os.path.exists(os.path.join(out, "samples.tsv")):
        sys.exit(f"{out} already holds a run")
    os.makedirs(out, exist_ok=True)
    bins = {"A": os.path.realpath(a.a), "B": os.path.realpath(a.b)}
    qnames = sorted(f[:-3] for f in os.listdir(a.queries) if f.endswith(".rq"))
    if a.only:
        want = a.only.split(",")
        missing = [q for q in want if q not in qnames]
        if missing:
            sys.exit(f"no query file for {missing}")
        qnames = want
    queries = {q: open(os.path.join(a.queries, f"{q}.rq")).read() for q in qnames}
    modes = [m for m in a.execution.split(",")] if a.execution else [""]
    kernel = kernel_counting_allowed()
    pmus = core_pmus()
    server_pmu = next((n for n, _, cpus in pmus if a.server_cpu in cpus), pmus[0][0])
    if len(pmus) > 1 and server_pmu != "cpu_core":
        print(f"warning: server CPU {a.server_cpu} is on {server_pmu}, not a performance core", file=sys.stderr)
    logf = open(os.path.join(out, "harness.log"), "a")

    def log(msg):
        line = f"{time.strftime('%H:%M:%S')} {msg}"
        print(line, file=logf, flush=True)
        if not a.quiet:
            print(line, flush=True)

    stores = {}
    if a.store:
        stores = {"A": os.path.realpath(a.store), "B": os.path.realpath(a.store)}
    else:
        for v in ("A",) if not a.separate_stores else ("A", "B"):
            stores[v] = os.path.join(out, f"store-{v}")
            if not os.path.exists(stores[v]):
                log(f"loading {a.data} with {v}")
                load_store(bins[v], a.data, stores[v], logf)
        stores.setdefault("B", stores["A"])

    manifest = {
        "host": os.uname().nodename,
        "kernel": os.uname().release,
        "cpu": next((l.split(":", 1)[1].strip() for l in open("/proc/cpuinfo") if l.startswith("model name")), ""),
        "A": {"path": bins["A"], "sha256": sha256(bins["A"]), "compilers": compilers(bins["A"]), "label": a.label_a},
        "B": {"path": bins["B"], "sha256": sha256(bins["B"]), "compilers": compilers(bins["B"]), "label": a.label_b},
        "store": stores,
        "data": a.data,
        "queries": qnames,
        "modes": modes,
        "rounds": a.rounds,
        "reps": a.reps,
        "warmup": a.warmup,
        "server_cpu": a.server_cpu,
        "client_cpu": a.client_cpu,
        "rayon_threads": a.rayon_threads,
        "spinners": not a.no_spin,
        "kernel_instructions": kernel,
        "perf_event_paranoid": open("/proc/sys/kernel/perf_event_paranoid").read().strip(),
        "load_before": open("/proc/loadavg").read().strip(),
        "started": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
    }
    if manifest["A"]["compilers"] != manifest["B"]["compilers"]:
        log("warning: A and B were built with different compilers")
    os.sched_setaffinity(0, {a.client_cpu})
    env = dict(os.environ, RAYON_NUM_THREADS=str(a.rayon_threads))
    spins = [] if a.no_spin else spinners([a.server_cpu, a.client_cpu])
    samples = open(os.path.join(out, "samples.tsv"), "w")
    cols = [
        "round", "epoch", "variant", "query", "mode", "reps", "instr_u", "instr_k", "cycles_u",
        "task_clock_ns", "ctx_switches", "page_faults", "migrations", "covered", "window_ms",
        "exec_ms", "wall_ms", "sched_ns", "run_frac",
    ]
    samples.write("\t".join(cols) + "\n")
    answers = {}
    problems = []
    epoch = 0
    srv = None
    try:
        for rnd in range(1, a.rounds + 1):
            for v in ("A", "B", "B", "A"):
                epoch += 1
                srvdir = os.path.join(out, "srv")
                shutil.rmtree(srvdir, ignore_errors=True)
                slog = open(os.path.join(out, f"server-{epoch}-{v}.log"), "w")
                cmd = server_cmd(bins[v], a.server_cpu, a.port, stores[v], srvdir, a.serve_args.split())
                srv = Server(cmd, env, slog, kernel)
                wait_ready(a.port, srv.proc)
                windows = []
                for q in qnames:
                    for mode in modes:
                        key = (q, mode)
                        if (v, key) not in answers:
                            _, _, data = request(a.port, queries[q], mode, "text/tab-separated-values")
                            answers[(v, key)] = answer_digest(data)
                        warm = [request(a.port, queries[q], mode)[0] for _ in range(a.warmup)]
                        # large answers get fewer counted requests, since their counts
                        # vary less and each takes long
                        reps = a.reps
                        if warm and a.max_window_ms:
                            reps = max(a.min_reps, min(a.reps, int(a.max_window_ms / max(st.median(warm), 1e-3))))
                        c0 = srv.counters.read()
                        t0 = time.perf_counter()
                        walls, execs = [], []
                        for _ in range(reps):
                            w, e, _ = request(a.port, queries[q], mode)
                            walls.append(w)
                            execs.append(e)
                        t1 = time.perf_counter()
                        c1 = srv.counters.read()
                        d = delta(c0, c1, server_pmu)
                        windows.append(t1 - t0)
                        row = [
                            rnd, epoch, v, q, mode or "-", reps, d["instr_u"], d.get("instr_k", ""),
                            d["cycles_u"], d["task_clock_ns"], d["ctx_switches"], d["page_faults"],
                            d["migrations"], int(d["covered"]), f"{(t1 - t0) * 1000:.3f}",
                            f"{st.median(execs):.6f}", f"{st.median(walls):.4f}", d["sched_ns"],
                            f"{d['run_frac']:.6f}",
                        ]
                        samples.write("\t".join(str(x) for x in row) + "\n")
                # an idle window as long as the median query window
                idle_s = st.median(windows)
                c0 = srv.counters.read()
                time.sleep(idle_s)
                c1 = srv.counters.read()
                d = delta(c0, c1, server_pmu)
                row = [
                    rnd, epoch, v, "_idle", "-", 1, d["instr_u"], d.get("instr_k", ""), d["cycles_u"],
                    d["task_clock_ns"], d["ctx_switches"], d["page_faults"], d["migrations"],
                    int(d["covered"]), f"{idle_s * 1000:.3f}", "nan", "nan", d["sched_ns"],
                    f"{d['run_frac']:.6f}",
                ]
                samples.write("\t".join(str(x) for x in row) + "\n")
                samples.flush()
                srv.stop()
                srv = None
                slog.close()
                log(f"round {rnd} epoch {epoch} ({v}) done")
    finally:
        if srv is not None:
            srv.stop()
        for s in spins:
            s.kill()
            s.wait()
    for q in qnames:
        for mode in modes:
            ka, kb = answers.get(("A", (q, mode))), answers.get(("B", (q, mode)))
            if ka != kb:
                problems.append(f"answers differ for {q} {mode or '-'}: A {ka} B {kb}")
    manifest["answers"] = {f"{v}/{q}/{m or '-'}": list(d) for (v, (q, m)), d in answers.items()}
    manifest["load_after"] = open("/proc/loadavg").read().strip()
    manifest["ended"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
    manifest["problems"] = problems
    json.dump(manifest, open(os.path.join(out, "manifest.json"), "w"), indent=1)
    for p in problems:
        log(f"FAIL: {p}")
    text = summarize(out, a.threshold, a.large_threshold)
    open(os.path.join(out, "summary.md"), "w").write(text)
    print(text)
    return 1 if problems else 0


# statistics


def per_round_ratios(rows, metric):
    """Per round, B's median over its processes divided by A's."""
    by = {}
    for r in rows:
        by.setdefault(r["round"], {}).setdefault(r["variant"], []).append(r[metric])
    out = []
    for rnd in sorted(by):
        va, vb = by[rnd].get("A"), by[rnd].get("B")
        if va and vb and st.median(va) > 0:
            out.append(st.median(vb) / st.median(va))
    return out


def bootstrap(ratios, n=10000, seed=42):
    if len(ratios) < 2:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    boots = sorted(st.median(rng.choices(ratios, k=len(ratios))) for _ in range(n))
    return boots[int(0.025 * n)], boots[int(0.975 * n) - 1]


def spread(values):
    """(max - min) / median of the per-process values."""
    m = st.median(values)
    return (max(values) - min(values)) / m if m else 0.0


def bimodal(values, rel=0.002, factor=4.0):
    """True when the sorted per-process values split at one gap that is wider than `rel`
    of the median and `factor` times the widest gap within the two groups on either side.
    Such a split comes from processes that differ by a fixed cost, such as a wake-up path
    or an address layout, and the median of a few processes then depends on which group
    more of them landed in."""
    v = sorted(values)
    if len(v) < 4:
        return False
    m = st.median(v)
    gaps = [v[i + 1] - v[i] for i in range(len(v) - 1)]
    i = max(range(len(gaps)), key=gaps.__getitem__)
    rest = gaps[:i] + gaps[i + 1 :]
    if min(i + 1, len(v) - i - 1) < 2:
        return False
    return gaps[i] > rel * m and gaps[i] > factor * max(rest + [0])


def load_samples(out):
    rows = []
    with open(os.path.join(out, "samples.tsv")) as f:
        hdr = f.readline().rstrip("\n").split("\t")
        for line in f:
            p = dict(zip(hdr, line.rstrip("\n").split("\t")))
            reps = int(p["reps"])
            r = {"round": int(p["round"]), "epoch": int(p["epoch"]), "variant": p["variant"],
                 "query": p["query"], "mode": p["mode"], "covered": p["covered"] == "1", "reps": reps}
            for k in ("instr_u", "instr_k", "cycles_u", "task_clock_ns", "ctx_switches", "page_faults"):
                r[k] = int(p[k]) / reps if p.get(k) not in (None, "") else None
            r["window_ms"] = float(p["window_ms"])
            r["exec_ms"] = float(p["exec_ms"])
            r["wall_ms"] = float(p["wall_ms"])
            rows.append(r)
    return rows


def fmt_count(x):
    if x >= 1e9:
        return f"{x / 1e9:.3f}G"
    if x >= 1e6:
        return f"{x / 1e6:.3f}M"
    if x >= 1e3:
        return f"{x / 1e3:.1f}k"
    return f"{x:.0f}"


def summarize(out, threshold, large_threshold=0.25):
    rows = load_samples(out)
    try:
        man = json.load(open(os.path.join(out, "manifest.json")))
    except OSError:
        man = {}
    lines = ["# Per-request instruction counts", ""]
    if man:
        for v in ("A", "B"):
            m = man[v]
            lines.append(f"- {v}: {m.get('label') or m['path']}, sha256 {m['sha256'][:16]}, {m['compilers']}")
        lines.append(
            f"- host {man['host']}, {man['cpu']}, kernel {man['kernel']}, perf_event_paranoid "
            f"{man['perf_event_paranoid']}, kernel instructions "
            f"{'counted' if man['kernel_instructions'] else 'not available'}"
        )
        lines.append(
            f"- {man['rounds']} rounds of A,B,B,A, {man['warmup']} warm-up and {man['reps']} counted "
            f"requests per query and process, server CPU {man['server_cpu']}, client CPU "
            f"{man['client_cpu']}, RAYON_NUM_THREADS={man['rayon_threads']}, spinners "
            f"{'on' if man['spinners'] else 'off'}"
        )
        lines.append(f"- load before {man['load_before']}, after {man.get('load_after', '?')}")
        for p in man.get("problems", []):
            lines.append(f"- FAIL: {p}")
    lines.append("")
    uncovered = sum(1 for r in rows if not r["covered"])
    if uncovered:
        lines.append(
            f"FAIL: {uncovered} windows were not fully counted: a counter on the server's core "
            "type was multiplexed, or the counters missed a thread."
        )
        lines.append("")
    idle = [r for r in rows if r["query"] == "_idle"]
    keys = []
    for r in rows:
        k = (r["query"], r["mode"])
        if r["query"] != "_idle" and k not in keys:
            keys.append(k)
    lines.append(
        f"Flag: the 95% interval of the instruction ratio does not lie within 1 +- {threshold:g}% "
        f"below {LARGE_INSTRUCTIONS / 1e6:g}M instructions per request or 1 +- {large_threshold:g}% above. "
        "Spread is (max - min) / median of the per-process counts. Bimodal marks a side whose "
        "processes split into two groups or spread by more than 5%."
    )
    lines.append("")
    lines.append(
        "| query | mode | A instr/req | B instr/req | B/A | 95% interval | per-round min-max "
        "| A spread | B spread | bimodal | cpu-time B/A | ctx-sw/req A, B | idle share | flag |"
    )
    lines.append("|---|---|--:|--:|--:|---|---|--:|--:|---|--:|---|--:|---|")
    idle_by_epoch = {r["epoch"]: r for r in idle}
    flagged = []
    for q, mode in sorted(keys):
        rs = [r for r in rows if r["query"] == q and r["mode"] == mode]
        va = [r["instr_u"] for r in rs if r["variant"] == "A"]
        vb = [r["instr_u"] for r in rs if r["variant"] == "B"]
        if not va or not vb:
            continue
        ratios = per_round_ratios(rs, "instr_u")
        med = st.median(ratios)
        lo, hi = bootstrap(ratios)
        cpu = st.median(per_round_ratios(rs, "task_clock_ns"))
        csa = st.median([r["ctx_switches"] for r in rs if r["variant"] == "A"])
        csb = st.median([r["ctx_switches"] for r in rs if r["variant"] == "B"])
        # the idle server's instructions over a window as long as this query's, as a
        # share of the window's count
        shares = []
        for r in rs:
            i = idle_by_epoch.get(r["epoch"])
            if i and i["window_ms"] > 0 and r["instr_u"] > 0:
                shares.append((i["instr_u"] / i["window_ms"]) * r["window_ms"] / (r["instr_u"] * r["reps"]))
        share = st.median(shares) if shares else float("nan")
        bi = ",".join(
            s for s, vals in (("A", va), ("B", vb)) if bimodal(vals) or spread(vals) > SPLIT_SPREAD
        )
        th = (threshold if st.median(va) < LARGE_INSTRUCTIONS else large_threshold) / 100
        flag = not (lo >= 1 - th and hi <= 1 + th)
        if flag:
            flagged.append(f"{q} {mode}")
        lines.append(
            f"| {q} | {mode} | {fmt_count(st.median(va))} | {fmt_count(st.median(vb))} | {med:.4f} "
            f"| {lo:.4f}-{hi:.4f} | {min(ratios):.4f}-{max(ratios):.4f} | {spread(va) * 100:.2f}% "
            f"| {spread(vb) * 100:.2f}% | {bi or '-'} | {cpu:.3f} | {csa:.2f}, {csb:.2f} | {share * 100:.2f}% "
            f"| {'FLAG' if flag else ''} |"
        )
    lines.append("")
    if rows and rows[0]["instr_k"] is not None:
        lines.append("Kernel instructions per request (median per process):")
        lines.append("")
        lines.append("| query | mode | A | B | B/A |")
        lines.append("|---|---|--:|--:|--:|")
        for q, mode in sorted(keys):
            rs = [r for r in rows if r["query"] == q and r["mode"] == mode]
            ratios = per_round_ratios(rs, "instr_k")
            ka = st.median([r["instr_k"] for r in rs if r["variant"] == "A"])
            kb = st.median([r["instr_k"] for r in rs if r["variant"] == "B"])
            lines.append(f"| {q} | {mode} | {fmt_count(ka)} | {fmt_count(kb)} | {st.median(ratios):.4f} |")
        lines.append("")
    lines.append(f"{len(flagged)} of {len(keys)} flagged" + (f": {', '.join(flagged)}" if flagged else ""))
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--a", required=True, help="base server binary")
    r.add_argument("--b", required=True, help="tip server binary (the same path counts A against A)")
    r.add_argument("--label-a", default="")
    r.add_argument("--label-b", default="")
    g = r.add_mutually_exclusive_group(required=True)
    g.add_argument("--store", help="an existing store directory both builds serve")
    g.add_argument("--data", help="N-Triples (.nt or .nt.zst) that A loads into OUT/store-A")
    r.add_argument("--separate-stores", action="store_true", help="with --data, B loads its own store")
    r.add_argument("--queries", required=True, help="directory of NAME.rq files")
    r.add_argument("--only", default="", help="comma-separated query names")
    r.add_argument("--execution", default="", help="comma-separated execution modes, such as streaming,auto")
    r.add_argument("--rounds", type=int, default=6)
    r.add_argument("--reps", type=int, default=50, help="counted requests per query and process")
    r.add_argument("--warmup", type=int, default=5)
    r.add_argument("--max-window-ms", type=float, default=3000,
                   help="cut a query's counted requests to about this long a window (0 keeps --reps)")
    r.add_argument("--min-reps", type=int, default=5)
    r.add_argument("--server-cpu", type=int, default=2)
    r.add_argument("--client-cpu", type=int, default=3)
    r.add_argument("--rayon-threads", type=int, default=1)
    r.add_argument("--port", type=int, default=4191)
    r.add_argument("--serve-args", default="", help="extra flags for serve")
    r.add_argument("--no-spin", action="store_true")
    r.add_argument("--threshold", type=float, default=1.0, help="flag threshold in percent for small queries")
    r.add_argument("--large-threshold", type=float, default=0.25,
                   help="flag threshold in percent for queries of LARGE_INSTRUCTIONS or more per request")
    r.add_argument("--out", required=True)
    r.add_argument("--quiet", action="store_true")
    s = sub.add_parser("summarize")
    s.add_argument("out")
    s.add_argument("--threshold", type=float, default=1.0)
    s.add_argument("--large-threshold", type=float, default=0.25)
    a = p.parse_args()
    if a.cmd == "run":
        sys.exit(cmd_run(a))
    print(summarize(a.out, a.threshold, a.large_threshold), end="")


if __name__ == "__main__":
    main()
