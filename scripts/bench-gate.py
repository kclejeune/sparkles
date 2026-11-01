#!/usr/bin/env python3
"""Gate records of the performance gates that guard engine rewrites (ROADMAP items 469-473).

    bench-gate.py executors OUT --out record.json      # scripts/bench-executors.sh results
    bench-gate.py metrics FILE.jsonl --gate NAME --out record.json [--manifest K=V ...]
    bench-gate.py w3c LOG --out record.json [--manifest K=V ...]   # test:w3c:streaming output
    bench-gate.py compare BASELINE CANDIDATE [--threshold F] [--floor V] [--keys REGEX]
    bench-gate.py catalog RECORD                       # add it to its workload's RUNS.json

A gate record is one JSON object:

    gate_record  1; other versions are refused
    gate         executors | incr | reasoning-latency | insn-screen | w3c-streaming
    suite, scale the workload and size, as in the common benchmark record
    kind         "gate-record"
    host         {name, cpu, cpus, kernel}
    manifest     the binary or build, its commit and hash, the scripts' commit and the window
    settings     how the run was made, including "threshold", the default allowed slowdown
    metrics      list of {key, value, unit, better: lower | higher, samples?}
    checks       list of {name, ok, detail}: answers and other conditions that must hold

`metrics` reads JSON lines of {key, value, unit, better, samples?} or {check, ok, detail}
that a Rust harness wrote, such as `incr_bench.rs` with INCR_BENCH_JSON or the
instruction-count screen with INSN_SCREEN_JSON.

`w3c` reads the per-suite summaries of `mise run test:w3c:streaming`: for each suite the
passed tests (more is better), the queries that needed eager fallback (fewer is better)
and a check that no test failed. Its threshold is 0, so any change in the wrong
direction is flagged.

`compare` prints a Markdown table with each metric of the baseline, the candidate and
their ratio, oriented so that a ratio above 1 is a slowdown or a loss. It exits with
status 1 when a ratio reaches 1 + threshold, when the candidate lacks a metric of the
baseline, or when a check of the candidate fails. The threshold comes from the
baseline's settings unless --threshold is given. --floor V ignores metrics whose
baseline value is below V, for timings too short for the gate's host. A comparison
across runs on different days carries the host's run-to-run noise, so a flagged metric
is confirmed by an interleaved A/B of the two builds before it counts as a regression,
as docs/BENCHMARKING.md describes for each gate.

`catalog` appends the record to the RUNS.json of the workload directory that holds it,
with the same run ID, hash and context rules as scripts/bench-docs.py.
"""

import argparse
import csv
import hashlib
import importlib.util
import json
import math
import os
import platform
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

VERSION = 1
GATES = {"executors", "incr", "reasoning-latency", "insn-screen", "w3c-streaming"}


def fail(message):
    print(f"bench-gate: {message}", file=sys.stderr)
    sys.exit(2)


def host_info():
    cpu = ""
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        pass
    return {"name": platform.node().split(".")[0], "cpu": cpu, "cpus": os.cpu_count(),
            "kernel": platform.release()}


def read_record(path):
    record = json.loads(Path(path).read_text())
    if record.get("gate_record") != VERSION:
        fail(f"{path}: unknown gate record version {record.get('gate_record')!r}")
    if record.get("gate") not in GATES:
        fail(f"{path}: unknown gate {record.get('gate')!r}")
    return record


def write_record(record, out):
    validate(record)
    Path(out).write_text(json.dumps(record, indent=1, sort_keys=True) + "\n")


def validate(record):
    keys = set()
    for m in record["metrics"]:
        if m["key"] in keys:
            fail(f"duplicate metric {m['key']}")
        keys.add(m["key"])
        if m["better"] not in ("lower", "higher"):
            fail(f"{m['key']}: better must be lower or higher")
        if not isinstance(m["value"], (int, float)) or not math.isfinite(m["value"]):
            fail(f"{m['key']}: value must be a finite number")
    for c in record["checks"]:
        if not isinstance(c.get("ok"), bool):
            fail(f"check {c.get('name')}: ok must be true or false")


def parse_kv(pairs):
    out = {}
    for p in pairs or []:
        k, sep, v = p.partition("=")
        if not sep:
            fail(f"--manifest takes KEY=VALUE, not {p!r}")
        out[k] = v
    return out


# ------------------------------------------------------------------ executors (item 469)
# Each query of each cell gives four metrics, `<cell>/<eager|streaming>/<query>/<exec|wall>`.
# A streamed answer's execMs counts the time spent making batches and leaves out the time
# spent writing them, so `wall` is the end-to-end measure for large answers.

