#!/usr/bin/env python3
"""Summarize benchmark results as a Markdown table (results/summary.md).

    bench-summary.py <results-dir> <query-name>...

Reads the files scripts/bench.sh and scripts/bench-billion.sh write into <results-dir>:
hyperfine JSON per query (`<name>.json`), `load.json`, `update-latency.json`,
`throughput.json` (star-join) or `throughput-<query>.json`, the answer fingerprints
(`answers.json`, see bench-answers.py), `rss.json`, `rss-probe.json`, `mem.json` (server
RSS at start, after a warm-up and at the throughput peak, each query's peak RSS, and the
Sparkles block cache), `load-details.json` (peak RSS and index size of each load),
`cold.json` and `cold-runs.json` (queries after a restart with the engine's files evicted
from the page cache, and the time to ready), `churn.json` (the commits of the updates
mode) and `mixed.json` (reads and writes of the mixed mode). Engines whose answer differs
from the majority are footnoted and not ranked. The best value of each row is bold.
"""
import json, os, statistics, sys
d, names = sys.argv[1], sys.argv[2:]
ORDER = ["sparkles", "jena-fuseki", "qlever", "fluree", "oxigraph"]
LOADNAME = {"jena-fuseki": "jena-tdb2"}
def load(f):
    return {r["command"]: r for r in json.load(open(f))["results"]} if os.path.exists(f) else {}
seen = set()
THROUGHPUT = sorted(f[:-5] for f in os.listdir(d) if f.startswith("throughput") and f.endswith(".json"))
for n in names + ["update-latency"] + THROUGHPUT:
    seen |= set(load(f"{d}/{n}.json"))
# engines that were only loaded or answer-checked (nothing timed yet) get a column too
if os.path.exists(f"{d}/answers.json"):
    seen |= {e for v in json.load(open(f"{d}/answers.json")).values() for e in v}
seen |= {next((c for c, l in LOADNAME.items() if l == e), e) for e in load(f"{d}/load.json")}
for f in ("mem.json", "churn.json", "mixed.json", "cold.json", "rss.json"):
    if os.path.exists(f"{d}/{f}"):
        j = json.load(open(f"{d}/{f}"))
        seen |= {e for e in j if e in ORDER} | {e for v in j.values() if isinstance(v, dict) for e in v if e in ORDER}
