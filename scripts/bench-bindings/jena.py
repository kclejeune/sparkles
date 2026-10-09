#!/usr/bin/env python3
"""Realistic Jena work on Sparkles against TDB2 and Jena's in-memory dataset (TIM).

    jena.py [--people N] [--workdir DIR] [--arm LABEL=ENGINE[;JVM OPTION]...]... [options]

Each case is one unit of an application's work through Jena's API, in its own transaction:
Model and Graph calls (getProperty, listProperties, listStatements, listSubjectsWithProperty,
resource navigation, a full iteration), Jena's RDFS reasoner over the dataset's graph, small
SPARQL SELECT, ASK, CONSTRUCT and DESCRIBE queries, parameterized queries
(ParameterizedSparqlString, `substitution` and an initial binding on a parsed query),
small SPARQL updates and Model writes, a bulk load with RDFDataMgr, and queries over HTTP
through Fuseki serving the dataset. The runner is
jvm/sparkles-jena/src/test/kotlin/.../bench/JenaUseCases.kt.

Method. Every arm runs in fresh JVMs. A check process per arm first runs each read case's
first operations and fingerprints the answers, order-independently, and the summary flags
an arm whose fingerprint differs from the majority's. Then for `--rounds` rounds every arm runs a
timing process, in the arms' order on even rounds and in reverse on odd ones (A/B/B/A).
Each case runs back to back on each thread count for a warm-up period and a measured one,
and reports operations per second and the median and 99th percentile latency. A figure is
the median over the arm's processes. Stores on disk are loaded once and copied for each
timing process, because the write cases change them.

An arm is an engine (`sparkles-mem`, `sparkles` on disk, `tdb2` on disk or `tim`) and JVM
options separated by semicolons, so a feature switch gives a before arm, e.g.
`--arm "before=sparkles-mem;-Dsparkles.smallQueries=0"`. Results go to WORKDIR/results:
raw.json and summary.md. Run it through `mise run bench:jena`.
"""

from __future__ import annotations

import argparse
import datetime
import importlib.util
import json
import os
import platform
import re
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent

# the cases in the order of the summary, with what each operation does
CASES = {
    "graph-contains": "Graph.contains on a bound triple (half present)",
    "model-getProperty": "Resource.getProperty(foaf:name)",
    "model-listProperties": "every statement of a resource",
    "model-listStatements-sp": "listStatements(person, foaf:knows, null)",
    "model-listSubjectsWithProperty": "listSubjectsWithProperty(ex:worksFor, org), about 35",
    "model-listResourcesOfType": "listResourcesWithProperty(rdf:type, Manager or Researcher), 1,000 to 3,000",
    "model-property-chain": "person name, employer, employer's name",
    "model-navigate": "rdf:type check, then each friend's name and age",
    "model-iterate-all": "iterate every triple of the default graph",
    "rdfs-hasType": "RDFS model: is the person an ex:Person (by subclass)",
    "rdfs-listTypes": "RDFS model: every type of a person",
    "sparql-ask": "ASK on a bound triple",
    "sparql-select-o": "SELECT ?o { <s> foaf:name ?o }",
    "sparql-select-po": "SELECT ?p ?o { <s> ?p ?o }",
    "sparql-friends": "friends and their names (two patterns)",
    "sparql-star-lookup": "bench.sh star-lookup on an organization",
    "sparql-values-star": "bench.sh values-star on five people",
    "sparql-filter": "employees of an organization older than 50",
    "sparql-count": "COUNT of an organization's employees",
    "sparql-repeated": "the same GROUP BY count every time (result cache allowed)",
    "sparql-construct": "CONSTRUCT WHERE { <s> ?p ?o }",
    "sparql-describe": "DESCRIBE <s>",
    "sparql-pss": "ParameterizedSparqlString, setIri, QueryExecution",
    "sparql-substitution": "parsed Query, QueryExec substitution",
    "sparql-initial-binding": "parsed Query, QueryExec initialBinding",
    "fuseki-select": "SELECT ?o over HTTP through Fuseki",
    "fuseki-star-lookup": "star-lookup over HTTP through Fuseki",
    "update-insert-data": "SPARQL INSERT DATA of two triples",
    "update-modify": "SPARQL DELETE/INSERT WHERE on one resource",
    "model-add-txn": "Model: a new resource with five statements",
    "model-update-txn": "Model: read, remove and add a counter",
    "bulk-load": "RDFDataMgr.read of the small file into a new store (1 thread)",
}
DISK = {"sparkles", "tdb2"}


