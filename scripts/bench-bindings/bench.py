#!/usr/bin/env python3
"""Binding comparison: each Sparkles binding against the library its users would
otherwise reach for, in the same language and through the same API.

    bench.py [--people N] [--workdir DIR] [--langs jvm,python,node] [options]

* JVM: Jena's API on Sparkles (`DatasetGraphSparkles`, on disk and in memory) against
  TDB2 on disk and Jena's transactional in-memory dataset (TIM), through the same Jena
  calls (`QueryExec`, `Graph.find`, `Graph.contains`, `Model.getProperty`, `Graph.add` in
  a write transaction). Also the small-query throughput of P04 §5.4 on 1, 4 and all cores
  (queries per second with median and 99th percentile latency) and the native calls per
  operation.
* Python: the native `sparkles` API against pyoxigraph's `Store`, and rdflib over the
  Sparkles store plugin against rdflib's default `Memory` store.
* Node.js: @sparkles-rdf/engine against Oxigraph's JavaScript package and N3.js's
  `Store`, with Comunica over the N3 store for SPARQL.

The data and the 28 queries are those of scripts/bench.sh (scripts/gen-data.py at
N people, about 10.5 triples each; the queries are read from bench.sh's `add` lines).
Every query is fully consumed, every row and term.

Method. Answers come first: every engine runs every case once, untimed, and its rows
are fingerprinted with scripts/bench-answers.py's term normalization (`exact` by RDF term,
`value` with numbers compared by value). An engine whose answer differs in value from the
majority of all engines, across languages, is reported and not ranked for that case.
Then, for `--processes` rounds with the engine order rotated each round, every engine
runs in fresh processes: one that loads the data (the load time), one that runs the
read cases with `--warmup` and `--runs` samples each, and one that runs the adds (on a
scratch copy for the stores on disk). A case's figure is the median of its process
medians, and each process's peak RSS is recorded. A sample that exceeds `--timeout`
records a timeout, and a case past `--budget` seconds stops after its first measured
sample. A process that stops answering is killed by a watchdog, its case recorded as a
timeout, and a fresh process resumes after it.

Results go to WORKDIR/results: raw.json (every process, sample and answer) and
summary.md. Run it through `mise run bench:bindings`.

Env and flags for a quiet benchmark machine: `--cpus 0-11` pins every process with
taskset, and `--cores N` (default: the number of pinned CPUs, else all) sets the JVM's
processor count, the Rayon pool and the throughput benchmark's "all cores".
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import importlib.util
import json
import os
import platform
import queue
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
EX = "http://example.org/"
RDF_TYPE = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"

ENGINES = {
    "jvm": ["sparkles", "sparkles-mem", "tdb2", "tim"],
    "python": ["sparkles", "pyoxigraph", "rdflib-sparkles", "rdflib-memory"],
    "node": ["sparkles", "oxigraph", "n3"],
}
LABEL = {
    "jvm:sparkles": "Jena on Sparkles (disk)",
    "jvm:sparkles-mem": "Jena on Sparkles (memory)",
    "jvm:tdb2": "Jena TDB2 (disk)",
    "jvm:tim": "Jena TIM (memory)",
    "python:sparkles": "sparkles",
    "python:pyoxigraph": "pyoxigraph",
    "python:rdflib-sparkles": "rdflib on Sparkles",
    "python:rdflib-memory": "rdflib Memory",
    "node:sparkles": "@sparkles-rdf/engine",
    "node:oxigraph": "oxigraph (JS)",
    "node:n3": "N3.js Store + Comunica",
}
# stores on disk: loaded once into WORKDIR/stores and opened by the later processes
DISK = {"jvm:sparkles", "jvm:tdb2"}
# answers that legitimately depend on the engine (LIMIT without ORDER BY): rows only
COUNT_ONLY = {"q:export-500k"}
GRAPH_CASES = ["iter-all", "pattern-s", "pattern-po", "contains", "value"]
WRITE_CASES = {"jvm": ["adds"], "python": ["adds", "adds-bulk"], "node": ["adds", "adds-bulk"]}
SMALL_OPS = ["sq-select-o", "sq-ask", "op-getProperty", "op-contains", "op-find-s"]
SMALL_QUERIES = ["star-lookup", "values-star"]
CASE_NOTE = {
    "load": "load the N-Triples file (s)",
    "iter-all": "iterate every triple",
    "pattern-s": "pattern (s ? ?) for each of the subjects",
    "pattern-po": "pattern (? rdf:type ex:Researcher)",
    "contains": "contains, for each probe (half present)",
    "value": "value of foaf:name, for each of the subjects",
    "adds": "add triples one at a time in one write transaction",
    "adds-bulk": "add triples with the library's bulk call",
}

_answers = None


def bench_answers():
    """scripts/bench-answers.py as a module, for its term normalization."""
    global _answers
    if _answers is None:
        spec = importlib.util.spec_from_file_location("bench_answers", ROOT / "scripts" / "bench-answers.py")
        _answers = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(_answers)
    return _answers


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def run(cmd, **kw):
    log("$ " + " ".join(str(c) for c in cmd))
    subprocess.run([str(c) for c in cmd], check=True, **kw)


# ------------------------------------------------------------------------- data, cases


def queries():
    """The (name, text) of every query of scripts/bench.sh, read from its `add` lines."""
    text = (ROOT / "scripts" / "bench.sh").read_text()
    prefix = re.search(r"^P='(.*)'$", text, re.M).group(1)
    return [(m.group(1), prefix + m.group(2)) for m in re.finditer(r"^add (\S+) '(.*)'$", text, re.M)]


def projection(q):
    """The variables a SELECT query projects, in order: `?v` at the top level of the
    SELECT clause, and the `?v` of each `(expr AS ?v)`."""
    m = re.search(r"\bSELECT\b(.*?)\bWHERE\b", q, re.S | re.I)
    clause, out, depth, i = m.group(1), [], 0, 0
    while i < len(clause):
        c = clause[i]
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "?":
            v = re.match(r"\?(\w+)", clause[i:]).group(1)
            before = clause[:i].rstrip()
            if depth == 0 or (depth == 1 and re.search(r"\bAS$", before, re.I)):
                out.append(v)
            i += len(v)
        i += 1
    return out


def subjects_and_probes(data, people, n=1000):
    """`n` person IRIs spread over the data, and `n` contains probes: the rdf:type triple
    of each of the first n/2 subjects and an absent type for each."""
    step = max(1, people // n)
    ids = list(range(0, people, step))[:n]
    subjects = [f"{EX}person/{i}" for i in ids]
    want = {s: None for s in subjects[: n // 2]}
    pat = re.compile(r"^<(" + re.escape(EX) + r"person/\d+)> <" + re.escape(RDF_TYPE) + r"> <([^>]*)> \.$")
    with open(data) as f:
        for line in f:
            m = pat.match(line)
            if m and m.group(1) in want and want[m.group(1)] is None:
                want[m.group(1)] = m.group(2)
    probes = []
    for s, t in want.items():
        probes.append([s, RDF_TYPE, t or EX + "Person"])
        probes.append([s, RDF_TYPE, EX + "Nonexistent"])
    return subjects, probes


def read_cases(qs):
    cases = [{"name": c, "kind": c} for c in GRAPH_CASES]
    for name, text in qs:
        cases.append({"name": f"q:{name}", "kind": "query", "query": text, "vars": projection(text)})
    for c in cases:
        if c["kind"] in ("iter-all", "pattern-s", "pattern-po"):
            c["vars"] = ["s", "p", "o"]
        elif c["kind"] == "contains":
            c["vars"] = ["found"]
        elif c["kind"] == "value":
            c["vars"] = ["value"]
    return cases


def small_cases(qs):
    text = dict(qs)
    return [{"name": k, "kind": k} for k in SMALL_OPS] + [
        {"name": f"sq-{q}", "kind": "small-query", "query": text[q], "vars": projection(text[q])} for q in SMALL_QUERIES
    ]


# ------------------------------------------------------------------------------- setup


class Setup:
    def __init__(self, args):
        self.args = args
        self.work = args.workdir
        self.java = os.environ.get("JAVA") or shutil.which("java") or "java"
        self.node = os.environ.get("NODE") or shutil.which("node") or "node"
        self.python = None
        self.classpath = None
        self.calls_classpath = None

    def cargo_env(self):
        env = dict(os.environ)
        env.setdefault("CARGO_BUILD_JOBS", "4")
        return env

    def jvm(self):
        if not self.args.no_build:
            run([ROOT / "scripts" / "jvm-native.sh"], cwd=ROOT, env=self.cargo_env())
            run(["./gradlew", "--console=plain", "-q", ":sparkles-jena:bindingsBenchClasspath"], cwd=ROOT / "jvm")
        d = ROOT / "jvm" / "sparkles-jena" / "build" / "bindings-bench"
        self.classpath = (d / "classpath.txt").read_text().strip()
        self.calls_classpath = (d / "classpath-calls.txt").read_text().strip()

    def py(self):
        crate = ROOT / "crates" / "sparkles-py"
        if not self.args.no_build:
            env = self.cargo_env() | {"PYO3_BUILD_EXTENSION_MODULE": "1"}
            run(["cargo", "build", "--locked", "--manifest-path", crate / "Cargo.toml", "--target-dir", ROOT / "target", "--release"], env=env)
        lib = ROOT / "target" / "release" / "lib_sparkles.so"
        if not lib.exists():
            lib = ROOT / "target" / "release" / "lib_sparkles.dylib"
        pkg = self.work / "py"
        shutil.rmtree(pkg, ignore_errors=True)
        shutil.copytree(crate / "python" / "sparkles", pkg / "sparkles")
        shutil.copy2(lib, pkg / "sparkles" / "_sparkles.abi3.so")
        venv = self.work / "py-venv"
        self.python = venv / "bin" / "python"
        if not self.python.exists():
            base = self.args.python or sys.executable
            if shutil.which("uv"):
                run(["uv", "venv", "--quiet", "--python", base, venv])
            else:
                run([base, "-m", "venv", venv])
        req = HERE / "python" / "requirements.txt"
        if shutil.which("uv"):
            run(["uv", "pip", "install", "--quiet", "--python", self.python, "--require-hashes", "-r", req])
        else:
            run([self.python, "-m", "pip", "install", "--quiet", "--require-hashes", "-r", req])

    def node_setup(self):
        if not self.args.no_build:
            run([ROOT / "scripts" / "node-native.sh"], cwd=ROOT, env=self.cargo_env())
            if not (ROOT / "js" / "node_modules").exists():
                run(["pnpm", "--dir", ROOT / "js", "install", "--frozen-lockfile"])
            run(["pnpm", "--dir", ROOT / "js", "build"])
        deps = self.work / "node-deps"
        deps.mkdir(parents=True, exist_ok=True)
        for f in ("package.json", "package-lock.json"):
            shutil.copy2(HERE / "node" / f, deps / f)
        marker = deps / "node_modules" / ".package-lock.json"
        if not marker.exists() or marker.stat().st_mtime < (deps / "package-lock.json").stat().st_mtime:
            run(["npm", "ci", "--ignore-scripts", "--no-audit", "--no-fund", "--loglevel=error"], cwd=deps)


# ---------------------------------------------------------------------------- processes


class Proc:
    """One benchmark process: its events, peak RSS, wall time and exit status."""

    def __init__(self, engine, mode, rnd, label):
        self.engine, self.mode, self.round, self.label = engine, mode, rnd, label
        self.events, self.rss_kib, self.wall_s, self.exit = [], None, None, None
        self.killed = None

    def json(self):
        return {"engine": self.engine, "mode": self.mode, "round": self.round, "label": self.label,
                "rss_kib": self.rss_kib, "wall_s": self.wall_s, "exit": self.exit, "killed": self.killed,
                "events": self.events}


class Runner:
    def __init__(self, args, setup, common):
        self.args, self.setup, self.common = args, setup, common
        self.work = args.workdir
        self.procs = []
        self.versions = {}

    def env(self):
        env = dict(os.environ)
        if self.args.cores:
            env["RAYON_NUM_THREADS"] = str(self.args.cores)
        env["PYTHONDONTWRITEBYTECODE"] = "1"
        return env

    def command(self, eid, calls=False):
        lang, engine = eid.split(":")
        pin = ["taskset", "-c", self.args.cpus] if self.args.cpus else []
        if lang == "jvm":
            cp = self.setup.calls_classpath if calls else self.setup.classpath
            jopts = [f"-Xmx{self.args.jvm_heap}", "-Xss64m", "-Dorg.slf4j.simpleLogger.defaultLogLevel=warn",
                     f"-Dsparkles.native.path={ROOT / 'target' / 'release' / 'libsparkles_ffi.so'}"]
            if self.args.cores:
                jopts.append(f"-XX:ActiveProcessorCount={self.args.cores}")
            return pin + [self.setup.java, *jopts, "-cp", cp, "io.github.kclejeune.sparkles.jena.bench.BindingsBench"]
        if lang == "python":
            return pin + ["env", f"PYTHONPATH={self.work / 'py'}", str(self.setup.python), "-u", str(HERE / "python" / "runner.py")]
        return pin + [self.setup.node, "--expose-gc", "--max-old-space-size=16384", str(HERE / "node" / "runner.mjs")]

    def config(self, eid, mode, cases, extra=None):
        lang, engine = eid.split(":")
        cfg = dict(self.common)
        cfg.update(engine=engine, mode=mode, cases=cases, warmup=self.args.warmup, runs=self.args.runs,
                   budget_s=self.args.budget, timeout_s=self.args.timeout, add_count=self.args.add_count)
        if lang == "node":
            cfg.update(deps_dir=str(self.work / "node-deps"), engine_module=str(ROOT / "js" / "engine" / "dist" / "index.js"))
        cfg.update(extra or {})
        return cfg

    def launch(self, eid, mode, cases, rnd, extra=None, calls=False):
        """Runs a process to the end, resuming after a case that the watchdog had to kill."""
        done, procs = set(), []
        attempt = 0
        while True:
            label = f"{eid.replace(':', '-')}-{mode}-r{rnd}" + (f"-a{attempt}" if attempt else "")
            cfg = self.config(eid, mode, cases, extra)
            cfg["skip"] = sorted(done | set(cfg.get("skip", [])))
            p = self.one(eid, mode, rnd, label, cfg, calls)
            procs.append(p)
            for e in p.events:
                if e.get("event") == "case":
                    done.add(e["case"])
            if p.killed and p.killed not in ("load", "open"):
                done.add(p.killed)
                attempt += 1
                names = [c["name"] for c in cases] if mode != "throughput" else [f"{c['name']}@{t}" for c in cases for t in cfg["threads"]]
                if any(n not in done for n in names):
                    continue
            return procs

    def one(self, eid, mode, rnd, label, cfg, calls):
        cdir, ldir = self.work / "configs", self.work / "logs"
        cdir.mkdir(parents=True, exist_ok=True)
        ldir.mkdir(parents=True, exist_ok=True)
        cpath = cdir / f"{label}.json"
        cpath.write_text(json.dumps(cfg))
        cmd = self.command(eid, calls) + [str(cpath)]
        p = Proc(eid, mode, rnd, label)
        log(f"{label}: {len(cfg['cases'])} cases" + (f", skipping {len(cfg['skip'])}" if cfg.get("skip") else ""))
        t0 = time.monotonic()
        with open(ldir / f"{label}.stderr", "w") as err:
            child = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=err, env=self.env(), text=True)
            lines = queue.Queue()

            def reader():
                for line in child.stdout:
                    lines.put(line)
                lines.put(None)

            threading.Thread(target=reader, daemon=True).start()
            current, ready = None, False
            per_case = self.args.timeout + 60
            if mode == "throughput":
                per_case += self.args.tp_seconds + self.args.tp_warmup
            while True:
                limit = per_case if ready else self.args.load_timeout
                try:
                    line = lines.get(timeout=limit)
                except queue.Empty:
                    p.killed = current or ("load" if mode == "load" else "open")
                    log(f"{label}: no progress for {limit:.0f} s in {p.killed}; killing pid {child.pid}")
                    child.kill()
                    break
                if line is None:
                    break
                try:
                    e = json.loads(line)
                except ValueError:
                    continue
                ev = e.get("event")
                if ev == "start":
                    current = e["case"]
                elif ev == "case":
                    current = None
                    p.events.append(e)
                    self.show(label, e)
                elif ev == "ready":
                    ready = True
                    p.events.append(e)
                elif ev == "versions":
                    self.versions.setdefault(eid, e["versions"])
                elif ev in ("fatal", "done"):
                    p.events.append(e)
                    if ev == "fatal":
                        log(f"{label}: fatal: {e.get('error')}")
            _, status, ru = os.wait4(child.pid, 0)
            p.exit = os.waitstatus_to_exitcode(status)
            p.rss_kib = ru.ru_maxrss
            p.wall_s = time.monotonic() - t0
        if p.killed:
            p.events.append({"event": "case", "case": p.killed, "status": "timeout", "killed": True,
                             "samples_ms": []})
        self.procs.append(p)
        return p

    @staticmethod
    def show(label, e):
        s = e.get("samples_ms") or []
        if e.get("status") != "ok":
            log(f"  {e['case']}: {e.get('status')} {e.get('error', '')}")
        elif "qps" in e:
            log(f"  {e['case']}: {e['qps']:.0f}/s p50 {e['p50_ms']:.3f} p99 {e['p99_ms']:.3f} ms")
        elif "calls_per_op" in e:
            log(f"  {e['case']}: {e['calls_per_op']:.2f} calls/op")
        elif s:
            log(f"  {e['case']}: {statistics.median(s):.3f} ms ({len(s)}), {e.get('rows')} rows")
        else:
            log(f"  {e['case']}: {e.get('rows')} rows")


# ------------------------------------------------------------------------------ answers


def fingerprint(path, count_only):
    """rows, and the `exact` and `value` digests of bench-answers.py over the rows of an
    answer file: a multiset digest (the sorted digests of the rows), so that large answers
    need no sort of the rows themselves."""
    ba = bench_answers()
    with open(path) as f:
        head = json.loads(f.readline())
        order = sorted(range(len(head["vars"])), key=lambda i: head["vars"][i])
        rows, exact, value = 0, [], []
        for line in f:
            rows += 1
            if count_only:
                continue
            r = json.loads(line)
            r = [r[i] for i in order]
            for key, by_value, acc in (("exact", False, exact), ("value", True, value)):
                acc.append(hashlib.sha256(repr(tuple(ba.term(t, by_value) for t in r)).encode()).digest())
    rec = {"rows": rows}
    if not count_only:
        for key, acc in (("exact", exact), ("value", value)):
            acc.sort()
            h = hashlib.sha256()
            for d in acc:
                h.update(d)
            rec[key] = h.hexdigest()[:16]
    return rec


def majority(vals):
    """the value most engines agree on, if at least two do and no other value ties it"""
    vs = list(vals.values())
    if not vs:
        return None
    counts = sorted((vs.count(v) for v in set(vs)), reverse=True)
    ref = max(set(vs), key=vs.count)
    return ref if counts[0] > 1 and (len(counts) == 1 or counts[1] < counts[0]) else None


def compare(answers):
    """For each case: the engines whose answer differs in value from the majority (not
    ranked), the ones with equal values as different RDF terms, and failed ones."""
    out = {}
    for case, an in answers.items():
        ok = {e: v for e, v in an.items() if v.get("status") == "ok"}
        by_value = {e: v.get("value", v["rows"]) for e, v in ok.items()}
        ref = majority(by_value)
        wrong = {e for e, v in by_value.items() if ref is not None and v != ref}
        split = set(by_value) if ref is None and len(set(by_value.values())) > 1 else set()
        same = {e: v["exact"] for e, v in ok.items() if e not in wrong and "exact" in v}
        eref = majority(same)
        lexical = {e for e, v in same.items() if eref is not None and v != eref}
        out[case] = {"wrong": sorted(wrong | split), "lexical": sorted(lexical),
                     "failed": sorted(e for e, v in an.items() if v.get("status") != "ok")}
    return out


def diff_answers(work, case, engines):
    """How the answers of `engines[1:]` to `case` differ from that of `engines[0]`: the
    rows, by value, that one has and the other lacks."""
    ba = bench_answers()
    sets = {}
    for e in engines:
        path = work / "answers" / e.replace(":", "-") / (case.replace(":", "_") + ".jsonl")
        if not path.exists():
            continue
        with open(path) as f:
            head = json.loads(f.readline())
            order = sorted(range(len(head["vars"])), key=lambda i: head["vars"][i])
            rows = {}
            for line in f:
                r = json.loads(line)
                k = repr(tuple(ba.term(r[i], True) for i in order))
                rows[k] = rows.get(k, 0) + 1
        sets[e] = rows
    lines = []
    # each engine that differs, against one engine with the majority's answer
    names = [e for e in engines if e in sets]
    if not names:
        return lines
    a = names[0]
    for b in names[1:]:
        only_a = [k for k in sets[a] if sets[a][k] > sets[b].get(k, 0)]
        only_b = [k for k in sets[b] if sets[b][k] > sets[a].get(k, 0)]
        if only_a or only_b:
            lines.append(f"{a} vs {b}: {len(only_a)} rows only in {a}, {len(only_b)} only in {b}"
                         + (f"; e.g. {a}: {only_a[0][:200]}" if only_a else "")
                         + (f"; e.g. {b}: {only_b[0][:200]}" if only_b else ""))
    return lines


# ------------------------------------------------------------------------------ summary


def med(xs):
    return statistics.median(xs) if xs else None


def collect(procs):
    """Per engine and case: the process medians, statuses and row counts."""
    res = {}
    for p in procs:
        for e in p.events:
            if e.get("event") != "case":
                continue
            r = res.setdefault(p.engine, {}).setdefault(e["case"], {"medians": [], "status": [], "rows": set(),
                                                                       "samples": 0, "rss_kib": [], "tp": [], "calls": []})
            r["status"].append(e.get("status"))
            if e.get("rows") not in (None, -1):
                r["rows"].add(e["rows"])
            s = e.get("samples_ms") or []
            if s and e.get("status") == "ok":
                r["medians"].append(statistics.median(s))
                r["samples"] += len(s)
            if "qps" in e and e.get("status") == "ok":
                r["tp"].append(e)
            if "calls_per_op" in e:
                r["calls"].append(e["calls_per_op"])
            if p.rss_kib:
                r["rss_kib"].append(p.rss_kib)
    return res


def fmt_ms(v):
    if v is None:
        return "—"
    return f"{v:.3f}" if v < 10 else f"{v:.1f}" if v < 1000 else f"{v:,.0f}"


def summary(args, meta, answers, cmp, procs, versions, diffs):
    res = collect(procs)
    out = ["# Binding comparison", ""]
    out += [f"* Date: {meta['date']}, commit `{meta['commit']}`, host `{meta['host']}`",
            f"* Dataset: {meta['triples']:,} triples ({args.people:,} people, scripts/gen-data.py)",
            f"* CPU: {meta['cpu']}; pinning: {args.cpus or 'none'}; cores: {args.cores or 'all'}; load before the run: {meta['loadavg']}",
            f"* Method: {args.processes} processes per engine and mode, {args.warmup} warm-up and {args.runs} measured samples per case,"
            f" a {args.timeout:.0f} s timeout per sample and a {args.budget:.0f} s budget per case. Figures are medians of process medians in ms"
            " (load in s). † marks an answer that differs from the majority (not ranked), ‡ equal values as different RDF terms.",
            f"* Subjects and probes: {len(meta['subjects'])} subjects, {len(meta['probes'])} contains probes, {args.add_count:,} triples per add sample",
            ""]
    if meta.get("timing_note"):
        out += [f"> {meta['timing_note']}", ""]
    langs = [l for l in ENGINES if l in args.langs]
    for lang in langs:
        engines = [f"{lang}:{e}" for e in ENGINES[lang] if f"{lang}:{e}" in meta["engines"]]
        if not engines:
            continue
        out += [f"## {lang.upper() if lang == 'jvm' else lang.capitalize()}", ""]
        out.append("| Case | " + " | ".join(LABEL[e] for e in engines) + " |")
        out.append("|---|" + "---:|" * len(engines))
        cases = ["load"] + [c["name"] for c in meta["read_cases"]] + WRITE_CASES[lang]
        for case in cases:
            cells, vals = [], {}
            c = cmp.get(case, {"wrong": [], "lexical": [], "failed": []})
            for e in engines:
                r = res.get(e, {}).get(case)
                if r is None:
                    cells.append("—")
                    continue
                bad = [s for s in r["status"] if s != "ok"]
                m = med(r["medians"])
                if m is None:
                    cells.append(bad[0] if bad else "—")
                    continue
                if case == "load":
                    m /= 1000
                mark = " †" if e in c["wrong"] else " ‡" if e in c["lexical"] else ""
                note = f" ({len(bad)} {bad[0]})" if bad else ""
                cells.append((f"{m:.2f}" if case == "load" else fmt_ms(m)) + mark + note)
                if e not in c["wrong"] and not bad:
                    vals[e] = m
            if vals and len(vals) > 1:
                best = min(vals, key=vals.get)
                cells[engines.index(best)] = "**" + cells[engines.index(best)] + "**"
            out.append(f"| {CASE_NOTE.get(case, '`' + case[2:] + '`' if case.startswith('q:') else case)} | " + " | ".join(cells) + " |")
        # peak RSS of the load and read processes
        for mode, title in (("load", "peak RSS, load process (MiB)"), ("time", "peak RSS, read process (MiB)")):
            cells = []
            for e in engines:
                rss = [p.rss_kib for p in procs if p.engine == e and p.mode == mode and p.rss_kib]
                cells.append(f"{med(rss) / 1024:,.0f}" if rss else "—")
            out.append(f"| {title} | " + " | ".join(cells) + " |")
        out.append("")
        if lang == "jvm":
            tp = [(e, res.get(e, {})) for e in engines]
            names = sorted({k for _, r in tp for k in r if "@" in k}, key=lambda k: (k.split("@")[0], int(k.split("@")[1])))
            if names:
                out += ["### Small-query throughput (P04 §5.4)", "",
                        "Queries or operations per second, with the median and 99th percentile latency in ms, each in its own"
                        f" read transaction, {args.tp_seconds:g} s after {args.tp_warmup:g} s of warm-up. The median of the processes.", "",
                        "| Operation @ threads | " + " | ".join(LABEL[e] for e in engines) + " |",
                        "|---|" + "---:|" * len(engines)]
                for k in names:
                    cells = []
                    for e, r in tp:
                        t = r.get(k, {}).get("tp", [])
                        if not t:
                            st = r.get(k, {}).get("status", [])
                            cells.append(st[0] if st else "—")
                            continue
                        cells.append(f"{med([x['qps'] for x in t]):,.0f} ({med([x['p50_ms'] for x in t]):.3f} / {med([x['p99_ms'] for x in t]):.3f})")
                    out.append(f"| {k} | " + " | ".join(cells) + " |")
                out.append("")
            calls = {k: v["calls"] for e in engines for k, v in res.get(e, {}).items() if v["calls"]}
            if calls:
                out += ["### Native calls per operation", "",
                        "Counted with the counting copy of the generated bindings on Jena on Sparkles (memory): calls into the"
                        " native library per query, per lookup (`pattern-s`, `contains`, `value`) and per added triple.", "",
                        "| Operation | Calls |", "|---|---:|"]
                for k, v in calls.items():
                    out.append(f"| {k} | {med(v):.2f} |")
                out.append("")
    out += ["## Answers", ""]
    bad = {k: v for k, v in cmp.items() if v["wrong"] or v["failed"] or v["lexical"]}
    agree = sum(1 for v in cmp.values() if not v["wrong"] and not v["failed"])
    out.append(f"{agree} of {len(cmp)} cases have every engine's answer agreeing by value.")
    out.append("")
    if bad:
        out += ["| Case | Differs (not ranked) | Equal values, other terms | Failed or timed out |", "|---|---|---|---|"]
        for k, v in bad.items():
            failed = ", ".join(f"{e} ({answers[k][e].get('status')})" for e in v["failed"])
            wrong = ", ".join(f"{e} ({answers[k][e]['rows']} rows)" for e in v["wrong"])
            out.append(f"| {k} | {wrong} | {', '.join(v['lexical'])} | {failed} |")
        out.append("")
        for k, lines in diffs.items():
            for line in lines:
                out.append(f"* `{k}`: {line}")
        if diffs:
            out.append("")
    out += ["## Versions", ""]
    for k, v in sorted(meta["tools"].items()):
        out.append(f"* {k}: {v}")
    for e, v in sorted(versions.items()):
        out.append(f"* {LABEL.get(e, e)}: " + ", ".join(f"{a} {b}" for a, b in v.items()))
    out.append("")
    return "\n".join(out)


# --------------------------------------------------------------------------------- main


def tool_versions(langs):
    def cmd(*c):
        try:
            return subprocess.run(c, capture_output=True, text=True, cwd=ROOT).stdout.strip().splitlines()[0]
        except (OSError, IndexError):
            return "?"

    v = {"rustc": cmd("rustc", "--version")}
    if "node" in langs:
        lock = json.loads((HERE / "node" / "package-lock.json").read_text())
        for name in ("oxigraph", "n3", "@comunica/query-sparql-rdfjs"):
            v[f"npm {name}"] = lock["packages"][f"node_modules/{name}"]["version"]
    if "python" in langs:
        for line in (HERE / "python" / "requirements.txt").read_text().splitlines():
            m = re.match(r"^([\w-]+)==([\w.]+)", line)
            if m:
                v[f"pypi {m.group(1)}"] = m.group(2)
    if "jvm" in langs:
        toml = (ROOT / "jvm" / "gradle" / "libs.versions.toml").read_text()
        v["jena (TDB2, TIM, ARQ)"] = re.search(r'^jena = "([^"]+)"', toml, re.M).group(1)
    return v


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--people", type=int, default=100000, help="dataset size in people (about 10.5 triples each)")
    ap.add_argument("--workdir", type=Path, default=ROOT / "target" / "bench-bindings")
    ap.add_argument("--langs", default="jvm,python,node")
    ap.add_argument("--engines", default="", help="only these engines, e.g. jvm:tdb2,python:pyoxigraph")
    ap.add_argument("--cases", default="", help="only these cases, e.g. iter-all,q:star-join")
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--runs", type=int, default=10)
    ap.add_argument("--processes", type=int, default=3, help="fresh processes per engine and mode")
    ap.add_argument("--timeout", type=float, default=300, help="seconds per sample")
    ap.add_argument("--budget", type=float, default=120, help="seconds per case before it stops repeating")
    ap.add_argument("--load-timeout", type=float, default=3600)
    ap.add_argument("--add-count", type=int, default=10000)
    ap.add_argument("--cpus", default=os.environ.get("BENCH_CPUS", ""), help="taskset CPU list, e.g. 0-11")
    ap.add_argument("--cores", type=int, default=int(os.environ.get("BENCH_CORES", "0") or 0))
    ap.add_argument("--tp-threads", default="1,4,all")
    ap.add_argument("--tp-seconds", type=float, default=10)
    ap.add_argument("--tp-warmup", type=float, default=3)
    ap.add_argument("--no-throughput", action="store_true")
    ap.add_argument("--no-calls", action="store_true")
    ap.add_argument("--answers-only", action="store_true", help="check answers, time nothing")
    ap.add_argument("--keep-answers", action="store_true", help="keep every engine's answer rows in WORKDIR/answers")
    ap.add_argument("--no-build", action="store_true", help="use the built bindings as they are")
    ap.add_argument("--jvm-heap", default="8g")
    ap.add_argument("--python", default="", help="the Python interpreter for the virtual environment")
    args = ap.parse_args()
    args.workdir = args.workdir.resolve()
    args.langs = [l for l in args.langs.split(",") if l]
    if args.cpus and not args.cores:
        n = 0
        for part in args.cpus.split(","):
            a, _, b = part.partition("-")
            n += int(b or a) - int(a) + 1
        args.cores = n
    work = args.workdir
    (work / "results").mkdir(parents=True, exist_ok=True)

    data = work / "data.nt"
    if not data.exists():
        log(f"generating the dataset ({args.people} people)")
        with open(data, "w") as f:
            subprocess.run([sys.executable, ROOT / "scripts" / "gen-data.py", str(args.people)], stdout=f, check=True)
    with open(data, "rb") as f:
        triples = sum(1 for _ in f)
    log(f"dataset: {triples:,} triples")
    qs = queries()
    subjects, probes = subjects_and_probes(data, args.people)
    rcases = read_cases(qs)
    only = set(filter(None, args.cases.split(",")))
    if only:
        rcases = [c for c in rcases if c["name"] in only]
    common = {"data": str(data), "subjects": subjects, "probes": probes}

    setup = Setup(args)
    engines = [f"{l}:{e}" for l in args.langs for e in ENGINES[l]]
    if args.engines:
        want = set(args.engines.split(","))
        engines = [e for e in engines if e in want]
    for lang in args.langs:
        if any(e.startswith(lang + ":") for e in engines):
            {"jvm": setup.jvm, "python": setup.py, "node": setup.node_setup}[lang]()
    runner = Runner(args, setup, common)
    stores = work / "stores"
    stores.mkdir(exist_ok=True)

    def store_of(eid, name=None):
        return str(stores / (name or eid.replace(":", "-")))

    # the stores on disk: loaded once, by the first load process
    loads = {}
    for eid in engines:
        if eid in DISK:
            d = Path(store_of(eid))
            if not (d / ".loaded").exists():
                shutil.rmtree(d, ignore_errors=True)
                d.mkdir(parents=True)
                procs = runner.launch(eid, "load", [], 0, {"store": str(d)})
                loads.setdefault(eid, []).extend(procs)
                if any(e.get("status") == "ok" for p in procs for e in p.events if e.get("case") == "load"):
                    (d / ".loaded").write_text("")

    # ---- answers
    answers = {}
    adir = work / "answers"
    for eid in engines:
        d = adir / eid.replace(":", "-")
        shutil.rmtree(d, ignore_errors=True)
        extra = {"answers_dir": str(d)}
        if eid in DISK:
            extra["store"] = store_of(eid)
        procs = runner.launch(eid, "answers", rcases, 0, extra)
        for p in procs:
            for e in p.events:
                if e.get("event") != "case":
                    continue
                rec = {"status": e.get("status"), "rows": e.get("rows")}
                f = d / (e["case"].replace(":", "_") + ".jsonl")
                if e.get("status") == "ok" and f.exists():
                    rec.update(fingerprint(f, e["case"] in COUNT_ONLY))
                    rec["status"] = "ok"
                elif e.get("error"):
                    rec["error"] = e["error"]
                answers.setdefault(e["case"], {})[eid] = rec
    cmp = compare(answers)
    diffs = {}
    for case, v in cmp.items():
        if v["wrong"]:
            ok = [e for e, r in answers[case].items() if r.get("status") == "ok" and e not in v["wrong"]]
            diffs[case] = diff_answers(work, case, ok[:1] + v["wrong"])
    json.dump(answers, open(work / "results" / "answers.json", "w"), indent=1)
    for case, v in cmp.items():
        if v["wrong"] or v["failed"]:
            log(f"answers: {case}: differs {v['wrong']}, failed {v['failed']}")
    if not args.keep_answers:
        shutil.rmtree(adir, ignore_errors=True)

    # ---- timing
    if not args.answers_only:
        threads = []
        for t in args.tp_threads.split(","):
            threads.append(args.cores or os.cpu_count() if t == "all" else int(t))
        threads = sorted(set(threads))
        scases = small_cases(qs)
        for rnd in range(args.processes):
            k = rnd % len(engines)
            for eid in engines[k:] + engines[:k]:
                lang = eid.split(":")[0]
                # load into a fresh store
                extra = {}
                if eid in DISK:
                    d = Path(store_of(eid, eid.replace(":", "-") + ".load"))
                    shutil.rmtree(d, ignore_errors=True)
                    d.mkdir(parents=True)
                    extra["store"] = str(d)
                if rnd > 0 or eid not in loads:
                    runner.launch(eid, "load", [], rnd, extra)
                if eid in DISK:
                    shutil.rmtree(extra["store"], ignore_errors=True)
                # reads
                extra = {"store": store_of(eid)} if eid in DISK else {}
                runner.launch(eid, "time", rcases, rnd, extra)
                # adds, on a scratch copy of a store on disk
                wcases = [{"name": c, "kind": c} for c in WRITE_CASES[lang] if not only or c in only]
                if wcases:
                    extra = {}
                    if eid in DISK:
                        scratch = stores / (eid.replace(":", "-") + ".scratch")
                        shutil.rmtree(scratch, ignore_errors=True)
                        subprocess.run(["cp", "-a", "--reflink=auto", store_of(eid), scratch], check=True)
                        extra["store"] = str(scratch)
                    runner.launch(eid, "time", wcases, rnd, extra)
                    if eid in DISK:
                        shutil.rmtree(extra["store"], ignore_errors=True)
                # small-query throughput (JVM)
                if lang == "jvm" and not args.no_throughput:
                    extra = {"threads": threads, "seconds": args.tp_seconds, "warmup_seconds": args.tp_warmup}
                    if eid in DISK:
                        extra["store"] = store_of(eid)
                    runner.launch(eid, "throughput", scases, rnd, extra)
        if "jvm:sparkles-mem" in engines and not args.no_calls:
            calls = rcases + [{"name": "adds", "kind": "adds"}] + scases
            runner.launch("jvm:sparkles-mem", "calls", calls, 0, calls=True)

    procs = [p for p in runner.procs]
    commit = subprocess.run(["git", "rev-parse", "--short=12", "HEAD"], capture_output=True, text=True, cwd=ROOT).stdout.strip()
    dirty = subprocess.run(["git", "status", "--porcelain", "--untracked-files=no"], capture_output=True, text=True, cwd=ROOT).stdout.strip()
    cpu = "?"
    try:
        cpu = re.search(r"model name\s*:\s*(.*)", Path("/proc/cpuinfo").read_text()).group(1)
    except (OSError, AttributeError):
        pass
    meta = {
        "date": datetime.datetime.now().isoformat(timespec="seconds"),
        "commit": commit + ("-dirty" if dirty else ""),
        "host": platform.node(),
        "cpu": f"{cpu} ({os.cpu_count()} logical CPUs)",
        "loadavg": Path("/proc/loadavg").read_text().split()[:3] if Path("/proc/loadavg").exists() else "?",
        "triples": triples,
        "engines": engines,
        "subjects": subjects,
        "probes": probes,
        "read_cases": [{k: v for k, v in c.items() if k != "query"} for c in rcases],
        "args": {k: (str(v) if isinstance(v, Path) else v) for k, v in vars(args).items()},
        "tools": tool_versions(args.langs),
        "timing_note": os.environ.get("BENCH_NOTE", ""),
    }
    raw = {"meta": meta, "versions": runner.versions, "answers": answers, "compare": cmp, "diffs": diffs,
           "processes": [p.json() for p in procs]}
    json.dump(raw, open(work / "results" / "raw.json", "w"), indent=1)
    md = summary(args, meta, answers, cmp, procs, runner.versions, diffs)
    (work / "results" / "summary.md").write_text(md)
    log(f"wrote {work / 'results' / 'summary.md'} and raw.json")
    bad = [k for k, v in cmp.items() if v["wrong"]]
    if bad:
        log(f"answers differ in {len(bad)} cases: {', '.join(bad)}")


if __name__ == "__main__":
    main()
