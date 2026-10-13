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

The driver does not look at what a query returns, so a query that fails quickly, or that
answers wrongly and quickly, makes the query mixes per hour look better. The second form compares
the average result count of every query across all runs. The driver draws the same queries for
a given number of mixes and seed, whatever the number of clients, so a correct engine returns
the same averages in every run. A run that differs from the majority on any query gets status
`wrong-answer` and the numbers of those queries in `disagreeing_queries`.
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
    entry = {"query": name, "status": "error", "qmph": None}
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
        entry = {"query": name, "status": status, "qmph": qmph, "runs": runs, "seconds": seconds,
                 "timeouts": timeouts, "avgresults": avg}
    except (OSError, ET.ParseError, AttributeError, ValueError, TypeError):
        pass
    d = load(out)
    d.setdefault(engine, {})[clients] = entry
    save(out, d)
    print(f"{engine} x{clients}: {entry['status']}" + (f", {entry['qmph']:.0f} QMpH" if entry["qmph"] else ""))


def check(path, skip=()):
    d = load(path)
    runs = [(e, c, v) for e, per in d.items() for c, v in per.items() if v.get("avgresults")]
    votes = {}
    for _, _, v in runs:
        for nr, x in v["avgresults"].items():
            if nr in skip:
                continue
            votes.setdefault(nr, []).append(round(x, 2))
    majority = {}
    for nr, xs in votes.items():
        top = Counter(xs).most_common()
        if len(xs) > 1 and (len(top) == 1 or top[0][1] > top[1][1]):
            majority[nr] = top[0][0]
    bad = 0
    for e, c, v in runs:
        diff = sorted((nr for nr, x in v["avgresults"].items() if nr in majority and round(x, 2) != majority[nr]),
                      key=int)
        if diff:
            v["status"] = "wrong-answer"
            v["disagreeing_queries"] = diff
            bad += 1
            print(f"{e} x{c}: average result counts differ from the majority on queries {', '.join(diff)}")
    save(path, d)
    print(f"{len(runs)} runs compared, {bad} disagree")


if __name__ == "__main__":
    if sys.argv[1] == "--check":
        check(sys.argv[2], set(sys.argv[3].split(",")) if len(sys.argv) > 3 else set())
    else:
        xml, out, engine, clients, name = sys.argv[1:6]
        add(xml, out, engine, clients, name, int(sys.argv[6]) if len(sys.argv) > 6 else 0)
