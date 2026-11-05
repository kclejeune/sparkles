#!/usr/bin/env python3
"""Gate records of the performance gates that guard engine rewrites (ROADMAP items 469-473).

    bench-gate.py executors OUT --out record.json      # scripts/bench-executors.sh results
    bench-gate.py metrics FILE.jsonl... --gate NAME --out record.json [--manifest K=V ...]
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
    checks       list of {name, ok, detail, fingerprint?}: answers and other conditions
                 that must hold; a fingerprint, such as an answer's row count and hash,
                 must also equal the baseline's

`metrics` reads JSON lines of {key, value, unit, better, samples?} or {check, ok, detail}
that a Rust harness wrote, such as `incr_bench.rs` with INCR_BENCH_JSON or the
instruction-count screen with INSN_SCREEN_JSON. Several files are runs of the same
harness in separate processes: a metric's value is then the median of the runs and its
`range` the smallest and largest, and a check must hold in every run with one
fingerprint.

`w3c` reads the per-suite summaries of `mise run test:w3c:streaming`: for each suite the
passed tests (more is better), the queries that needed eager fallback (fewer is better)
and a check that no test failed. Its threshold is 0, so any change in the wrong
direction is flagged.

`compare` prints a Markdown table with each metric of the baseline, the candidate and
their ratio, oriented so that a ratio above 1 is a slowdown or a loss. It exits with
status 1 when a ratio above 1 reaches 1 + threshold, when the candidate lacks a metric or a check of
the baseline, when a check of the candidate fails or when a fingerprint differs from the
baseline's. The threshold comes from the baseline's settings unless --threshold is
given. When both records hold a metric's range over several processes, a slowdown
counts only if the ranges do not overlap. A baseline's `short_below` and `short_threshold` settings give timings below
that many milliseconds their own threshold. It refuses two records whose suite, scale,
workload settings, host, build (the manifest's compilers and build configuration) or
metric units differ, or whose build is unknown, unless --allow-different is given; a
record whose settings say `host_independent` may come from another host and build. --floor V ignores metrics whose
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
    record = json.loads(Path(path).read_text(), parse_constant=lambda c: fail(f"{path}: {c} is not a number"))
    if record.get("gate_record") != VERSION:
        fail(f"{path}: unknown gate record version {record.get('gate_record')!r}")
    if record.get("gate") not in GATES:
        fail(f"{path}: unknown gate {record.get('gate')!r}")
    validate(record, path)
    return record


def write_record(record, out):
    validate(record)
    Path(out).write_text(json.dumps(record, indent=1, sort_keys=True) + "\n")


def finite(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)


def validate(record, where="record"):
    for k in ("host", "manifest", "settings", "metrics", "checks"):
        if k not in record:
            fail(f"{where}: no {k}")
    keys = set()
    for m in record["metrics"]:
        for k in ("key", "value", "unit", "better"):
            if k not in m:
                fail(f"{where}: a metric has no {k}")
        if m["key"] in keys:
            fail(f"{where}: duplicate metric {m['key']}")
        keys.add(m["key"])
        if m["better"] not in ("lower", "higher"):
            fail(f"{where}: {m['key']}: better must be lower or higher")
        if not finite(m["value"]):
            fail(f"{where}: {m['key']}: value must be a finite number")
        if "range" in m:
            r = m["range"]
            if not (isinstance(r, list) and len(r) == 2 and all(map(finite, r)) and r[0] <= m["value"] <= r[1]):
                fail(f"{where}: {m['key']}: range must be two finite numbers around the value")
        if not all(map(finite, m.get("samples", []))):
            fail(f"{where}: {m['key']}: samples must be finite numbers")
    names = set()
    for c in record["checks"]:
        if c.get("name") in names:
            fail(f"{where}: duplicate check {c.get('name')}")
        names.add(c.get("name"))
        if not isinstance(c.get("ok"), bool):
            fail(f"{where}: check {c.get('name')}: ok must be true or false")


def compilers(path):
    """The compiler identities in an ELF binary's .comment section, such as rustc's and
    GCC's for the C code it links, sorted and joined by "; ", or None."""
    import struct
    try:
        data = Path(path).read_bytes()
    except OSError:
        return None
    if data[:4] != b"\x7fELF" or data[4] != 2:
        return None
    end = "<" if data[5] == 1 else ">"
    shoff, = struct.unpack_from(end + "Q", data, 0x28)
    shentsize, shnum, shstrndx = struct.unpack_from(end + "HHH", data, 0x3A)
    def section(i):
        name, _, _, _, off, size = struct.unpack_from(end + "IIQQQQ", data, shoff + i * shentsize)
        return name, off, size
    _, stroff, _ = section(shstrndx)
    for i in range(shnum):
        name, off, size = section(i)
        if data[stroff + name:data.index(b"\0", stroff + name)] == b".comment":
            parts = {p.decode(errors="replace").strip() for p in data[off:off + size].split(b"\0")}
            return "; ".join(sorted(p for p in parts if p)) or None
    return None


