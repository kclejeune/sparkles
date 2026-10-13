#!/usr/bin/env python3
"""Add one BSBM test driver run to results/qmph.json.

    qmph.py <driver-output.xml> <qmph.json> <engine> <clients> <query-name>

The driver (benchmark.testdriver.TestDriver) writes an XML file with the query mixes per
hour it measured and, per query, how often it ran and timed out. The file qmph.json maps an
engine and a client count to {qmph, runs, seconds, timeouts, query, status}, the form
scripts/bench-record.py reads. A missing or unreadable XML file records status `error`, and
a run in which any query timed out records `timeout`.
"""

import json
import os
import sys
import xml.etree.ElementTree as ET

xml, out, engine, clients, name = sys.argv[1:6]
entry = {"query": name, "status": "error", "qmph": None}
try:
    root = ET.parse(xml).getroot()
    mix = root.find("querymix")
    timeouts = sum(int(q.findtext("timeoutcount") or 0) for q in root.iter("query"))
    qmph = float(mix.findtext("qmph"))
    runs = int(mix.findtext("querymixruns"))
    seconds = float(mix.findtext("actualtotalruntime") or mix.findtext("totalruntime"))
    entry = {"query": name, "status": "timeout" if timeouts else "ok", "qmph": qmph, "runs": runs,
             "seconds": seconds, "timeouts": timeouts}
except (OSError, ET.ParseError, AttributeError, ValueError, TypeError):
    pass
d = {}
if os.path.exists(out):
    with open(out) as f:
        d = json.load(f)
d.setdefault(engine, {})[clients] = entry
with open(out, "w") as f:
    json.dump(d, f, indent=1)
print(f"{engine} x{clients}: {entry['status']}" + (f", {entry['qmph']:.0f} QMpH" if entry["qmph"] else ""))