cmds = [c for c in ORDER if c in seen] + sorted(seen - set(ORDER))
out = ["| query | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
# hyperfine has no standard deviation for a single run (null)
def fmt(r): return f"{r['mean']*1000:.1f} ± {(r['stddev'] or 0)*1000:.1f}"
def nfailed(r): return sum(1 for e in r.get("exit_codes", []) if e != 0)
def row(label, rs, bad=frozenset(), f=fmt, better=min, key=lambda r: r["mean"], err="error",
        mark=None, unranked=frozenset()):
    mark = mark or {}
    ok = {c: rs[c] for c in cmds if c in rs and c not in bad and nfailed(rs[c]) == 0}
    rank = {c: r for c, r in ok.items() if c not in unranked}
    best = better(rank, key=lambda c: key(rank[c])) if rank else None
    cells = []
    for c in cmds:
        if c not in rs: cells.append("—")
        elif c in bad: cells.append(err)
        elif c not in ok:
            k = nfailed(rs[c]); cells.append(f"{err} ({k}/{len(rs[c].get('exit_codes', []))} runs failed)")
        else: cells.append(("**%s**" if c == best else "%s") % f(ok[c]) + mark.get(c, ""))
    out.append(f"| {label} | " + " | ".join(cells) + " |")
def majority(vals):
    """the value most engines agree on, if at least two do and no other value ties it"""
    vs = list(vals.values())
    if not vs: return None
    counts = sorted((vs.count(v) for v in set(vs)), reverse=True)
    ref = max(set(vs), key=vs.count)
    return ref if counts[0] > 1 and (len(counts) == 1 or counts[1] < counts[0]) else None
lr = load(f"{d}/load.json")
if lr:
    row("**load** (s)", {c: lr[LOADNAME.get(c, c)] for c in cmds if LOADNAME.get(c, c) in lr}, f=lambda r: f"{r['mean']:.2f}")
answers = json.load(open(f"{d}/answers.json")) if os.path.exists(f"{d}/answers.json") else {}
rows = json.load(open(f"{d}/rows.json")) if os.path.exists(f"{d}/rows.json") else {}
notes = []
for n in names:
    rs = load(f"{d}/{n}.json")
    if n in answers:
        an = answers[n]
        bad = {c for c, v in an.items() if v["rows"] == "error"}
        ok = {c: v for c, v in an.items() if c not in bad}
        # value-level disagreement with the majority: shown, footnoted, not ranked
        by_value = {c: v.get("value", v["rows"]) for c, v in ok.items()}
        ref = majority(by_value)
        wrong = {c for c, v in by_value.items() if ref is not None and v != ref}
        # no majority (two engines that differ, or a tie): none is ranked
        split = set(by_value) if ref is None and len(set(by_value.values())) > 1 else set()
        # same values but different RDF terms (lexical forms): footnoted, still ranked
        same = {c: v["exact"] for c, v in ok.items() if c not in wrong and "exact" in v}
        eref = majority(same)
        lexical = {c for c, v in same.items() if eref is not None and v != eref}
        mark = {c: " †" for c in wrong | split} | {c: " ‡" for c in lexical}
        row(n, rs, bad, err="error", mark=mark, unranked=wrong | split)
        if split:
            notes.append(f"† `{n}`: the answers differ and no answer has a majority (" + ", ".join(f"{c} {an[c]['rows']} rows" for c in sorted(split)) + "); not ranked")
        notes += [f"† `{n}`: {c} returned a different answer ({an[c]['rows']} rows) than the majority; not ranked" for c in sorted(wrong)]
        notes += [f"‡ `{n}`: {c} returned equal values as different RDF terms (numeric datatype or lexical form)" for c in sorted(lexical)]
    else:
        # older runs recorded row counts only
        rn = rows.get(n, {})
        counts = {c: v for c, v in rn.items() if v != "error"}
        ref = majority(counts)
        bad = {c for c, v in rn.items() if v == "error"}
        wrong = {c for c, v in counts.items() if ref is not None and v != ref}
        row(n, rs, bad, err="error", mark={c: " †" for c in wrong})
        notes += [f"† `{n}`: {c} returned {rn[c]} rows, the majority {ref}" for c in sorted(wrong)]
rs = load(f"{d}/update-latency.json")
if rs: row("**update** (1-triple INSERT DATA)", rs)
nreq = int(os.environ.get("NREQ", "160"))
conc = os.environ.get("CONC", "16")
for t in THROUGHPUT:
    rs = load(f"{d}/{t}.json")
    q = t.removeprefix("throughput-") if t != "throughput" else "star-join"
    if rs:
        row(f"**throughput** {q}, {conc} clients (queries/s)", rs, f=lambda r: ("%.0f" if nreq / r["mean"] >= 10 else "%.1f") % (nreq / r["mean"]),
            better=max, key=lambda r: nreq / r["mean"], err="timeout/error")
# ------------------------------------------------------------------------ memory
def jload(name):
    f = f"{d}/{name}"
    return json.load(open(f)) if os.path.exists(f) else {}
def isnum(v): return isinstance(v, (int, float)) and not isinstance(v, bool)
def table(caption, first):
    """the caption and head of a Markdown table with one column per engine"""
    return ["", caption, "", f"| {first} | " + " | ".join(cmds) + " |", "|---|" + "---:|" * len(cmds)]
def vrow(label, vals, f=lambda v: "%.0f" % v, better=min):
    """one row of per-engine values (numbers, or strings such as "?"); the best number is bold"""
    if not any(c in vals for c in cmds): return None
    nums = {c: vals[c] for c in cmds if isnum(vals.get(c))}
    best = better(nums, key=nums.get) if better and len(nums) > 1 else None
    cells = ["—" if c not in vals else ("**%s**" if c == best else "%s") % f(vals[c]) if isnum(vals[c]) else str(vals[c])
             for c in cmds]
    return f"| {label} | " + " | ".join(cells) + " |"
def size(v): return "%.1f GiB" % (v / 2**30) if v >= 2**30 else "%.0f MiB" % (v / 2**20) if v >= 10 * 2**20 else "%.1f MiB" % (v / 2**20)
mem = jload("mem.json")
rss = {c: int(v) if str(v).isdigit() else v for c, v in jload("rss.json").items()}
probe = jload("rss-probe.json")
ld = jload("load-details.json")
lde = {c: ld[LOADNAME.get(c, c)] for c in cmds if LOADNAME.get(c, c) in ld}
mrows = [
    vrow("server RSS 2 s after start, before any query (MiB)", mem.get("idle", {})),
    vrow("server RSS after every query ran once (MiB)", mem.get("warm", {})),
    vrow("server RSS after the run (MiB)", rss),
    vrow("peak RSS during the throughput run (MiB)", mem.get("throughput_peak", {})),
]
bc = mem.get("block_cache", {})
if isnum(bc.get("sparkles")) and isnum(rss.get("sparkles")):
    mrows.append(vrow(f"server RSS after the run less Sparkles' decoded-block cache of {bc['sparkles']} MiB (MiB)",
                      {"sparkles": rss["sparkles"] - bc["sparkles"]}, better=None))
if probe:
    cell = lambda c, k: str(probe[c][k]) if c in probe and k in probe[c] else "—"
    mrows.append("| RSS probe, fresh server: after the queries / after 3×160 star-join (MiB) | "
                 + " | ".join(f"{cell(c, 'after_queries_mib')} / {cell(c, 'after_round3_mib')}" if c in probe else "—" for c in cmds) + " |")
mrows += [
    vrow("load peak RSS (MiB)", {c: v["max_rss_kib"] / 1024 for c, v in lde.items() if "max_rss_kib" in v}),
    vrow("index size on disk", {c: v["index_bytes"] for c, v in lde.items() if "index_bytes" in v}, f=size),
]
mrows = [r for r in mrows if r]
if mrows:
    out += table("Memory: the servers' resident set (VmRSS, and VmHWM for peaks) and each load's peak RSS (GNU time). "
                 "Lower is better.", "memory") + mrows
mq = mem.get("queries", {})
if any(n in mq for n in names):
    out += table("Memory per query: how far the server's peak RSS rose over its RSS before the request, then the peak, "
                 "in MiB. The peak is reset (clear_refs) before each request of the answer check.", "query (rise / peak)")
    for n in names:
        if n not in mq: continue
        v = mq[n]
        rise = {c: v[c]["delta"] for c in cmds if c in v and isnum(v[c].get("delta"))}
        best = min(rise, key=rise.get) if len(rise) > 1 else None
        cells = ["—" if c not in v else (("**+%s**" if c == best else "+%s") % v[c]["delta"] + f" / {v[c]['peak']}"
                 if c in rise else f"? / {v[c].get('peak', '?')}") for c in cmds]
        out.append(f"| {n} | " + " | ".join(cells) + " |")

# ------------------------------------------------------------------------ writes
churn = jload("churn.json")
if churn:
    total = max((v.get("commits", 0) + v.get("errors", 0) for v in churn.values()), default=0)
    ms = lambda v: "%.2f" % v
    out += table(f"Churn before the queries: {total} single-triple commits, one request each and the same for every "
                 "engine. They are INSERT DATA and DELETE DATA in the default graph.", "commits")
    out += [r for r in [
        vrow("commits/s", {c: v["per_s"] for c, v in churn.items()}, better=max),
        vrow("latency p50 (ms)", {c: v["p50_ms"] for c, v in churn.items()}, f=ms),
        vrow("latency p99 (ms)", {c: v["p99_ms"] for c, v in churn.items()}, f=ms),
        vrow("failed commits", {c: v["errors"] for c, v in churn.items()}, better=None),
    ] if r]
mixed = jload("mixed.json")
if mixed:
    ms = lambda v: "%.1f" % v
    out += table("Mixed load: concurrent star-join readers (oha) and one writer of single-triple INSERT DATA commits. "
                 "Each engine runs alone on a copy of its store.", "mixed")
    out += [r for r in [
        vrow("reads/s", {c: v["read_qps"] for c, v in mixed.items()}, f=ms, better=max),
        vrow("read p50 (ms)", {c: v["read_p50_ms"] for c, v in mixed.items()}, f=ms),
        vrow("read p99 (ms)", {c: v["read_p99_ms"] for c, v in mixed.items()}, f=ms),
        vrow("writes/s", {c: v["writes_per_s"] for c, v in mixed.items()}, f=ms, better=max),
        vrow("write p50 (ms)", {c: v["write_p50_ms"] for c, v in mixed.items()}, f=ms),
        vrow("write p99 (ms)", {c: v["write_p99_ms"] for c, v in mixed.items()}, f=ms),
        vrow("failed reads / writes", {c: f"{v['read_errors']} / {v['write_errors']}" for c, v in mixed.items()}, better=None),
    ] if r]

# ------------------------------------------------------------------------- cold
if os.path.exists(f"{d}/cold.json"):
    cold = json.load(open(f"{d}/cold.json"))
    runs = jload("cold-runs.json")
    k = max((len(r["times"]) for e in runs.values() for r in e.values()), default=1)
    what = f"The median of {k} runs" if k > 1 else "One run"
    out += ["", f"Cold runs: {what.lower()} of each query after a server restart with the engine's files evicted from "
            "the page cache.", "",
            "| query (cold) | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
    for n in names:
        vals = {c: cold.get(c, {}).get(n) for c in cmds}
        times = {c: v for c, v in vals.items() if isinstance(v, (int, float))}
        best = min(times, key=times.get) if times else None
        cells = ["—" if v is None else "error" if not isinstance(v, (int, float))
                 else ("**%.1f**" if c == best else "%.1f") % (v * 1000) for c, v in vals.items()]
        if any(v is not None for v in vals.values()):
            out.append(f"| {n} | " + " | ".join(cells) + " |")
    ready = {c: statistics.median([t for r in runs[c].values() for t in r["ready"]]) for c in cmds if c in runs}
    r = vrow("**time to ready** after the restart (median, s)", ready, f=lambda v: "%.2f" % v)
    if r: out.append(r)
if notes:
    out += [""] + notes
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