def runtime_env(environ=None):
    """The SPARKLES_ environment variables that switch the engine's behaviour at run time,
    such as SPARKLES_DISABLE_OPTIMIZATIONS, as a dict, which a record keeps in its settings
    only when there are any."""
    environ = os.environ if environ is None else environ
    return {k: v for k, v in sorted(environ.items()) if k.startswith("SPARKLES_")}


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
    unordered = set()
    for f in sorted((root / "queries").glob("*.rq")):
        text = f.read_text().upper()
        if re.search(r"\bLIMIT\b", text) and not re.search(r"\bORDER\s+BY\b", text):
            unordered.add(f.stem)
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
        for row in read_tsv(d / "results" / "answers.tsv"):
            # variant, query, rows, digest of the sorted rows and, when the run had
            # ORDER_DIGEST, a digest of the ORDER BY keys in the returned order
            v, q, n, h = row[:4]
            answers[q][v] = (n, h) + tuple(o for o in row[4:5] if o != "-")
            if q in unordered:
                # rows may differ, but not their number or how many are distinct
                answers[q][v] = (n,) + tuple(o for o in row[4:5] if o.startswith("distinct:"))
        # the timed format's answer after each server process's timed requests
        warm = defaultdict(set)
        for row in read_tsv(d / "results" / "warm-answers.tsv"):
            # epoch, variant, query, [timed repetition,] digest
            v, q, digest = row[1], row[2], row[-1]
            # unordered LIMIT answers may hold other rows, but not another count, schema or
            # number of distinct rows
            warm[q].add(",".join(digest.split(",")[:3]) if q in unordered else digest)
        table = []
        for q in order:
            # LIMIT without ORDER BY may return other rows in each mode, so only the
            # counts must agree
            got = dict(answers[q])
            agree = set(got) == {"A", "B"} and len(set(got.values())) == 1 and "error" not in str(answers[q])
            # rows that are not triples of the input are wrong in any mode
            agree = agree and not re.search(r"missing:(?!0\b)\d+", str(answers[q]))
            eager = answers[q].get("A", ("error", "error"))
            # LIMIT without ORDER BY may return other rows, so only the count is kept
            fingerprint = ", ".join([f"{eager[0]} rows"] + list(eager[1:])) if q in unordered else ", ".join(
                [f"{eager[0]} rows", eager[1]] + [o if o.startswith("distinct:") else f"order {o}" for o in eager[2:]])
            checks.append({"name": f"{cell}/{q}/answers", "ok": agree, "fingerprint": fingerprint,
                           "detail": {MODES[v]: list(a) for v, a in sorted(answers[q].items())}})
            if warm:
                digests = warm.get(q, set())
                rows = answers[q].get("A", ("error",))[0]
                one = next(iter(digests)) if len(digests) == 1 else ""
                ok = len(digests) == 1 and (one.startswith("ask ") or one.split(" ")[0] == rows) \
                    and not re.search(r"missing (?!0\b)\d+", one)
                checks.append({"name": f"{cell}/{q}/timed-answers", "ok": ok,
                               "fingerprint": next(iter(digests)) if len(digests) == 1 else None,
                               "detail": sorted(digests)})
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
    # the dataset each cell served, by its file's digest
    data_digests = {}
    for d in cells:
        m = manifest_txt(d / "manifest.txt")
        data_digests[d.name.split("-threads-")[0]] = m.get("data sha256")
    comp = manifest.get("compilers") or compilers(manifest.get("binary") or "")
    record = {
        "gate_record": VERSION, "gate": "executors", "kind": "gate-record",
        "suite": "synthetic", "scale": "1.05m and 10.5m",
        "host": host_info() | {"name": run_env.get("host", host_info()["name"])},
        "manifest": {"binary": manifest.get("binary"), "binary_sha256": manifest.get("binary sha256"),
                     "rustc": manifest.get("rustc"), "compilers": comp,
                     "build_config": manifest.get("build config"),
                     "binary_commit": manifest.get("binary commit"), "version": manifest.get("version"),
                     "scripts_commit": manifest.get("scripts commit"),
                     "scripts_dirty_files": manifest.get("scripts dirty"),
                     "started": manifest.get("started"), "ended": manifest.get("ended"),
                     "load_before": manifest.get("load before"), "load_after": manifest.get("load after"),
                     "cells": env},
        "settings": {"threshold": 0.05, "short_below": 0.3, "short_threshold": 0.0,
                     "queries_sha256": corpus_digest(root / "queries"),
                     "generator_sha256": manifest.get("generator sha256"),
                     "data_sha256": data_digests,
                     "runtime_env": manifest.get("runtime env"),
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


def corpus_digest(qdir):
    """A digest of every query file's name and text, in name order."""
    h = hashlib.sha256()
    files = sorted(Path(qdir).glob("*.rq"))
    if not files:
        return None
    for f in files:
        h.update(f.name.encode() + b"\0" + f.read_bytes() + b"\0")
    return h.hexdigest()


# ----------------------------------------------------------------------- metrics (JSONL)

def read_lines(path):
    """The metrics and checks of one JSON-lines file."""
    ms, checks = [], []
    for n, line in enumerate(Path(path).read_text().splitlines(), 1):
        if not line.strip():
            continue
        row = json.loads(line, parse_constant=lambda c: fail(f"{path}:{n}: {c} is not a number"))
        if "check" in row:
            if not isinstance(row.get("ok"), bool):
                fail(f"{path}:{n}: a check's ok must be true or false")
            check = {"name": row["check"], "ok": row["ok"], "detail": row.get("detail")}
            if "fingerprint" in row:
                check["fingerprint"] = row["fingerprint"]
            checks.append(check)
        else:
            for k in ("key", "value", "unit", "better"):
                if k not in row:
                    fail(f"{path}:{n}: a metric needs {k}")
            if row["better"] not in ("lower", "higher") or not finite(row["value"]) \
                    or not all(map(finite, row.get("samples", []))):
                fail(f"{path}:{n}: a metric needs a finite value and better lower or higher")
            ms.append(row)
    return ms, checks


def merge_processes(runs):
    """Combine the runs of several processes: each metric's value is the median of the
    processes' values and `range` their smallest and largest; a check holds when it held
    in every process with one fingerprint."""
    ms, checks = {}, {}
    for i, (run_ms, run_checks) in enumerate(runs, 1):
        seen = set()
        for m in run_ms:
            if m["key"] in seen:
                fail(f"metric {m['key']} occurs twice in run {i}")
            seen.add(m["key"])
            ms.setdefault(m["key"], []).append(m)
        seen = set()
        for c in run_checks:
            if c["name"] in seen:
                fail(f"check {c['name']} occurs twice in run {i}")
            seen.add(c["name"])
            checks.setdefault(c["name"], []).append(c)
    out_ms = []
    for key, rows in ms.items():
        if len(rows) != len(runs):
            fail(f"metric {key} is in {len(rows)} of {len(runs)} runs")
        if len({(r["unit"], r["better"]) for r in rows}) != 1:
            fail(f"metric {key} has different units or directions in different runs")
        if not all(finite(r["value"]) for r in rows):
            fail(f"metric {key} has a value that is not a finite number")
        values = [r["value"] for r in rows]
        m = dict(rows[0])
        m["value"] = statistics.median(values)
        m["range"] = [min(values), max(values)]
        m["samples"] = [x for r in rows for x in r.get("samples", [r["value"]])]
        out_ms.append(m)
    out_checks = []
    for name, rows in checks.items():
        c = dict(rows[0])
        fps = {r.get("fingerprint") for r in rows}
        c["ok"] = all(r["ok"] for r in rows) and len(rows) == len(runs) and len(fps) == 1
        if not c["ok"]:
            c["detail"] = [r.get("detail") for r in rows]
        out_checks.append(c)
    return out_ms, out_checks


def metrics(args):
    runs = [read_lines(f) for f in args.file]
    ms, checks = runs[0] if len(runs) == 1 else merge_processes(runs)
    extra = parse_kv(args.manifest)
    for b in args.binary or []:
        # the harness binaries' compilers, which compare requires to match
        extra.setdefault("binary_sha256", hashlib.sha256(Path(b).read_bytes()).hexdigest())
        comp = compilers(b)
        if comp is None:
            fail(f"{b}: no compiler identities in its .comment section")
        if extra.get("compilers") not in (None, comp):
            fail(f"the binaries were built by different compilers: {extra['compilers']} and {comp}")
        extra["compilers"] = comp
    settings = parse_kv(args.setting)
    settings["threshold"] = args.threshold
    # the harness ran in this environment, as the gate tasks run it
    if runtime_env():
        settings["runtime_env"] = runtime_env()
    if len(runs) > 1:
        settings["processes"] = len(runs)
    record = {"gate_record": VERSION, "gate": args.gate, "kind": "gate-record",
              "suite": args.suite or args.gate, "scale": args.scale, "host": host_info(),
              "manifest": extra, "settings": settings, "metrics": ms, "checks": checks}
    write_record(record, args.out)
    print(f"{len(ms)} metrics and {len(checks)} checks from {len(runs)} runs into {args.out}")


# ------------------------------------------------------------------- w3c (item 470)

def w3c(args):
    text = Path(args.log).read_text()
    ms, checks = [], []
    # a suite's failures that are on its known-failure list fail in eager execution too,
    # so only the unexpected ones fail the check
    unexpected = {name: (int(k), int(u)) for name, k, u in re.findall(
        r"^(.+): (\d+) known failures, (\d+) unexpected failures", text, re.M)}
    for name, passed, failed, skipped in re.findall(
            r"^(.+): (\d+) passed, (\d+) failed, (\d+) skipped", text, re.M):
        ms.append({"key": f"{name}/passed", "value": int(passed), "unit": "tests", "better": "higher"})
        known, new = unexpected.get(name, (0, int(failed)))
        detail = f"{passed} passed, {failed} failed, {skipped} skipped"
        if known:
            detail += f", {known} of the failures on the suite's known-failure list"
        checks.append({"name": f"{name}/failed", "ok": new == 0, "detail": detail})
    for name, n in re.findall(r"^(.+): (\d+) queries needed eager fallback", text, re.M):
        ms.append({"key": f"{name}/fallbacks", "value": int(n), "unit": "queries", "better": "lower"})
    if not ms:
        fail(f"no suite summaries in {args.log}")
    ok = re.search(r"^test result: ok\.", text, re.M) is not None
    checks.append({"name": "test result", "ok": ok, "detail": "the test binary reported ok" if ok else "no ok result"})
    record = {"gate_record": VERSION, "gate": "w3c-streaming", "kind": "gate-record",
              "suite": "w3c-sparql", "scale": None, "host": host_info(),
              "manifest": parse_kv(args.manifest),
              "settings": {"threshold": 0.0, "host_independent": True, "execution": "streaming", "batch_rows": 4096,
                           "harness": "crates/sparkles-core/tests/w3c.rs, the streaming_ tests",
                           **({"runtime_env": runtime_env()} if runtime_env() else {})},
              "metrics": ms, "checks": checks}
    write_record(record, args.out)
    print(f"{len(ms)} metrics and {len(checks)} checks into {args.out}")


# ------------------------------------------------------------------------------ compare

# Settings that describe a run's circumstances rather than its workload.
VOLATILE_SETTINGS = {"threshold", "short_below", "short_threshold", "load", "host_independent"}
# Manifest fields of the build that must match, since another compiler or code generation
# setting can hide or make a regression. The source revision may differ.
# `compilers` comes from the binaries themselves and `build_config` from
# `bench-gate.py build-config` in the build's environment; both must be known in both
# records.
TOOLCHAIN = ("compilers", "build_config")


def incompatibilities(base, cand):
    """Why two records do not measure the same workload, as a list of reasons."""
    out = []
    for k in ("gate", "suite", "scale"):
        if base.get(k) != cand.get(k):
            out.append(f"{k}: {base.get(k)!r} against {cand.get(k)!r}")
    bs = {k: v for k, v in base["settings"].items() if k not in VOLATILE_SETTINGS}
    cs = {k: v for k, v in cand["settings"].items() if k not in VOLATILE_SETTINGS}
    for k in sorted(set(bs) | set(cs)):
        if bs.get(k) != cs.get(k):
            out.append(f"setting {k}: {bs.get(k)!r} against {cs.get(k)!r}")
    if not base["settings"].get("host_independent"):
        if base["host"].get("name") != cand["host"].get("name"):
            out.append(f"host: {base['host'].get('name')!r} against {cand['host'].get('name')!r}")
        bm, cmf = base.get("manifest", {}), cand.get("manifest", {})
        for k in TOOLCHAIN:
            unknown = [n for n, m in (("the baseline", bm), ("the candidate", cmf)) if not m.get(k)]
            if unknown:
                out.append(f"manifest {k}: unknown in " + " and ".join(unknown))
            elif bm[k] != cmf[k]:
                out.append(f"manifest {k}: {bm[k]!r} against {cmf[k]!r}")
    cm = {m["key"]: m for m in cand["metrics"]}
    for m in base["metrics"]:
        c = cm.get(m["key"])
        if c and (c["unit"] != m["unit"] or c["better"] != m["better"]):
            out.append(f"metric {m['key']}: {m['unit']}/{m['better']} against {c['unit']}/{c['better']}")
    return out


def loss_ratio(base, cand, better):
    """The ratio of the two values, oriented so that above 1 is a loss."""
    if base == cand:
        return 1.0
    worse = cand > base if better == "lower" else cand < base
    if base == 0 or cand == 0:
        return math.inf if worse else 0.0
    return cand / base if better == "lower" else base / cand


def compare(args):
    base, cand = read_record(args.baseline), read_record(args.candidate)
    if base["gate"] != cand["gate"]:
        fail(f"gates differ: {base['gate']} and {cand['gate']}")
    failed_base = [c["name"] for c in base["checks"] if not c["ok"]]
    if failed_base:
        fail(f"the baseline has failed checks and cannot be compared with: {', '.join(failed_base[:10])}")
    reasons = incompatibilities(base, cand)
    if reasons and not args.allow_different:
        fail("the records measure different workloads (--allow-different compares them anyway): "
             + "; ".join(reasons))
    settings = base["settings"]
    threshold = args.threshold if args.threshold is not None else float(settings.get("threshold", 0.05))
    # X14: a query under 0.3 ms on an Intel host may not be slower at all
    short_below = settings.get("short_below")
    short_threshold = float(settings.get("short_threshold", threshold))
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
        ratio = loss_ratio(m["value"], c["value"], m["better"])
        short = short_below is not None and m["unit"] == "ms" and m["value"] < float(short_below)
        limit = short_threshold if short else threshold
        flag = ("slower, short" if short else "slower") if ratio > 1 and ratio >= 1 + limit else ""
        # values from several processes count only when their ranges do not overlap
        if flag and "range" in m and "range" in c:
            lo, hi = m["range"]
            clo, chi = c["range"]
            if (m["better"] == "lower" and clo <= hi) or (m["better"] == "higher" and chi >= lo):
                flag = ""
        if flag:
            bad.append(key)
        rows.append((key, m["value"], c["value"], ratio, flag))
    failed = [c for c in cand["checks"] if not c["ok"]]
    # every check of the baseline must be in the candidate, and fingerprints such as
    # answers must match the baseline's, whatever the candidate's own checks say
    cc = {c["name"]: c for c in cand["checks"]}
    changed, missing = [], []
    # --keys selects metrics only, so every check of the baseline still counts
    for c in base["checks"]:
        other = cc.get(c["name"])
        if other is None:
            missing.append(c["name"])
        elif "fingerprint" in c and other.get("fingerprint") != c["fingerprint"]:
            changed.append((c["name"], c["fingerprint"], other.get("fingerprint")))
    print(f"# {base['gate']} gate: candidate against baseline")
    print()
    for label, r in (("baseline", base), ("candidate", cand)):
        mf = r.get("manifest", {})
        print(f"- {label}: host {r['host'].get('name')}, commit {mf.get('binary_commit') or mf.get('commit')}, "
              f"started {mf.get('started')}")
    print(f"- threshold: {threshold:.1%} slower{f', floor {args.floor}' if args.floor is not None else ''}"
          + (f", {short_threshold:.1%} below {short_below} ms" if short_below is not None else ""))
    for r in reasons:
        print(f"- compared although different: {r}")
    print()
    print("| metric | baseline | candidate | ratio | flag |")
    print("|---|--:|--:|--:|---|")
    fmt = lambda v: "" if v is None else f"{v:.6g}"
    for key, b, c, ratio, flag in rows:
        print(f"| {key} | {fmt(b)} | {fmt(c)} | {'' if ratio is None else f'{ratio:.3f}'} | {flag} |")
    print()
    if failed:
        print("Failed candidate checks: " + ", ".join(c["name"] for c in failed) + ".")
    if missing:
        print("Checks of the baseline that the candidate lacks: " + ", ".join(missing) + ".")
    for name, b, c in changed:
        print(f"Changed from the baseline: {name}, {b} against {c}.")
    print(f"{len(rows)} metrics compared, {len(bad)} flagged, {len(failed)} failed checks, "
          f"{len(missing)} missing checks, {len(changed)} changed fingerprints.")
    sys.exit(1 if bad or failed or changed or missing else 0)


# --------------------------------------------------------------------------- order-digest

XSD_NS = "http://www.w3.org/2001/XMLSchema#"


def nt_norm(term):
    """An N-Triples term with the abbreviations of SPARQL TSV undone: a bare number or
    boolean becomes its typed literal and an xsd:string literal a plain one."""
    t = term.strip()
    if re.fullmatch(r"[+-]?\d+", t):
        return f'"{t}"^^<{XSD_NS}integer>'
    if re.fullmatch(r"[+-]?\d*\.\d+", t):
        return f'"{t}"^^<{XSD_NS}decimal>'
    if re.fullmatch(r"[+-]?(\d+\.?\d*|\.\d+)[eE][+-]?\d+", t):
        return f'"{t}"^^<{XSD_NS}double>'
    if t in ("true", "false"):
        return f'"{t}"^^<{XSD_NS}boolean>'
    if t.endswith(f"^^<{XSD_NS}string>"):
        return t[: -len(f"^^<{XSD_NS}string>")]
    return t


def is_triple_export(query, names):
    """Whether the query is a plain export of triples, `SELECT ?s ?p ?o` or `SELECT *` over
    the single pattern `?s ?p ?o`, whose rows must then be triples of the input."""
    q = re.sub(r"#[^\n]*", "", re.sub(r"<[^<>\s]*>", "<>", query))
    return names == ["s", "p", "o"] and re.search(r"\{\s*\?s\s+\?p\s+\?o\s*\.?\s*\}", q) is not None \
        and len(re.findall(r"[{}]", q)) == 2


def missing_triples(rows, data):
    """How many of `rows`, (s, p, o) N-Triples terms, are not triples of the N-Triples file
    `data`, which is read once as a stream."""
    want = {tuple(nt_norm(x) for x in r) for r in rows}
    with open(data, encoding="utf-8", errors="replace") as f:
        for line in f:
            m = re.match(r"\s*(\S+)\s+(\S+)\s+(.*?)\s*\.\s*$", line)
            if m:
                want.discard((nt_norm(m.group(1)), nt_norm(m.group(2)), nt_norm(m.group(3))))
                if not want:
                    break
    return len(want)


def native_nt(t):
    """An x-sparkles+json term written as N-Triples, for `missing_triples`."""
    if t is None:
        return "-"
    v = str(t.get("value", ""))
    if t.get("type") == "uri":
        return f"<{v}>"
    if t.get("type") == "bnode":
        return f"_:{v}"
    if t.get("type") == "literal":
        e = v.replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n").replace("\r", "\\r")
        if t.get("xml:lang"):
            return f'"{e}"@{t["xml:lang"]}'
        if t.get("datatype"):
            return f'"{e}"^^<{t["datatype"]}>'
        return f'"{e}"'
    return json.dumps(t, sort_keys=True)

def order_digest(args):
    """A digest of a TSV answer's order, or for a query without a top-level ORDER BY the
    number of its distinct rows (`distinct:N`).
    When the ORDER BY lists projected variables, the digest covers those key columns in
    the returned order (`keys:`), which rows that tie on the keys cannot change. Otherwise,
    such as for an expression, it covers every row in the returned order (`rows:`), which
    is exact for a total order and may reject a correct answer whose ties come out in
    another order, but never accepts a wrong order. Uses bench-answers.py's parser."""
    root = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location("bench_answers", root / "bench-answers.py")
    ba = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(ba)
    query = Path(args.query).read_text()
    with open(args.tsv, newline="") as f:
        header = f.readline().rstrip("\n").split("\t")
        names = [h.lstrip("?") for h in header]
        if ba._after_order_by(query) is None:
            # the number of distinct rows, so that a LIMIT without ORDER BY, whose rows may
            # be any, still cannot repeat rows unnoticed; with --data, a three-column answer
            # also counts its rows that are not triples of the input
            lines = [line.rstrip("\n") for line in f]
            # the header, since a LIMIT's count and distinct rows say nothing about it
            schema = hashlib.sha256("\t".join(header).encode()).hexdigest()[:8]
            out = f"distinct:{len(set(lines))} schema:{schema}"
            if args.data and is_triple_export(query, names):
                rows = [line.split("\t") for line in lines]
                out += f" missing:{missing_triples(rows, args.data)}"
            print(out)
            return
        keys = ba.order_keys(query, set(names))
        cols = [names.index(k) for k in keys] if keys else None
        h = hashlib.sha256()
        for line in f:
            row = line.rstrip("\n").split("\t")
            if cols is not None:
                row = [row[c] if c < len(row) else "" for c in cols]
            h.update("\t".join(row).encode() + b"\n")
    print(("keys:" if keys else "rows:") + h.hexdigest()[:16])


# environment variables that change the code Cargo and rustc generate; wrappers such as
# RUSTC_WRAPPER and cache paths do not
# and the compilers themselves are in the binary's .comment section
BUILD_ENV = re.compile(r"^(CARGO_PROFILE_.*|.*RUSTFLAGS|CARGO_BUILD_TARGET|CARGO_INCREMENTAL|CFLAGS.*|CXXFLAGS.*)$")


def build_config_cmd(args):
    """The build configuration that decides the generated code, as one line of JSON for
    the manifest's `build_config`: the workspace's Cargo profile, a digest of Boxington's
    effective build policy (scripts/mbx-policy-key.py), the profile and build
    settings of every Cargo config file that applies (the workspace's .cargo directories
    up to the root and $CARGO_HOME), and the environment variables that override them.
    Run it in the environment of the build."""
    import tomllib
    root = Path(args.root).resolve()
    out = {"profile": args.name, "cargo": args.cargo}
    cargo = tomllib.loads((root / "Cargo.toml").read_text()) if (root / "Cargo.toml").exists() else {}
    # the profile and the profiles it inherits from
    profiles, name = {}, args.name
    while name and name not in profiles:
        profiles[name] = cargo.get("profile", {}).get(name)
        name = (profiles[name] or {}).get("inherits")
    out["Cargo.toml"] = profiles
    configs = [d / ".cargo" / n for d in [root, *root.parents] for n in ("config.toml", "config")]
    home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    configs += [home / "config.toml", home / "config"]
    for f in configs:
        if f.is_file():
            c = tomllib.loads(f.read_text())
            # [env] reaches the build scripts that compile C code, such as mimalloc's
            out[str(f)] = {k: c[k] for k in ("profile", "build", "target", "env") if k in c}
    out["env"] = {k: v for k, v in sorted(os.environ.items()) if BUILD_ENV.match(k)}
    # Boxington's effective incremental, linker and cache policy, which its own resolver
    # reads from the environment and the global configuration, as mise's validation keys do
    import shutil
    if shutil.which("mbx"):
        spec = importlib.util.spec_from_file_location("mbx_policy", Path(__file__).resolve().parent / "mbx-policy-key.py")
        mp = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mp)
        out["mbx_policy"] = mp.policy_key()
    print(json.dumps(out, sort_keys=True, separators=(",", ":")))


def native_digest(args):
    """A digest of each application/x-sparkles+json SELECT or ASK answer in `args.responses`, one
    line each in order, for the matrix's check of the timed format after the timed
    requests: "<rows> rows, <digest of the sorted rows>" and, for a query with a top-level
    ORDER BY, the order digest of `order-digest` over the rows as returned. Rows are
    written as JSON lists of their terms' type, value, datatype and language. The answers of one query share
    one process, and the check against the input runs once for each distinct set of rows."""
    root = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location("bench_answers", root / "bench-answers.py")
    ba = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(ba)
    query = Path(args.query).read_text()
    ordered = ba._after_order_by(query) is not None
    # the number of missing triples of each distinct answer, which depends only on its rows
    missing_of = {}

    def term(t):
        if t is None:
            return None
        if "its:dir" in t:
            fail("native-digest does not support directional RDF literals")
        if t.get("type") == "triple":
            v = t["value"]
            return ["triple"] + [term(v[k]) for k in ("subject", "predicate", "object")]
        return [str(t.get(k, "")) for k in ("type", "value", "datatype", "xml:lang")]

    def show(cells):
        # JSON, so that no value can pass for a delimiter between terms or rows
        return json.dumps([term(t) for t in cells], ensure_ascii=True)

    def digest(response):
        try:
            r = json.loads(Path(response).read_text())
        except (OSError, ValueError):
            return "unreadable"
        kind = r.get("queryType") if isinstance(r, dict) else None
        if kind not in ("SELECT", "ASK"):
            fail(f"{response}: native-digest supports SELECT and ASK, got {kind!r}; graph answers need an RDF checker")
        names = r.get("vars") or []
        if kind == "ASK":
            if type(r.get("boolean")) is not bool:
                fail(f"{response}: native ASK answer requires a boolean")
            return f"ask {r['boolean']}"
        if not isinstance(r.get("vars"), list) or not isinstance(r.get("rows"), list):
            fail(f"{response}: native SELECT answer requires vars and rows arrays")
        if any(len(row) != len(names) for row in r.get("rows") or []):
            return "rows wider or narrower than vars"
        rows = [show(row) for row in r.get("rows") or []]
        # the query type and the projected variables, in order, belong to the answer
        head = f"{r.get('queryType')} " + " ".join(names)
        sorted_hash = hashlib.sha256((head + "\n" + "\n".join(sorted(rows))).encode()).hexdigest()
        missing = None
        if args.data and is_triple_export(query, names) and not ordered:
            if sorted_hash not in missing_of:
                missing_of[sorted_hash] = missing_triples(
                    [[native_nt(t) for t in row] for row in r.get("rows") or []], args.data)
            missing = missing_of[sorted_hash]
        out = [f"{len(rows)} rows", "schema " + hashlib.sha256(head.encode()).hexdigest()[:8],
               f"distinct {len(set(rows))}" + ("" if missing is None else f" missing {missing}"),
               sorted_hash[:16]]
        if ordered:
            keys = ba.order_keys(query, set(names))
            h = hashlib.sha256()
            for row in r.get("rows") or []:
                cells = [row[names.index(k)] for k in keys] if keys else row
                h.update(show(cells).encode() + b"\n")
            out.append(("keys:" if keys else "rows:") + h.hexdigest()[:16])
        return ", ".join(out)

    for response in args.responses:
        print(digest(response), flush=True)


def compilers_cmd(args):
    c = compilers(args.binary)
    if c is None:
        fail(f"{args.binary}: no compiler identities in its .comment section")
    print(c)


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
    p.add_argument("file", nargs="+")
    p.add_argument("--gate", required=True, choices=sorted(GATES))
    p.add_argument("--out", required=True)
    p.add_argument("--suite")
    p.add_argument("--scale")
    p.add_argument("--threshold", type=float, default=0.05)
    p.add_argument("--manifest", action="append", metavar="KEY=VALUE")
    p.add_argument("--setting", action="append", metavar="KEY=VALUE")
    p.add_argument("--binary", action="append", help="a harness binary whose compilers go into the manifest")
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
    p.add_argument("--allow-different", action="store_true")
    p = sub.add_parser("catalog")
    p.add_argument("record")
    p = sub.add_parser("order-digest")
    p.add_argument("query")
    p.add_argument("tsv")
    p.add_argument("--data", help="the N-Triples input, to check three-column unordered answers against")
    p = sub.add_parser("compilers")
    p.add_argument("binary")
    p = sub.add_parser("native-digest")
    p.add_argument("query")
    p.add_argument("responses", nargs="+", help="the answers to the query, one output line each")
    p.add_argument("--data", help="the N-Triples input, to check three-column unordered answers against")
    p = sub.add_parser("build-config")
    p.add_argument("name", nargs="?", default="release", help="the Cargo profile")
    p.add_argument("--cargo", required=True,
                   help="the Cargo command and arguments of the build, such as its package and features")
    p.add_argument("--root", default=".")
    args = ap.parse_args()
    {"executors": executors, "metrics": metrics, "w3c": w3c, "compare": compare, "catalog": catalog,
     "order-digest": order_digest, "compilers": compilers_cmd, "build-config": build_config_cmd,
     "native-digest": native_digest}[args.cmd](args)


if __name__ == "__main__":
    main()
