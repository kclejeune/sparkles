#!/usr/bin/env python3
"""Build, check and describe the common benchmark record (docs/specs/X06 §6 and §7).

    bench-record.py manifest  --out manifest.json --suite NAME [options]
    bench-record.py finish    manifest.json
    bench-record.py build     --suite NAME --scale LABEL --results DIR --manifest FILE --out record.json
    bench-record.py validate  record.json...

`manifest` writes the run manifest at the start of a run: the Sparkles commit and binary
hash, every engine's version, binary hash and license, the host, the memory limit, the CPU
pinning, the data hashes and the start time. `finish` adds the end time.

`build` turns the files a harness leaves in a results directory into one record. It reads

* `<query>.json`: hyperfine's output, one result per engine (warm runs). The files
  `update-latency.json` and `throughput*.json` are read as the modes `update` and
  `throughput`, with the query name taken from the file,
* `cold.json` and `cold-runs.json`: the restarts of bench.sh's cold mode,
* `answers.json`: the answer fingerprints of bench-answers.py,
* `qmph.json`: query mixes per hour, `{engine: {clients: {"qmph": …, …}}}` (bench-bsbm.sh),
* `load.json`, `load-details.json`, `rss.json` and `mem.json`: loads and memory,
* `extra.json`: optional `{"settings": {…}, "results": [ … ]}` merged into the record.

Which query files count is decided by `--queries` (a list of names); without it every
`*.json` file in the directory that is not one of the files above is taken as a query.

`validate` checks a record against the shape below and exits with status 1 on the first
file that does not conform.

Record 1 (every field is required unless marked optional):

    record      1
    suite       string, for example "bsbm-explore"
    scale       string, for example "10m"
    manifest    object (see `manifest`)
    settings    object: warmup, runs, timer, resultCache (others optional)
    results     list of
        engine       sparkles | jena | qlever | fluree | oxigraph | …
        query        string
        mode         warm | cold | update | throughput | qmph | load
        clients      integer
        durability   synced | written | memory | null
        status       ok | timeout | error | oom | wrong-answer
        samples_ms   list of numbers (optional for throughput results)
        median_ms    number or null
        p95_ms       number or null
        answer       {rows, hash, agrees} or null; agrees is true, false or null (no majority)
        qps, qmph    numbers (optional; throughput results)
    load        optional: {engine: {seconds, max_rss_kib, index_bytes}}
    memory      optional: {engine: {rss_after_mib, peak_mib, cache_mib}}
"""

import argparse
import datetime
import hashlib
import json
import math
import os
import platform
import re
import statistics
import subprocess
import sys

RECORD = 1
ENGINE_NAMES = {"jena-fuseki": "jena", "jena-tdb2": "jena"}
LICENSES = {
    "sparkles": "the repository's",
    "jena": "Apache-2.0",
    "qlever": "Apache-2.0",
    "oxigraph": "MIT OR Apache-2.0",
    "fluree": "BUSL-1.1",
}
# written (OS cache), synced or memory: docs/specs/X06 §5.1, from each engine's documentation
DURABILITY = {"sparkles": "synced", "jena": "synced", "oxigraph": "written", "qlever": "memory", "fluree": "synced"}
STATUSES = {"ok", "timeout", "error", "oom", "wrong-answer"}
MODES = {"warm", "cold", "update", "throughput", "qmph", "load"}
AUX = {"load", "load-details", "answers", "mem", "rss", "rss-probe", "churn", "mixed", "cold", "cold-runs",
       "qmph", "extra", "manifest", "record", "update-latency"}
CURL_TIMEOUT = 28


def engine_name(n):
    return ENGINE_NAMES.get(n, n)


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd, **kw):
    try:
        return subprocess.run(cmd, capture_output=True, text=True, timeout=20, **kw).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return ""


def percentile(xs, q):
    """Nearest-rank percentile, so that p95 of ten samples is the largest."""
    s = sorted(xs)
    return s[max(0, math.ceil(q * len(s)) - 1)]


