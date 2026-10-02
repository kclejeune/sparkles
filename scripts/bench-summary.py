#!/usr/bin/env python3
"""Summarize benchmark results as a Markdown table (results/summary.md).

    bench-summary.py <results-dir> <query-name>...

Reads the files scripts/bench.sh and scripts/bench-billion.sh write into <results-dir>:
hyperfine JSON per query (`<name>.json`), `load.json`, `update-latency.json`,
`throughput.json` (star-join) or `throughput-<query>.json`, the answer fingerprints
(`answers.json`, see bench-answers.py), `rss.json` and `rss-probe.json`, and from
bench-billion.sh `load-details.json` (peak RSS and index size of each load) and
`cold.json` (one run per query after a restart with the engine's files evicted from the
page cache). Engines whose answer differs from the majority are footnoted and not ranked.
"""
import json, sys, os
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
cmds =[c for c in ORDER if c in seen] + sorted(seen - set(ORDER))
out = ["| query | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
def fmt(r): return f"{r['mean']*1000:.1f} ± {r['stddev']*1000:.1f}"
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
if os.path.exists(f"{d}/rss.json"):
    rss = json.load(open(f"{d}/rss.json"))
    out.append("| **server RSS** after the run (MiB) | " + " | ".join(str(rss.get(c, "—")) for c in cmds) + " |")
if os.path.exists(f"{d}/rss-probe.json"):
    pr = json.load(open(f"{d}/rss-probe.json"))
    cell = lambda c, k: str(pr[c][k]) if c in pr and k in pr[c] else "—"
    out.append("| **RSS probe**, fresh server: after the queries / after 3×160 star-join (MiB) | "
               + " | ".join(f"{cell(c, 'after_queries_mib')} / {cell(c, 'after_round3_mib')}" if c in pr else "—" for c in cmds) + " |")
if os.path.exists(f"{d}/load-details.json"):
    ld = json.load(open(f"{d}/load-details.json"))
    cell = lambda c, k, f: f(ld[LOADNAME.get(c, c)][k]) if k in ld.get(LOADNAME.get(c, c), {}) else "—"
    out.append("| **load** peak RSS (MiB) | " + " | ".join(cell(c, "max_rss_kib", lambda v: "%.0f" % (v / 1024)) for c in cmds) + " |")
    out.append("| **index size** (GiB) | " + " | ".join(cell(c, "index_bytes", lambda v: "%.1f" % (v / 2**30)) for c in cmds) + " |")
if os.path.exists(f"{d}/cold.json"):
    cold = json.load(open(f"{d}/cold.json"))
    out += ["", "Cold runs: the first run of each query after a server restart with the engine's files evicted from the page cache.", "",
            "| query (cold) | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|" + "---:|" * len(cmds)]
    for n in names:
        vals = {c: cold.get(c, {}).get(n) for c in cmds}
        times = {c: v for c, v in vals.items() if isinstance(v, (int, float))}
        best = min(times, key=times.get) if times else None
        cells = ["—" if v is None else "error" if not isinstance(v, (int, float))
                 else ("**%.1f**" if c == best else "%.1f") % (v * 1000) for c, v in vals.items()]
        if any(v is not None for v in vals.values()):
            out.append(f"| {n} | " + " | ".join(cells) + " |")
if notes:
    out += [""] + notes
open(f"{d}/summary.md", "w").write("\n".join(out) + "\n")
print("\n".join(out))