def read_tsv(path):
    if not path.exists():
        return []
    with path.open() as f:
        return list(csv.reader(f, delimiter="\t"))


def manifest_txt(path):
    out = {}
    if path.exists():
        for line in path.read_text().splitlines():
            k, sep, v = line.partition(": ")
            if sep:
                out[k] = v
    return out


MODES = {"A": "eager", "B": "streaming"}


def executors(args):
    root = Path(args.dir)
    manifest = manifest_txt(root / "manifest.txt")
    cells = sorted(d for d in root.iterdir() if d.is_dir() and (d / "results" / "samples.tsv").exists())
    if not cells:
        fail(f"no cells with results/samples.tsv under {root}")
    metrics, checks, tables, env = [], [], [], {}
    for d in cells:
        m = re.fullmatch(r"people-(\d+)-threads-(\d+)", d.name)
        if not m:
            fail(f"unexpected cell directory {d.name}")
        people, threads = int(m.group(1)), int(m.group(2))
        cell = f"{people * 105 / 10 / 1e6:g}m-{threads}t"
        rows = read_tsv(d / "results" / "samples.tsv")
        col = {name: i for i, name in enumerate(rows[0])}
        samples = defaultdict(lambda: defaultdict(list))
        errors = defaultdict(int)
        order = []
        for r in rows[1:]:
            q, v = r[col["query"]], r[col["variant"]]
            if q not in order:
                order.append(q)
            if r[col["exec_ms"]] == "error":
                errors[(q, v)] += 1
                continue
            samples[q][v].append((float(r[col["wall_ms"]]), float(r[col["exec_ms"]])))
        answers = defaultdict(dict)
        for v, q, n, h in read_tsv(d / "results" / "answers.tsv"):
            answers[q][v] = (n, h)
        table = []
        for q in order:
            agree = len(set(answers[q].values())) == 1 and "error" not in str(answers[q])
            checks.append({"name": f"{cell}/{q}/answers", "ok": agree,
                           "detail": {MODES[v]: list(a) for v, a in sorted(answers[q].items())}})
            for v in ("A", "B"):
                if errors[(q, v)]:
                    checks.append({"name": f"{cell}/{q}/{MODES[v]}/errors", "ok": False,
                                   "detail": f"{errors[(q, v)]} failed timed requests"})
            med = {}
            for v in ("A", "B"):
                xs = samples[q][v]
                if not xs:
                    continue
                wall = [x[0] for x in xs]
                exe = [x[1] for x in xs]
                med[v] = (statistics.median(wall), statistics.median(exe))
                for name, values in (("exec", exe), ("wall", wall)):
                    metrics.append({"key": f"{cell}/{MODES[v]}/{q}/{name}",
                                    "value": round(statistics.median(values), 4),
                                    "unit": "ms", "better": "lower",
                                    "samples": [round(x, 4) for x in values]})
            table.append((q, med.get("A"), med.get("B"), agree))
        tables.append((cell, threads, table))
        env[cell] = (d / "results" / "env.txt").read_text().splitlines() if (d / "results" / "env.txt").exists() else []
    run_env = manifest_txt(root / "manifest.txt")
    record = {
        "gate_record": VERSION, "gate": "executors", "kind": "gate-record",
        "suite": "synthetic", "scale": "1.05m and 10.5m",
        "host": host_info() | {"name": run_env.get("host", host_info()["name"])},
        "manifest": {"binary": manifest.get("binary"), "binary_sha256": manifest.get("binary sha256"),
                     "binary_commit": manifest.get("binary commit"), "version": manifest.get("version"),
                     "scripts_commit": manifest.get("scripts commit"),
                     "scripts_dirty_files": manifest.get("scripts dirty"),
                     "started": manifest.get("started"), "ended": manifest.get("ended"),
                     "load_before": manifest.get("load before"), "load_after": manifest.get("load after"),
                     "cells": env},
        "settings": {"threshold": 0.05,
                     "metric": "medians per request of the server's execMs (/exec) and of curl's time_total (/wall)",
                     "variants": manifest.get("variants"), "run": manifest.get("settings"),
                     "harness": "scripts/bench-executors.sh with scripts/nsc-bench-remote.sh"},
        "metrics": metrics, "checks": checks,
    }
    write_record(record, args.out)
    print("# Eager-against-cursor matrix")
    print()
    for k in ("host", "binary commit", "binary sha256", "started", "ended", "settings"):
        if k in manifest:
            print(f"- {k}: {manifest[k]}")
    failed = [c["name"] for c in checks if not c["ok"]]
    print()
    print("Every cell's eager and streaming answers agree." if not failed
          else "Failed checks: " + ", ".join(failed) + ".")
    for cell, threads, table in tables:
        print()
        print(f"## {cell}: {cell.split('-')[0].upper()} triples, {threads} thread{'s' if threads > 1 else ''}")
        print()
        print("| query | eager ms | streaming ms | streaming / eager | eager exec ms | streaming exec ms |")
        print("|---|--:|--:|--:|--:|--:|")
        ratios = []
        for q, a, b, agree in table:
            if a and b:
                ratios.append(b[0] / a[0])
                print(f"| {q}{'' if agree else ' (answers differ)'} | {a[0]:.3f} | {b[0]:.3f} | {b[0] / a[0]:.3f} | {a[1]:.3f} | {b[1]:.3f} |")
            else:
                print(f"| {q} | {'error' if not a else f'{a[0]:.3f}'} | {'error' if not b else f'{b[0]:.3f}'} | | | |")
        if ratios:
            g = math.exp(sum(map(math.log, ratios)) / len(ratios))
            print(f"| **geometric mean** | | | {g:.3f} | | |")
    print()
    print("Times are medians over every timed request of the client's wall time (curl time_total) and of the "
          "server's execMs. The ratio is of the wall times.")