# ------------------------------------------------------------------------- manifest
def host_info(path):
    cpu = ""
    try:
        for line in open("/proc/cpuinfo"):
            if line.startswith("model name"):
                cpu = line.split(":", 1)[1].strip()
                break
    except OSError:
        cpu = platform.processor()
    mem_kib = 0
    try:
        for line in open("/proc/meminfo"):
            if line.startswith("MemTotal:"):
                mem_kib = int(line.split()[1])
                break
    except OSError:
        pass
    fs = run(["findmnt", "-T", path, "-no", "SOURCE,FSTYPE,OPTIONS"]).split(None, 2)
    disk = ""
    if fs and fs[0].startswith("/dev/"):
        disk = run(["lsblk", "-no", "MODEL", fs[0]]).splitlines()[0:1]
        disk = disk[0].strip() if disk else ""
    return {
        "name": os.environ.get("BENCH_HOST") or platform.node(),
        "cpu": cpu,
        "cpus": os.cpu_count(),
        "ram_mib": mem_kib // 1024,
        "disk_model": disk,
        "kernel": platform.release(),
        "filesystem": fs[1] if len(fs) > 1 else "",
        "mount_options": fs[2] if len(fs) > 2 else "",
    }


def version_of(path, args):
    out = run([path, *args]) if path and os.path.exists(path) else ""
    return out.splitlines()[0] if out else "unknown"


def cmd_manifest(a):
    m = {"suite": a.suite, "started": now(), "ended": None, "host": host_info(a.workdir)}
    sp = {"binary": a.sparkles, "version": version_of(a.sparkles, ["--version"])}
    if a.sparkles and os.path.exists(a.sparkles):
        sp["sha256"] = sha256(a.sparkles)
    if a.commit:
        sp["commit"] = a.commit
    else:
        sp["commit"] = run(["git", "-C", os.path.dirname(os.path.abspath(__file__)), "rev-parse", "HEAD"]) or "unknown"
        if run(["git", "-C", os.path.dirname(os.path.abspath(__file__)), "status", "--porcelain", "--untracked-files=no"]):
            sp["commit"] += "+dirty"
    m["sparkles"] = sp
    engines = {}
    for spec in a.engine:
        name, _, path = spec.partition("=")
        name = engine_name(name)
        e = {"license": LICENSES.get(name, "unknown"), "binary": path}
        if path and os.path.exists(path):
            e["sha256"] = sha256(path)
            e["version"] = version_of(path, ["--version"])
        else:
            e["version"] = "unknown"
        engines[name] = e
    m["engines"] = engines
    m["memory_limit"] = a.memory_limit or None
    m["client_cpu"] = a.client_cpu or None
    data = {}
    for spec in a.data:
        # name=path[:license[:params]] hashed here; name=sha256:… recorded as given
        name, _, rest = spec.partition("=")
        path, _, extra = rest.partition("|")
        lic, _, params = extra.partition("|")
        d = {"license": lic or "unknown", "params": params or None}
        if path.startswith("sha256:"):
            d["sha256"] = path[7:]
        elif os.path.exists(path):
            d["sha256"] = sha256(path)
            d["bytes"] = os.path.getsize(path)
        data[name] = d
    m["data"] = data
    m["license_check"] = {
        "gpl_benchmark_allowed": os.environ.get("SPARKLES_ALLOW_GPL_BENCHMARK") == "1",
        "notes": a.license_note or [],
    }
    with open(a.out, "w") as f:
        json.dump(m, f, indent=1)
    print(a.out)


def cmd_finish(a):
    m = json.load(open(a.manifest))
    m["ended"] = now()
    json.dump(m, open(a.manifest, "w"), indent=1)


# ----------------------------------------------------------------------------- build
def load_json(d, name):
    p = os.path.join(d, name)
    return json.load(open(p)) if os.path.exists(p) else None


def fingerprints(answers):
    """Per query and engine: (rows, hash, agrees). Agreement is with the majority of the engines
    that answered, on the digest by value; None when there is no majority."""
    out = {}
    for q, per in (answers or {}).items():
        ok = {e: v for e, v in per.items() if v.get("rows") != "error"}
        digest = {e: v.get("value") or v.get("exact") or str(v.get("rows")) for e, v in ok.items()}
        vals = list(digest.values())
        ref = None
        if vals:
            counts = sorted((vals.count(v) for v in set(vals)), reverse=True)
            top = max(set(vals), key=vals.count)
            if len(vals) == 1:
                ref = top
            elif counts[0] > 1 and (len(counts) == 1 or counts[1] < counts[0]):
                ref = top
        for e, v in per.items():
            en = engine_name(e)
            if v.get("rows") == "error":
                out[q, en] = None
            else:
                agrees = None if ref is None or len(vals) == 1 else digest[e] == ref
                out[q, en] = {"rows": v["rows"], "hash": digest[e], "agrees": agrees}
    return out