def log(msg):
    print(time.strftime("%H:%M:%S"), msg, flush=True)


def run(cmd, **kw):
    log("$ " + " ".join(str(c) for c in cmd))
    subprocess.run([str(c) for c in cmd], check=True, **kw)


def bench_module():
    """scripts/bench-bindings/bench.py, for its build and data helpers."""
    spec = importlib.util.spec_from_file_location("bench_bindings", HERE / "bench.py")
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def parse_arm(text):
    label, _, rest = text.partition("=")
    engine, *opts = rest.split(";")
    if not label or engine not in ("sparkles-mem", "sparkles", "tdb2", "tim"):
        raise SystemExit(f"bad --arm {text!r}: LABEL=ENGINE[;JVM OPTION]..., ENGINE one of sparkles-mem, sparkles, tdb2, tim")
    return {"label": label, "engine": engine, "opts": [o for o in opts if o]}


class Runner:
    def __init__(self, args, classpath):
        self.args, self.classpath = args, classpath
        self.procs = []
        self.java = os.environ.get("JAVA") or shutil.which("java") or "java"

    def command(self, arm):
        a = self.args
        pin = ["taskset", "-c", a.cpus] if a.cpus else []
        jopts = [f"-Xmx{a.jvm_heap}", "-Xss64m", "-Dorg.slf4j.simpleLogger.defaultLogLevel=warn",
                 f"-Dsparkles.native.path={ROOT / 'target' / 'release' / 'libsparkles_ffi.so'}"]
        if a.cores:
            jopts.append(f"-XX:ActiveProcessorCount={a.cores}")
        jopts += a.jvm_opts.split() + arm["opts"]
        return pin + [self.java, *jopts, "-cp", self.classpath, "io.github.kclejeune.sparkles.jena.bench.JenaUseCases"]

    def launch(self, arm, mode, cfg, label, timeout):
        cdir, ldir = self.args.workdir / "configs", self.args.workdir / "logs"
        cdir.mkdir(parents=True, exist_ok=True)
        ldir.mkdir(parents=True, exist_ok=True)
        cfg = dict(cfg, engine=arm["engine"], mode=mode, tag=label)
        cpath = cdir / f"{label}.json"
        cpath.write_text(json.dumps(cfg))
        env = dict(os.environ)
        if self.args.cores:
            env["RAYON_NUM_THREADS"] = str(self.args.cores)
        log(f"{label}: {mode}, {len(cfg.get('cases', []))} cases")
        t0 = time.monotonic()
        events, killed = [], False
        with open(ldir / f"{label}.stderr", "w") as err:
            child = subprocess.Popen(self.command(arm) + [str(cpath)], stdout=subprocess.PIPE, stderr=err, env=env, text=True)
            try:
                stdout, _ = child.communicate(timeout=timeout)
            except subprocess.TimeoutExpired:
                child.kill()
                stdout, _ = child.communicate()
                killed = True
        for line in stdout.splitlines():
            line = line.strip()
            if line.startswith("{"):
                try:
                    events.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        p = {"arm": arm["label"], "engine": arm["engine"], "opts": arm["opts"], "mode": mode, "label": label,
             "exit": child.returncode, "killed": killed, "wall_s": time.monotonic() - t0, "events": events}
        if killed or child.returncode != 0:
            log(f"{label}: exit {child.returncode}{' (killed)' if killed else ''}, see logs/{label}.stderr")
        self.procs.append(p)
        return p


def med(xs):
    xs = [x for x in xs if x is not None and x == x]
    return statistics.median(xs) if xs else None