# ----------------------------------------------------------------------- metrics (JSONL)

def metrics(args):
    ms, checks = [], []
    for n, line in enumerate(Path(args.file).read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line)
        if "check" in row:
            checks.append({"name": row["check"], "ok": row["ok"], "detail": row.get("detail")})
        else:
            for k in ("key", "value", "unit", "better"):
                if k not in row:
                    fail(f"{args.file}:{n}: a metric needs {k}")
            ms.append(row)
    extra = parse_kv(args.manifest)
    settings = parse_kv(args.setting)
    settings["threshold"] = args.threshold
    record = {"gate_record": VERSION, "gate": args.gate, "kind": "gate-record",
              "suite": args.suite or args.gate, "scale": args.scale, "host": host_info(),
              "manifest": extra, "settings": settings, "metrics": ms, "checks": checks}
    write_record(record, args.out)
    print(f"{len(ms)} metrics and {len(checks)} checks into {args.out}")


# ------------------------------------------------------------------- w3c (item 470)

def w3c(args):
    text = Path(args.log).read_text()
    ms, checks = [], []
    for name, passed, failed, skipped in re.findall(
            r"^(.+): (\d+) passed, (\d+) failed, (\d+) skipped", text, re.M):
        ms.append({"key": f"{name}/passed", "value": int(passed), "unit": "tests", "better": "higher"})
        checks.append({"name": f"{name}/failed", "ok": failed == "0",
                       "detail": f"{passed} passed, {failed} failed, {skipped} skipped"})
    for name, n in re.findall(r"^(.+): (\d+) queries needed eager fallback", text, re.M):
        ms.append({"key": f"{name}/fallbacks", "value": int(n), "unit": "queries", "better": "lower"})
    if not ms:
        fail(f"no suite summaries in {args.log}")
    ok = re.search(r"^test result: ok\.", text, re.M) is not None
    checks.append({"name": "test result", "ok": ok, "detail": "the test binary reported ok" if ok else "no ok result"})
    record = {"gate_record": VERSION, "gate": "w3c-streaming", "kind": "gate-record",
              "suite": "w3c-sparql", "scale": None, "host": host_info(),
              "manifest": parse_kv(args.manifest),
              "settings": {"threshold": 0.0, "execution": "streaming", "batch_rows": 4096,
                           "harness": "crates/sparkles-core/tests/w3c.rs, the streaming_ tests"},
              "metrics": ms, "checks": checks}
    write_record(record, args.out)
    print(f"{len(ms)} metrics and {len(checks)} checks into {args.out}")


# ------------------------------------------------------------------------------ compare