def status_of(codes, answer):
    if codes and any(c != 0 for c in codes):
        return "timeout" if all(c in (0, CURL_TIMEOUT) for c in codes) and CURL_TIMEOUT in codes else "error"
    if answer is not None and answer["agrees"] is False:
        return "wrong-answer"
    return "ok"


def result(engine, query, mode, samples_s, codes, answer, clients=1, **extra):
    ok = [s * 1000 for s, c in zip(samples_s, codes or [0] * len(samples_s)) if c == 0]
    st = status_of(codes, answer)
    r = {
        "engine": engine,
        "query": query,
        "mode": mode,
        "clients": clients,
        "durability": DURABILITY.get(engine) if mode in ("update",) else None,
        "samples_ms": [round(x, 3) for x in ok],
        "median_ms": round(statistics.median(ok), 3) if ok else None,
        "p95_ms": round(percentile(ok, 0.95), 3) if ok else None,
        "answer": answer,
        "status": st,
    }
    r.update(extra)
    return r


def hyperfine_results(d, name, mode, fps, clients=1, query=None):
    j = load_json(d, name + ".json")
    out = []
    for r in (j or {}).get("results", []):
        en = engine_name(r["command"])
        q = query or name
        out.append(result(en, q, mode, r.get("times", []), r.get("exit_codes"), fps.get((q, en)), clients))
    return out


def cmd_build(a):
    d = a.results
    man = json.load(open(a.manifest)) if a.manifest else {}
    answers = load_json(d, "answers.json")
    fps = fingerprints(answers)
    names = a.queries or sorted(f[:-5] for f in os.listdir(d)
                                if f.endswith(".json") and f[:-5] not in AUX and not f.startswith("throughput")
                                and not f.endswith(".new.json"))
    results = []
    for n in names:
        results += hyperfine_results(d, n, "warm", fps)
    results += hyperfine_results(d, "update-latency", "update", fps)
    for f in sorted(os.listdir(d)):
        if f.startswith("throughput") and f.endswith(".json"):
            j = load_json(d, f)
            q = f[:-5] if f != "throughput.json" else "star-join"
            for r in j["results"]:
                en = engine_name(r["command"])
                reqs = int(a.throughput_requests) if a.throughput_requests else None
                ok = [t for t, c in zip(r.get("times", []), r.get("exit_codes", [0] * len(r.get("times", [])))) if c == 0]
                extra = {}
                if reqs and ok:
                    extra["qps"] = round(reqs / statistics.median(ok), 1)
                results.append(result(en, q, "throughput", r.get("times", []), r.get("exit_codes"), None,
                                      int(a.throughput_clients), **extra))
    cold = load_json(d, "cold.json") or {}
    cold_runs = load_json(d, "cold-runs.json") or {}
    for e, per in cold.items():
        en = engine_name(e)
        for q, med in per.items():
            runs = cold_runs.get(e, {}).get(q, {})
            times = runs.get("times") or ([med] if med != "error" else ["error"])
            rec = result(en, q, "cold", [t if t != "error" else 0 for t in times],
                         [0 if t != "error" else 1 for t in times], fps.get((q, en)))
            if runs.get("ready"):
                rec["ready_s"] = runs["ready"]
            results.append(rec)
    for e, per in (load_json(d, "qmph.json") or {}).items():
        en = engine_name(e)
        for clients, v in per.items():
            results.append({
                "engine": en, "query": v.get("query", "mix"), "mode": "qmph", "clients": int(clients),
                "durability": None, "samples_ms": [], "median_ms": None, "p95_ms": None, "answer": None,
                "status": v.get("status", "ok"), "qmph": v.get("qmph"),
                **{k: v[k] for k in ("qps", "runs", "timeouts", "seconds") if k in v},
            })
    extra = load_json(d, "extra.json") or {}
    results += extra.get("results", [])
    rec = {
        "record": RECORD,
        "suite": a.suite,
        "scale": a.scale,
        "manifest": man,
        "settings": {"warmup": a.warmup, "runs": a.runs, "timer": a.timer, "resultCache": False,
                     **extra.get("settings", {})},
        "results": results,
    }
    load = load_json(d, "load-details.json")
    if load:
        rec["load"] = {engine_name(e): v for e, v in load.items()}
    mem = {}
    for e, v in (load_json(d, "rss.json") or {}).items():
        mem.setdefault(engine_name(e), {})["rss_after_mib"] = v
    for e, v in ((load_json(d, "mem.json") or {}).get("throughput_peak") or {}).items():
        mem.setdefault(engine_name(e), {})["peak_mib"] = v
    for e, v in ((load_json(d, "mem.json") or {}).get("block_cache") or {}).items():
        mem.setdefault(engine_name(e), {})["cache_mib"] = v
    if mem:
        rec["memory"] = mem
    problems = check(rec)
    if problems:
        sys.exit("the record does not conform: " + "; ".join(problems[:5]))
    json.dump(rec, open(a.out, "w"), indent=1)
    print(f"{a.out}: {len(results)} results")


