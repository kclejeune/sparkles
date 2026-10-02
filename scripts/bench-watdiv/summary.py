#!/usr/bin/env python3
"""Summarize a WatDiv run of scripts/bench-watdiv.sh as Markdown.

    summary.py <results-dir> <triples> <params> <instance>...

Instances are named <template>-<i> (L1-1, …, C3-5). The script reads each instance's
hyperfine JSON (`<instance>.json`), the answer fingerprints (`answers.json`, see
scripts/bench-answers.py) and `load.json`, and writes two files.

`summary.md` has one row per template with the geometric mean, over the template's
instances, of each engine's mean time. An engine whose answer to any instance differs
from the majority is marked with † and not ranked for that template, and an engine with a
failed request shows `error`. Rows per category (L, S, F, C) and over all templates give
the geometric mean of the template means, taken over the templates where every engine
answered every instance correctly, so that each engine is measured on the same queries.

`instances.md` has every instance with the majority's row count and each engine's mean
and standard deviation.
"""
import json
import math
import os
import sys

d, triples, params, names = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4:]
ORDER = ["sparkles", "jena-fuseki", "qlever", "fluree", "oxigraph"]
LOADNAME = {"jena-fuseki": "jena-tdb2"}
CATEGORIES = {"L": "linear", "S": "star", "F": "snowflake", "C": "complex"}


def load(f):
    p = os.path.join(d, f)
    return {r["command"]: r for r in json.load(open(p))["results"]} if os.path.exists(p) else {}


def majority(vals):
    """The value most engines agree on, if at least two do and no other value ties it."""
    vs = list(vals.values())
    if not vs:
        return None
    counts = sorted((vs.count(v) for v in set(vs)), reverse=True)
    ref = max(set(vs), key=vs.count)
    return ref if counts[0] > 1 and (len(counts) == 1 or counts[1] < counts[0]) else None


def gmean(xs):
    return math.exp(sum(math.log(x) for x in xs) / len(xs))


def ms(s):
    v = s * 1000
    return f"{v:.0f}" if v >= 100 else f"{v:.1f}" if v >= 10 else f"{v:.2f}"


answers = json.load(open(os.path.join(d, "answers.json"))) if os.path.exists(os.path.join(d, "answers.json")) else {}
times = {n: load(f"{n}.json") for n in names}
loads = load("load.json")
seen = {c for n in names for c in times[n]} | {c for n in names for c in answers.get(n, {})}
seen |= {next((c for c, l in LOADNAME.items() if l == e), e) for e in loads}
cmds = [c for c in ORDER if c in seen] + sorted(seen - set(ORDER))

# per instance and engine: "ok", "wrong" (differs from the majority), "split" (no
# majority), "error" (a failed request) or None (not run)
status, ref_rows, notes = {}, {}, []
for n in names:
    an = answers.get(n, {})
    ok = {c: v for c, v in an.items() if v["rows"] != "error"}
    by_value = {c: v.get("value", v["rows"]) for c, v in ok.items()}
    ref = majority(by_value)
    split = ref is None and len(set(by_value.values())) > 1
    ref_rows[n] = next((ok[c]["rows"] for c in ok if by_value[c] == ref), None) if ref is not None else None
    for c in cmds:
        r = times[n].get(c)
        failed = r is not None and any(e != 0 for e in r.get("exit_codes", []))
        if c in an and an[c]["rows"] == "error" or failed:
            s = "error"
        elif c in by_value and split:
            s = "split"
        elif c in by_value and ref is not None and by_value[c] != ref:
            s = "wrong"
        elif r is None and c not in an:
            s = None
        else:
            s = "ok"
        status[n, c] = s
    if split:
        notes.append(f"† `{n}`: the answers differ and no answer has a majority ("
                     + ", ".join(f"{c} {an[c]['rows']} rows" for c in sorted(by_value)) + ")")
    for c in sorted(by_value):
        if ref is not None and by_value[c] != ref:
            notes.append(f"† `{n}`: {c} returned a different answer ({an[c]['rows']} rows, the majority {ref_rows[n]})")
    for c in sorted(an):
        if an[c]["rows"] == "error":
            notes.append(f"`{n}`: {c} failed: {an[c].get('error', '')}")

