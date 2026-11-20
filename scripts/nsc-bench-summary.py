#!/usr/bin/env python3
"""Summarize an interleaved A/B run of scripts/nsc-bench-remote.sh.

    scripts/nsc-bench-summary.py [--pair X,Y] OUT
    scripts/nsc-bench-summary.py --check SUMMARY_JSON first|rerun [FLAGGED]

Reads OUT/manifest.txt and OUT/results/{samples,answers,load}.tsv and env.txt, prints a
Markdown summary and writes OUT/summary.json. For each query and metric (the server's
execution time and the client's wall time), the summary gives the median of every timed
request of A and of B, their ratio B/A, the ratio within each A,B,B,A round, and the spread
of each variant between server processes (the largest median of one process over the
smallest, minus one). The last row is the geometric mean of the ratios.

With --pair X,Y, a run of more variants is read as the A/B run of variant X against
variant Y, as if X were A and Y were B, and the other variants are left out. The summary
then goes to OUT/summary-X-Y.json. A queue of several tips against one base is summarized
this way, one pair at a time. Pairs that share the base's server processes are not
independent of each other.

The run's validity is judged by how far one binary's speed moves between server processes,
which a quiet host keeps near 1 to 2%. The same-binary spread is the median, over the queries
whose A median takes at least 0.3 ms, of the larger of their A and B spreads, and it must be
at most 3%. The machine speed of each server process is the geometric mean, over all queries,
of the process's median over the median of the query's requests to the same binary in the
whole run, and every process must be within 5% of the median process. summary.json gets them
under "validity", with "valid" false when either bound fails, and the summary says so in its
first lines.

--check exits 0 when a pass may decide and 1 when it may not. A first pass needs a valid
run. A rerun needs timings of both variants for every flagged query, and a valid run unless
all of them take under 0.3 ms.
"""

import csv
import json
import os
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path


def read_tsv(path):
    if not path.exists():
        return []
    with path.open() as f:
        return list(csv.reader(f, delimiter="\t"))


MIN_MS = 0.3
MAX_SPREAD = float(os.environ.get("SPARKLES_SCREEN_MAX_SPREAD", "0.03"))
MAX_DRIFT = float(os.environ.get("SPARKLES_SCREEN_MAX_DRIFT", "0.05"))


def validity(samples, order):
    """The same-binary spread and the per-process machine speed of the execution times."""
    # Each server process (epoch) runs one variant, and it is compared with that variant's
    # median, so that a binary that is uniformly faster or slower than the other reads as no
    # drift at all.
    meds = {}  # (query, epoch) -> (variant, median)
    per_query = {}  # (query, variant) -> median
    for q in order:
        for v in ("A", "B"):
            epochs = defaultdict(list)
            for _round, epoch, x in samples[q].get(v, []):
                epochs[epoch].append(x)
            if not epochs:
                continue
            per_query[(q, v)] = statistics.median(x for xs in epochs.values() for x in xs)
            for e, xs in epochs.items():
                meds[(q, e)] = (v, statistics.median(xs))

    def spread(xs):
        by = defaultdict(list)
        for _round, epoch, x in xs:
            by[epoch].append(x)
        m = [statistics.median(v) for v in by.values()]
        return max(m) / min(m) - 1 if m and min(m) > 0 else float("nan")

    big = [q for q in order if samples[q].get("A") and statistics.median(x[2] for x in samples[q]["A"]) >= MIN_MS]
    spreads = [max(spread(samples[q]["A"]), spread(samples[q].get("B", []))) for q in big if samples[q].get("B")]
    epochs = sorted({e for _q, e in meds})
    speed = []
    for e in epochs:
        r = [m / per_query[(q, v)] for (q, ee), (v, m) in meds.items() if ee == e and per_query[(q, v)] > 0]
        if r:
            speed.append(math.exp(sum(math.log(x) for x in r) / len(r)))
    if not spreads or not speed:
        return None
    mid = statistics.median(speed)
    rel = [x / mid for x in speed]
    sp = statistics.median(spreads)
    return {"spread": sp, "queries": len(spreads), "speed_min": min(rel), "speed_max": max(rel),
            "max_spread": MAX_SPREAD, "max_drift": MAX_DRIFT,
            "valid": sp <= MAX_SPREAD and min(rel) >= 1 - MAX_DRIFT and max(rel) <= 1 + MAX_DRIFT}


