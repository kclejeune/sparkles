#!/usr/bin/env python3
"""Collect the BSBM test driver's runs in results/qmph.json and check them against each other.

    qmph.py <driver-output.xml> <qmph.json> <engine> <clients> <query-name> [<driver exit status>]
    qmph.py --check <qmph.json> [<comma-separated query numbers to leave out>]

The first form adds one run. The driver (benchmark.testdriver.TestDriver) writes an XML file
with the query mixes per hour it measured and, per query, how often it ran, how many results
it returned on average and how often it timed out. The file qmph.json maps an engine and a
client count to {qmph, runs, seconds, timeouts, avgresults, query, status}, the form
scripts/bench-record.py reads. A missing or unreadable XML file, or a driver that exited with a
nonzero status, records status `error`, and a run in which any query timed out records `timeout`.
That outcome is also kept in `driver_status`.

The driver does not look at what a query returns, so a query that fails quickly, or that
answers wrongly and quickly, makes the query mixes per hour look better. The second form compares
the average result count of every query. The driver draws the same queries for a given number of
mixes and seed, whatever the number of clients, so a correct engine returns the same averages in
every run. Only runs in which the driver succeeded without a timeout take part, and they are
compared only with runs that have the same mix count. Each engine casts one vote per query, and
only when its own runs agree, so an engine with more client counts does not outvote the others. When
only one engine has runs, its runs vote against each other. A run that differs from the majority on
any query gets status `wrong-answer` and the numbers of those queries in `disagreeing_queries`. A run
that failed keeps its `error` or `timeout`, and every check starts again from `driver_status`, so
a label from an earlier check does not stay.
"""

import json
import os
import sys
import xml.etree.ElementTree as ET
from collections import Counter


def load(path):
    if os.path.exists(path):
        with open(path) as f:
            return json.load(f)
    return {}


def save(path, d):
    with open(path, "w") as f:
        json.dump(d, f, indent=1)


def add(xml, out, engine, clients, name, exit_status):
    entry = {"query": name, "status": "error", "driver_status": "error", "qmph": None}
    try:
        root = ET.parse(xml).getroot()
        mix = root.find("querymix")
        timeouts = sum(int(q.findtext("timeoutcount") or 0) for q in root.iter("query"))
        qmph = float(mix.findtext("qmph"))
        runs = int(mix.findtext("querymixruns"))
        seconds = float(mix.findtext("actualtotalruntime") or mix.findtext("totalruntime"))
        avg = {q.get("nr"): float(q.findtext("avgresults")) for q in root.iter("query")
               if q.findtext("avgresults") is not None and int(q.findtext("executecount") or 0) > 0}
        status = "timeout" if timeouts else "ok"
        if exit_status:
            status = "error"
        entry = {"query": name, "status": status, "driver_status": status, "qmph": qmph, "runs": runs, "seconds": seconds,
                 "timeouts": timeouts, "avgresults": avg}
    except (OSError, ET.ParseError, AttributeError, ValueError, TypeError):
        pass
    d = load(out)
    d.setdefault(engine, {})[clients] = entry
    save(out, d)
    print(f"{engine} x{clients}: {entry['status']}" + (f", {entry['qmph']:.0f} QMpH" if entry["qmph"] else ""))


def base_status(v):
    """The status the driver run itself earned: ok, timeout or error."""
    base = v.get("driver_status")
    if base is None:
        base = v.get("status")
    return base if base in ("ok", "timeout", "error") else "ok"


def reference(votes):
    """The value most votes agree on, or None. Needs at least two votes and a strict plurality."""
    if len(votes) < 2:
        return None
    top = Counter(votes).most_common()
    if top[0][1] >= 2 and (len(top) == 1 or top[0][1] > top[1][1]):
        return top[0][0]
    return None


def check(path, skip=()):
    d = load(path)
    for per in d.values():
        for v in per.values():
            v["driver_status"] = v["status"] = base_status(v)
            v.pop("disagreeing_queries", None)
    groups = {}
    for e, per in d.items():
        for c, v in per.items():
            if v["driver_status"] == "ok" and v.get("avgresults"):
                groups.setdefault(v.get("runs"), []).append((e, c, v))
    compared = bad = 0
    for runs in groups.values():
        compared += len(runs)
        engines = {e for e, _, _ in runs}
        majority = {}
        for nr in {nr for _, _, v in runs for nr in v["avgresults"] if nr not in skip}:
            per_engine = {}
            for e, _, v in runs:
                if nr in v["avgresults"]:
                    per_engine.setdefault(e, []).append(round(v["avgresults"][nr], 2))
            if len(engines) > 1:
                # an engine counts once, and only when its own runs agree
                ref = reference([xs[0] for xs in per_engine.values() if len(set(xs)) == 1])
            else:
                ref = reference(next(iter(per_engine.values())))
            if ref is not None:
                majority[nr] = ref
        for e, c, v in runs:
            diff = sorted((nr for nr, x in v["avgresults"].items() if nr in majority and round(x, 2) != majority[nr]),
                          key=int)
            if diff:
                v["status"] = "wrong-answer"
                v["disagreeing_queries"] = diff
                bad += 1
                print(f"{e} x{c}: average result counts differ from the majority on queries {', '.join(diff)}")
    save(path, d)
    print(f"{compared} runs compared, {bad} disagree")


if __name__ == "__main__":
    if sys.argv[1] == "--check":
        check(sys.argv[2], set(sys.argv[3].split(",")) if len(sys.argv) > 3 else set())
    else:
        xml, out, engine, clients, name = sys.argv[1:6]
        add(xml, out, engine, clients, name, int(sys.argv[6]) if len(sys.argv) > 6 else 0)