def fmt(v, unit):
    if v is None:
        return "–"
    if unit == "us":
        return f"{v:,.1f}" if v < 1000 else f"{v:,.0f}"
    return f"{v:,.0f}"


def verdict(mine, theirs, higher_better):
    """Win, tie (within 10%) or loss of `mine` against `theirs`."""
    if mine is None or theirs is None or theirs == 0 or mine == 0:
        return ""
    r = mine / theirs if higher_better else theirs / mine
    if r >= 1.0:
        return f"win {r:.2f}x"
    if r >= 1 / 1.1:
        return f"tie {r:.2f}x"
    return f"loss {r:.2f}x"


def summary(args, meta, arms, procs, checks):
    res = {}
    for p in procs:
        if p["mode"] != "time":
            continue
        for e in p["events"]:
            if e.get("event") == "case" and e.get("status") == "ok":
                k = (p["arm"], e["case"], e["threads"])
                res.setdefault(k, {"p50": [], "ops": [], "p99": []})
                res[k]["p50"].append(e.get("p50_us"))
                res[k]["ops"].append(e.get("ops_per_s"))
                res[k]["p99"].append(e.get("p99_us"))
    labels = [a["label"] for a in arms]
    first = labels[0]
    ref = "tdb2" if "tdb2" in labels else None
    out = [f"# Realistic Jena work: {', '.join(labels)}", ""]
    out += [f"* Date: {meta['date']}, commit {meta['commit']}, host {meta['host']}, {meta['cpu']}",
            f"* Load average at the start: {' '.join(meta['loadavg'])}; CPUs {args.cpus or 'all'}, JVM processors {args.cores or 'all'}",
            f"* Data: {meta['triples']:,} triples ({args.people} people); small load file {meta['small_triples']:,} triples",
            f"* {args.rounds} timing processes per arm in A/B/B/A order, {args.warmup_seconds:g} s warm-up and {args.seconds:g} s measured per case and thread count; figures are medians over processes",
            "* Arms: " + "; ".join(f"`{a['label']}` = {a['engine']} {' '.join(a['opts'])}".rstrip() for a in arms), ""]
    bad = [(c, v) for c, v in checks.items() if v.get("differs")]
    if bad:
        out += ["**Answers differ** from the majority's in: " + ", ".join(f"{c} ({', '.join(v['differs'])})" for c, v in bad),
                "TDB2 writes inlined `xsd:decimal` literals in canonical form, so it differs alone where salaries appear.", ""]
    elif checks:
        out += ["Answers: every checked read case gives the same fingerprint in every arm.", ""]
    for threads, metric, unit, hb, title in [(1, "p50", "us", False, "One thread, median latency in µs (lower is better)"),
                                              (4, "ops", "ops", True, "Four threads, operations per second (higher is better)")]:
        out += [f"## {title}", ""]
        head = "| Case | " + " | ".join(labels) + (f" | {first} vs {ref} |" if ref and ref != first else " |")
        out += [head, "|---|" + "---:|" * len(labels) + ("---|" if ref and ref != first else "")]
        for case in CASES:
            if case not in args.case_list:
                continue
            t = 1 if case == "bulk-load" else threads
            if case == "bulk-load" and threads != 1:
                continue
            vals = {l: med(res.get((l, case, t), {}).get(metric, [])) for l in labels}
            if all(v is None for v in vals.values()):
                continue
            row = f"| {case} | " + " | ".join(fmt(vals[l], unit) for l in labels)
            if ref and ref != first:
                row += f" | {verdict(vals[first], vals[ref], hb)}"
            out.append(row + " |")
        out.append("")
    out += ["## Cases", ""] + [f"* `{c}`: {d}" for c, d in CASES.items() if c in args.case_list] + [""]
    return "\n".join(out), res


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--people", type=int, default=10000, help="dataset size in people (about 10.5 triples each)")
    ap.add_argument("--small-people", type=int, default=1000, help="people in the bulk-load file")
    ap.add_argument("--workdir", type=Path, default=ROOT / "target" / "bench-jena")
    ap.add_argument("--arm", action="append", default=[], help="LABEL=ENGINE[;JVM OPTION]..., repeatable; the first is compared with tdb2")
    ap.add_argument("--cases", default="", help="only these cases, comma-separated")
    ap.add_argument("--skip", default="", help="leave out these cases, comma-separated")
    ap.add_argument("--threads", default="1,4")
    ap.add_argument("--seconds", type=float, default=5)
    ap.add_argument("--warmup-seconds", type=float, default=2)
    ap.add_argument("--rounds", type=int, default=2, help="timing processes per arm")
    ap.add_argument("--check-ops", type=int, default=64, help="operations per case in the answer check")
    ap.add_argument("--no-check", action="store_true")
    ap.add_argument("--cpus", default=os.environ.get("BENCH_CPUS", ""), help="taskset CPU list, e.g. 8-15")
    ap.add_argument("--cores", type=int, default=int(os.environ.get("BENCH_CORES", "0") or 0))
    ap.add_argument("--no-build", action="store_true", help="use the built bindings as they are")
    ap.add_argument("--jvm-heap", default="8g")
    ap.add_argument("--jvm-opts", default="", help="JVM options for every arm")
    args = ap.parse_args()
    args.workdir = args.workdir.resolve()
    arms = [parse_arm(a) for a in (args.arm or ["sparkles=sparkles-mem", "sparkles-disk=sparkles", "tdb2=tdb2", "tim=tim"])]
    if len({a["label"] for a in arms}) != len(arms):
        raise SystemExit("arm labels must differ")
    if args.cpus and not args.cores:
        n = 0
        for part in args.cpus.split(","):
            a, _, b = part.partition("-")
            n += int(b or a) - int(a) + 1
        args.cores = n
    only = [c for c in args.cases.split(",") if c]
    unknown = [c for c in only if c not in CASES]
    if unknown:
        raise SystemExit(f"unknown cases: {', '.join(unknown)}")
    skip = set(c for c in args.skip.split(",") if c)
    args.case_list = [c for c in (only or CASES) if c not in skip]
    threads = [int(t) for t in args.threads.split(",") if t]
    work = args.workdir
    (work / "results").mkdir(parents=True, exist_ok=True)

    bench = bench_module()
    data = work / f"data-{args.people}.nt"
    small = work / f"data-{args.small_people}.nt"
    for path, n in ((data, args.people), (small, args.small_people)):
        if not path.exists():
            log(f"generating {path.name}")
            with open(str(path) + ".tmp", "w") as f:
                subprocess.run([sys.executable, ROOT / "scripts" / "gen-data.py", str(n)], stdout=f, check=True)
            os.replace(str(path) + ".tmp", path)
    count = lambda p: sum(1 for _ in open(p, "rb"))
    triples, small_triples = count(data), count(small)
    log(f"dataset: {triples:,} triples; bulk-load file: {small_triples:,}")
    _, probes = bench.subjects_and_probes(data, args.people)

    class SetupArgs:
        no_build = args.no_build
        workdir = work
    setup = bench.Setup(SetupArgs)
    setup.jvm()
    runner = Runner(args, setup.classpath)

    common = {"data": str(data), "small_data": str(small), "people": args.people, "orgs": max(10, args.people // 50),
              "probes": probes, "threads": threads, "seconds": args.seconds, "warmup_seconds": args.warmup_seconds,
              "check_ops": args.check_ops}
    stores = work / "stores"
    stores.mkdir(exist_ok=True)
    scratch = work / "scratch"

    def store_of(engine):
        return stores / f"{engine}-{args.people}"

    for engine in sorted({a["engine"] for a in arms} & DISK):
        d = store_of(engine)
        if not (d / ".loaded").exists():
            shutil.rmtree(d, ignore_errors=True)
            d.mkdir(parents=True)
            arm = next(a for a in arms if a["engine"] == engine)
            p = runner.launch(arm, "load", dict(common, store=str(d)), f"load-{engine}", 3600)
            if p["exit"] != 0:
                raise SystemExit(f"loading {engine} failed")
            (d / ".loaded").write_text("")

    def fresh_store(arm, label):
        """A copy of the loaded store for a process that may write, and a scratch directory."""
        extra = {"scratch": str(scratch / label)}
        (scratch / label).mkdir(parents=True, exist_ok=True)
        if arm["engine"] in DISK:
            d = scratch / label / "store"
            subprocess.run(["cp", "-a", str(store_of(arm["engine"])), str(d)], check=True)
            extra["store"] = str(d)
        return extra

    per_case = args.warmup_seconds + args.seconds + 5
    timeout = 600 + per_case * len(args.case_list) * len(threads) * 2

    checks = {}
    if not args.no_check:
        reads = [c for c in args.case_list if not c.startswith(("update-", "bulk-")) and c not in ("model-add-txn", "model-update-txn")]
        prints = {}
        for arm in arms:
            label = f"{arm['label']}-check"
            extra = fresh_store(arm, label)
            p = runner.launch(arm, "check", dict(common, cases=reads, **extra), label, timeout)
            shutil.rmtree(scratch / label, ignore_errors=True)
            for e in p["events"]:
                if e.get("event") == "check":
                    prints.setdefault(e["case"], {})[arm["label"]] = e.get("fingerprint") if e.get("status") == "ok" else f"error: {e.get('error')}"
        for case, fp in prints.items():
            # the majority's answer; TDB2 alone writes inlined decimals in canonical form
            # ("12345.50" as "12345.5"), so its answers differ where salaries appear
            counts = {}
            for v in fp.values():
                counts[v] = counts.get(v, 0) + 1
            majority = max(counts, key=lambda v: (counts[v], not str(v).startswith("error")))
            differs = [l for l, v in fp.items() if v != majority]
            checks[case] = {"fingerprints": fp, "differs": differs}
            if differs:
                log(f"check: {case} differs from the majority in {', '.join(differs)}")

    for rnd in range(args.rounds):
        order = arms if rnd % 2 == 0 else list(reversed(arms))
        for arm in order:
            label = f"{arm['label']}-r{rnd}"
            extra = fresh_store(arm, label)
            runner.launch(arm, "time", dict(common, cases=args.case_list, **extra), label, timeout)
            shutil.rmtree(scratch / label, ignore_errors=True)

    def git(*a):
        try:
            return subprocess.run(["git", *a], capture_output=True, text=True, cwd=ROOT).stdout.strip()
        except OSError:
            return os.environ.get("BENCH_COMMIT", "?")
    cpu = "?"
    try:
        cpu = re.search(r"model name\s*:\s*(.*)", Path("/proc/cpuinfo").read_text()).group(1)
    except (OSError, AttributeError):
        pass
    meta = {
        "date": datetime.datetime.now().isoformat(timespec="seconds"),
        "commit": git("rev-parse", "--short=12", "HEAD") + ("-dirty" if git("status", "--porcelain", "--untracked-files=no") else ""),
        "host": platform.node(),
        "cpu": f"{cpu} ({os.cpu_count()} logical CPUs)",
        "loadavg": Path("/proc/loadavg").read_text().split()[:3] if Path("/proc/loadavg").exists() else ["?"],
        "triples": triples,
        "small_triples": small_triples,
        "args": {k: (str(v) if isinstance(v, Path) else v) for k, v in vars(args).items()},
    }
    md, res = summary(args, meta, arms, runner.procs, checks)
    table = [{"arm": a, "case": c, "threads": t, **{k: med(v) for k, v in m.items()}} for (a, c, t), m in sorted(res.items())]
    raw = {"meta": meta, "arms": arms, "checks": checks, "results": table, "processes": runner.procs}
    json.dump(raw, open(work / "results" / "raw.json", "w"), indent=1)
    (work / "results" / "summary.md").write_text(md)
    log(f"wrote {work / 'results' / 'summary.md'} and raw.json")


if __name__ == "__main__":
    main()