def compare(args):
    base, cand = read_record(args.baseline), read_record(args.candidate)
    if base["gate"] != cand["gate"]:
        fail(f"gates differ: {base['gate']} and {cand['gate']}")
    threshold = args.threshold if args.threshold is not None else float(base["settings"].get("threshold", 0.05))
    pattern = re.compile(args.keys) if args.keys else None
    cm = {m["key"]: m for m in cand["metrics"]}
    rows, bad = [], []
    for m in base["metrics"]:
        key = m["key"]
        if pattern and not pattern.search(key):
            continue
        if args.floor is not None and m["value"] < args.floor:
            continue
        c = cm.get(key)
        if c is None:
            rows.append((key, m["value"], None, None, "missing"))
            bad.append(key)
            continue
        if m["value"] == 0 or c["value"] == 0:
            ratio = 1.0 if m["value"] == c["value"] else math.inf
        elif m["better"] == "lower":
            ratio = c["value"] / m["value"]
        else:
            ratio = m["value"] / c["value"]
        flag = "slower" if ratio > 1 and ratio >= 1 + threshold else ""
        if flag:
            bad.append(key)
        rows.append((key, m["value"], c["value"], ratio, flag))
    failed = [c for c in cand["checks"] if not c["ok"]]
    print(f"# {base['gate']} gate: candidate against baseline")
    print()
    for label, r in (("baseline", base), ("candidate", cand)):
        mf = r.get("manifest", {})
        print(f"- {label}: host {r['host'].get('name')}, commit {mf.get('binary_commit') or mf.get('commit')}, "
              f"started {mf.get('started')}")
    print(f"- threshold: {threshold:.1%} slower{f', floor {args.floor}' if args.floor is not None else ''}")
    print()
    print("| metric | baseline | candidate | ratio | flag |")
    print("|---|--:|--:|--:|---|")
    fmt = lambda v: "" if v is None else f"{v:.6g}"
    for key, b, c, ratio, flag in rows:
        print(f"| {key} | {fmt(b)} | {fmt(c)} | {'' if ratio is None else f'{ratio:.3f}'} | {flag} |")
    print()
    if failed:
        print("Failed candidate checks: " + ", ".join(c["name"] for c in failed) + ".")
    print(f"{len(rows)} metrics compared, {len(bad)} flagged, {len(failed)} failed checks.")
    sys.exit(1 if bad or failed else 0)


# ------------------------------------------------------------------------------ catalog

def catalog(args):
    path = Path(args.record).resolve()
    root = Path(__file__).resolve().parent.parent
    docs = (root / "docs" / "benchmarks").resolve()
    rel = path.relative_to(docs)
    workload = docs / rel.parts[0]
    spec = importlib.util.spec_from_file_location("bench_docs", root / "scripts" / "bench-docs.py")
    bd = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(bd)
    raw = path.read_bytes()
    record = json.loads(raw)
    digest = hashlib.sha256(raw).hexdigest()
    runs = workload / "RUNS.json"
    cat = json.loads(runs.read_text())
    if any(e["sha256"] == digest for e in cat["sources"]):
        print(f"{rel} is already in {runs.relative_to(root)}")
        return
    cat["sources"].append({
        "run_id": "sha256-" + digest, "path": str(path.relative_to(workload)), "sha256": digest,
        "format": "legacy-assessment", "context": bd.context_of(record),
        "metadata_note": "A gate record of scripts/bench-gate.py. Read the record for its "
                         "manifest, settings, metrics and checks.",
    })
    runs.write_text(json.dumps(cat, indent=2, ensure_ascii=False) + "\n")
    print(f"added {rel} to {runs.relative_to(root)}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("executors")
    p.add_argument("dir")
    p.add_argument("--out", required=True)
    p = sub.add_parser("metrics")
    p.add_argument("file")
    p.add_argument("--gate", required=True, choices=sorted(GATES))
    p.add_argument("--out", required=True)
    p.add_argument("--suite")
    p.add_argument("--scale")
    p.add_argument("--threshold", type=float, default=0.05)
    p.add_argument("--manifest", action="append", metavar="KEY=VALUE")
    p.add_argument("--setting", action="append", metavar="KEY=VALUE")
    p = sub.add_parser("w3c")
    p.add_argument("log")
    p.add_argument("--out", required=True)
    p.add_argument("--manifest", action="append", metavar="KEY=VALUE")
    p = sub.add_parser("compare")
    p.add_argument("baseline")
    p.add_argument("candidate")
    p.add_argument("--threshold", type=float)
    p.add_argument("--floor", type=float)
    p.add_argument("--keys")
    p = sub.add_parser("catalog")
    p.add_argument("record")
    args = ap.parse_args()
    {"executors": executors, "metrics": metrics, "w3c": w3c, "compare": compare, "catalog": catalog}[args.cmd](args)


if __name__ == "__main__":
    main()