# --------------------------------------------------------------------------- validate
def check(rec):
    p = []
    if not isinstance(rec, dict):
        return ["not an object"]
    if rec.get("record") != RECORD:
        p.append(f"record must be {RECORD}")
    for k, t in (("suite", str), ("scale", str), ("manifest", dict), ("settings", dict), ("results", list)):
        if not isinstance(rec.get(k), t):
            p.append(f"{k} must be a {t.__name__}")
    if p:
        return p
    for k in ("warmup", "runs", "timer", "resultCache"):
        if k not in rec["settings"]:
            p.append(f"settings.{k} is missing")
    for i, r in enumerate(rec["results"]):
        w = f"results[{i}]"
        if not isinstance(r, dict):
            p.append(f"{w} is not an object")
            continue
        for k in ("engine", "query", "mode", "clients", "durability", "status", "answer"):
            if k not in r:
                p.append(f"{w}.{k} is missing")
        if r.get("mode") not in MODES:
            p.append(f"{w}.mode {r.get('mode')!r} is not one of {sorted(MODES)}")
        if r.get("status") not in STATUSES:
            p.append(f"{w}.status {r.get('status')!r} is not one of {sorted(STATUSES)}")
        if r.get("durability") not in (None, "synced", "written", "memory"):
            p.append(f"{w}.durability is not synced, written, memory or null")
        if not isinstance(r.get("clients"), int):
            p.append(f"{w}.clients must be an integer")
        for k in ("median_ms", "p95_ms"):
            if k in r and r[k] is not None and not isinstance(r[k], (int, float)):
                p.append(f"{w}.{k} must be a number or null")
        s = r.get("samples_ms", [])
        if not isinstance(s, list) or not all(isinstance(x, (int, float)) for x in s):
            p.append(f"{w}.samples_ms must be a list of numbers")
        ans = r.get("answer")
        if ans is not None and not (isinstance(ans, dict) and {"rows", "hash", "agrees"} <= set(ans)):
            p.append(f"{w}.answer must have rows, hash and agrees")
        if r.get("mode") in ("warm", "cold", "update") and r.get("status") == "ok" and not s:
            p.append(f"{w} is ok but has no samples")
    return p


def cmd_validate(a):
    bad = 0
    for f in a.files:
        problems = check(json.load(open(f)))
        for x in problems:
            print(f"{f}: {x}", file=sys.stderr)
        bad += bool(problems)
        if not problems:
            print(f"{f}: ok")
    sys.exit(1 if bad else 0)


def main():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    m = sub.add_parser("manifest")
    m.add_argument("--out", required=True)
    m.add_argument("--suite", required=True)
    m.add_argument("--sparkles", default="")
    m.add_argument("--commit", default="")
    m.add_argument("--engine", action="append", default=[], help="name=path of an engine's binary")
    m.add_argument("--data", action="append", default=[],
                   help="name=path|license|params, or name=sha256:HEX|license|params")
    m.add_argument("--memory-limit", default="")
    m.add_argument("--client-cpu", default="")
    m.add_argument("--workdir", default=".")
    m.add_argument("--license-note", action="append")
    m.set_defaults(f=cmd_manifest)
    f = sub.add_parser("finish")
    f.add_argument("manifest")
    f.set_defaults(f=cmd_finish)
    b = sub.add_parser("build")
    b.add_argument("--suite", required=True)
    b.add_argument("--scale", required=True)
    b.add_argument("--results", required=True)
    b.add_argument("--manifest", default="")
    b.add_argument("--out", required=True)
    b.add_argument("--queries", nargs="*")
    b.add_argument("--warmup", type=int, default=2)
    b.add_argument("--runs", type=int, default=10)
    b.add_argument("--timer", default="hyperfine+curl")
    b.add_argument("--throughput-requests", default="")
    b.add_argument("--throughput-clients", default="16")
    b.set_defaults(f=cmd_build)
    v = sub.add_parser("validate")
    v.add_argument("files", nargs="+")
    v.set_defaults(f=cmd_validate)
    a = p.parse_args()
    a.f(a)


if __name__ == "__main__":
    main()