def check(path, kind, flagged):
    """The exit status of a host screen's pass: 0 when it may decide, 1 when it may not.

    A first pass needs a valid run. A rerun needs timings of both binaries for every flagged
    query, and a valid run unless all of them take under 0.3 ms, when there is no spread to
    judge."""
    s = json.load(open(path))
    v = s.get("validity")
    if kind == "rerun":
        for q in flagged.split():
            m = s["queries"].get(q, {}).get("exec_ms")
            if not m or not m.get("n_a") or not m.get("n_b") or (not v and m["a"] >= MIN_MS):
                return 1
        return 0 if not v or v["valid"] else 1
    return 0 if v and v["valid"] else 1


def main():
    if sys.argv[1] == "--check":
        sys.exit(check(sys.argv[2], sys.argv[3], sys.argv[4] if len(sys.argv) > 4 else ""))
    args = sys.argv[1:]
    pair = None
    if args[0] == "--pair":
        x, _, y = args[1].partition(",")
        if not x or not y or x == y:
            sys.exit(f"--pair takes two different variants, such as A,C, not {args[1]}")
        pair = {x: "A", y: "B"}
        args = args[2:]
    out = Path(args[0])
    res = out / "results"
    rows = read_tsv(res / "samples.tsv")
    header, rows = rows[0], rows[1:]
    col = {name: i for i, name in enumerate(header)}
    if pair:
        rows = [r[:col["variant"]] + [pair[r[col["variant"]]]] + r[col["variant"] + 1:]
                for r in rows if r[col["variant"]] in pair]
        if not rows:
            sys.exit(f"no samples of {', '.join(pair)} in {res / 'samples.tsv'}")

    # samples[metric][query][variant] -> list of (round, epoch, value)
    samples = {m: defaultdict(lambda: defaultdict(list)) for m in ("exec_ms", "wall_ms")}
    errors = defaultdict(int)
    order = []
    for r in rows:
        q, v = r[col["query"]], r[col["variant"]]
        if q not in order:
            order.append(q)
        if r[col["exec_ms"]] == "error":
            errors[(q, v)] += 1
            continue
        for m in samples:
            samples[m][q][v].append((int(r[col["round"]]), int(r[col["epoch"]]), float(r[col["exec_ms"] if m == "exec_ms" else col["wall_ms"]])))

    def stats(per_variant):
        a, b = per_variant.get("A", []), per_variant.get("B", [])
        if not a or not b:
            return None
        med = lambda xs: statistics.median(x[2] for x in xs)
        rounds = sorted({x[0] for x in a} & {x[0] for x in b})
        per_round = [med([x for x in b if x[0] == k]) / med([x for x in a if x[0] == k]) for k in rounds]

        def spread(xs):
            epochs = defaultdict(list)
            for x in xs:
                epochs[x[1]].append(x[2])
            meds = [statistics.median(v) for v in epochs.values()]
            return max(meds) / min(meds) - 1 if min(meds) > 0 else float("nan")

        ma, mb = med(a), med(b)
        return {"a": ma, "b": mb, "ratio": mb / ma, "rounds": per_round, "spread_a": spread(a), "spread_b": spread(b), "n_a": len(a), "n_b": len(b)}

    summary = {"queries": {}, "geomean": {}, "validity": validity(samples["exec_ms"], order)}
    for m in samples:
        for q in order:
            s = stats(samples[m][q])
            if s:
                summary["queries"].setdefault(q, {})[m] = s
        ratios = [summary["queries"][q][m]["ratio"] for q in order if m in summary["queries"].get(q, {})]
        if ratios:
            summary["geomean"][m] = math.exp(sum(math.log(r) for r in ratios) / len(ratios))

    p = print
    manifest = (out / "manifest.txt").read_text().splitlines() if (out / "manifest.txt").exists() else []
    p("# Interleaved A/B on " + __import__("socket").gethostname())
    p()
    if pair:
        p(f"Variant {x} is A and variant {y} is B in this summary.")
        p()
    for line in manifest:
        p(f"- {line}")
    env = (res / "env.txt").read_text().splitlines() if (res / "env.txt").exists() else []
    for line in env:
        p(f"- {line}")
    p()
    v = summary["validity"]
    if v:
        verdict = "valid" if v["valid"] else "INVALID: the host was not quiet enough for a timing comparison"
        p(f"Validity: {verdict}. Same-binary spread {v['spread'] * 100:.1f}% over {v['queries']} queries of at least {MIN_MS} ms (at most {MAX_SPREAD * 100:.0f}%), server-process speed {v['speed_min']:.3f} to {v['speed_max']:.3f} of the median (within {MAX_DRIFT * 100:.0f}%).")
        p()

    answers = read_tsv(res / "answers.tsv")
    if pair:
        answers = [[pair[r[0]]] + r[1:] for r in answers if r and r[0] in pair]
    by_q = defaultdict(dict)
    for row in answers:
        # variant, query, rows, digest and, with ORDER_DIGEST, an order digest
        v, q = row[:2]
        by_q[q][v] = tuple(row[2:])
    diff = [q for q in order if len(set(by_q[q].values())) != 1 or "error" in str(by_q[q])]
    if answers:
        if diff:
            p("Answers differ or failed for: " + ", ".join(f"{q} (A {by_q[q].get('A')}, B {by_q[q].get('B')})" for q in diff) + ".")
        else:
            p(f"A and B returned the same sorted TSV answers to all {len(by_q)} queries.")
        p()
    if errors:
        p("Failed timed requests: " + ", ".join(f"{q} {v} {n}" for (q, v), n in sorted(errors.items())) + ".")
        p()
    load = read_tsv(res / "load.tsv")
    if pair:
        load = [[pair[r[0]]] + r[1:] for r in load if r and r[0] in pair]
    try:
        (_, ta, ka), (_, tb, kb) = load
        ta, tb = float(ta), float(tb)
        p(f"Load: A {ta:.2f} s, B {tb:.2f} s (B/A {tb / ta:.3f}). Store size: A {int(ka) // 1024} MiB, B {int(kb) // 1024} MiB.")
        p()
    except (ValueError, ZeroDivisionError):
        pass

    titles = {"exec_ms": "Server execution time (execMs, ms)", "wall_ms": "Client wall time (ms)"}
    for m, title in titles.items():
        p(f"## {title}")
        p()
        p("| query | A | B | B/A | change | per-round B/A | A spread | B spread |")
        p("|---|--:|--:|--:|--:|---|--:|--:|")
        for q in order:
            s = summary["queries"].get(q, {}).get(m)
            if not s:
                p(f"| {q} | error | | | | | | |")
                continue
            rounds = " ".join(f"{r:.3f}" for r in s["rounds"])
            p(f"| {q} | {s['a']:.3f} | {s['b']:.3f} | {s['ratio']:.3f} | {(s['ratio'] - 1) * 100:+.1f}% | {rounds} | {s['spread_a'] * 100:.1f}% | {s['spread_b'] * 100:.1f}% |")
        if m in summary["geomean"]:
            g = summary["geomean"][m]
            p(f"| **geometric mean** | | | {g:.3f} | {(g - 1) * 100:+.1f}% | | | |")
        p()
    p("The ratios are medians of all timed requests of B over those of A. Per-round B/A uses the requests of one A,B,B,A round. A spread and B spread compare the medians of the same binary in different server processes.")
    if pair:
        summary["pair"] = {"A": x, "B": y}
    (out / (f"summary-{x}-{y}.json" if pair else "summary.json")).write_text(json.dumps(summary, indent=1))


if __name__ == "__main__":
    main()