templates = list(dict.fromkeys(n.rsplit("-", 1)[0] for n in names))
inst = {t: [n for n in names if n.rsplit("-", 1)[0] == t] for t in templates}

# per template and engine: (geometric mean in seconds or None, cell text, clean)
cells = {}
for t in templates:
    for c in cmds:
        st = [status[n, c] for n in inst[t]]
        means = [times[n][c]["mean"] for n in inst[t] if c in times[n]]
        if all(s is None for s in st):
            cells[t, c] = (None, "—", False)
        elif "error" in st:
            k = st.count("error")
            cells[t, c] = (None, f"error ({k}/{len(st)})", False)
        elif len(means) < len(st):
            cells[t, c] = (None, "—", False)
        else:
            g = gmean(means)
            bad = any(s in ("wrong", "split") for s in st)
            cells[t, c] = (g, ms(g) + (" †" if bad else ""), not bad)

out = [f"WatDiv basic testing, {params}, {int(triples):,} generated triples. Times are in ms: the geometric "
       "mean over each template's instances of the mean of the timed runs.", ""]
if loads:
    out += ["| load (s) | " + " | ".join(cmds) + " |", "|---|" + "---:|" * len(cmds)]
    lr = {c: loads[LOADNAME.get(c, c)]["mean"] for c in cmds if LOADNAME.get(c, c) in loads}
    best = min(lr, key=lr.get) if lr else None
    out.append("| load | " + " | ".join(("**%.2f**" if c == best else "%.2f") % lr[c] if c in lr else "—" for c in cmds) + " |")
    out.append("")

out += ["| template | instances | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|---:|" + "---:|" * len(cmds)]
for t in templates:
    ranked = {c: cells[t, c][0] for c in cmds if cells[t, c][2]}
    best = min(ranked, key=ranked.get) if ranked else None
    row = [f"**{cells[t, c][1]}**" if c == best else cells[t, c][1] for c in cmds]
    out.append(f"| {t} | {len(inst[t])} | " + " | ".join(row) + " |")

# category and overall rows, over the templates every engine answered correctly
common = [t for t in templates if all(cells[t, c][2] for c in cmds)]
out += ["", "| category | templates | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|---:|" + "---:|" * len(cmds)]
groups = [(f"{CATEGORIES.get(k, k)} ({k})", [t for t in templates if t[0] == k]) for k in CATEGORIES]
groups.append(("all templates", templates))
for label, ts in groups:
    if not ts:
        continue
    use = [t for t in ts if t in common]
    if not use or not cmds:
        out.append(f"| {label} | 0 of {len(ts)} | " + " | ".join("—" for _ in cmds) + " |")
        continue
    g = {c: gmean([cells[t, c][0] for t in use]) for c in cmds}
    best = min(g, key=g.get)
    out.append(f"| **{label}** | {len(use)} of {len(ts)} | "
               + " | ".join(("**%s**" if c == best else "%s") % ms(g[c]) for c in cmds) + " |")
left = [t for t in templates if t not in common]
if left:
    out += ["", "The category rows leave out " + ", ".join(f"`{t}`" for t in left)
            + ", where an engine failed or disagreed with the majority."]
if notes:
    out += [""] + notes
open(os.path.join(d, "summary.md"), "w").write("\n".join(out) + "\n")
print("\n".join(out))

# every instance
rows = ["| instance | rows | " + " | ".join(f"{c} (ms)" for c in cmds) + " |", "|---|---:|" + "---:|" * len(cmds)]
for n in names:
    cs = []
    for c in cmds:
        s, r = status[n, c], times[n].get(c)
        if s is None:
            cs.append("—")
        elif s == "error":
            cs.append("error")
        elif r is None:
            cs.append("—" + (" †" if s != "ok" else ""))
        else:
            cs.append(f"{ms(r['mean'])} ± {ms(r['stddev'] or 0)}" + (" †" if s != "ok" else ""))
    rr = ref_rows[n]
    rows.append(f"| {n} | {'—' if rr is None else rr} | " + " | ".join(cs) + " |")
open(os.path.join(d, "instances.md"), "w").write("\n".join(rows) + "\n")
