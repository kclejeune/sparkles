#!/usr/bin/env python3
"""Write the query instances that the BSBM test driver would send, one file each.

    instantiate.py --tools DIR --usecase explore|bi --data td_data --out queries --instances 3 \
                   --java java [--runs 20] [--port 48390]

The BSBM driver draws the parameters of its query templates from the files the data
generator leaves in its data directory, with a fixed seed. This script runs the driver once
against a stub endpoint that records every query it receives and answers it with an empty
result. It then matches each recorded query to the template it came from (the template text
with each `%Name%` placeholder as a wildcard) and writes the first --instances distinct
instances of each template as `<out>/<usecase>-q<N>-<i>.rq`. The names go to standard output,
one per line, in template order.

The queries are the driver's, so the answer check and the per-query timings of
scripts/bench-bsbm.sh use exactly the parameters of the driver's own query mixes. The tools
directory is the unpacked BSBM tools (GPL-3.0), run as a separate process.
"""

import argparse
import http.server
import os
import re
import subprocess
import sys
import threading
import urllib.parse

p = argparse.ArgumentParser()
p.add_argument("--tools", required=True)
p.add_argument("--usecase", required=True, choices=["explore", "bi"])
p.add_argument("--data", default="td_data")
p.add_argument("--out", required=True)
p.add_argument("--instances", type=int, default=3)
p.add_argument("--runs", type=int, default=20)
p.add_argument("--java", default="java")
p.add_argument("--port", type=int, default=48390)
a = p.parse_args()

USECASE_FILE = {"explore": "usecases/explore/sparql.txt", "bi": "usecases/businessIntelligence/sparql.txt"}[a.usecase]
mixdir = None
for line in open(os.path.join(a.tools, USECASE_FILE)):
    if line.startswith("querymix="):
        mixdir = line.strip().split("=", 1)[1]
if not mixdir:
    sys.exit(f"no querymix in {USECASE_FILE}")
ignored = set()
ign = os.path.join(a.tools, mixdir, "ignoreQueries.txt")
if os.path.exists(ign):
    ignored = {int(x) for x in open(ign).read().split() if x.isdigit()}

templates = {}
for f in sorted(os.listdir(os.path.join(a.tools, mixdir))):
    m = re.fullmatch(r"query(\d+)\.txt", f)
    if m:
        text = " ".join(open(os.path.join(a.tools, mixdir, f)).read().split())
        pattern = "".join(".+?" if part.startswith("%") else re.escape(part)
                          for part in re.split(r"(%\w+%)", text) if part)
        templates[int(m.group(1))] = re.compile(pattern, re.S)

captured = []


class Stub(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def respond(self, query):
        captured.append(query)
        if "rdf+xml" in (self.headers.get("Accept") or ""):
            body = b'<?xml version="1.0"?><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"/>'
            kind = "application/rdf+xml"
        else:
            body = (b'<?xml version="1.0"?><sparql xmlns="http://www.w3.org/2005/sparql-results#">'
                    b"<head/><results/></sparql>")
            kind = "application/sparql-results+xml"
        self.send_response(200)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        q = urllib.parse.parse_qs(urllib.parse.urlparse(self.path).query).get("query", [""])[0]
        self.respond(q)

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        q = urllib.parse.parse_qs(self.rfile.read(n).decode()).get("query", [""])[0]
        self.respond(q)


server = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), Stub)
threading.Thread(target=server.serve_forever, daemon=True).start()
cp = ".:" + os.path.join(a.tools, "lib", "*")
r = subprocess.run([a.java, "-Xmx512m", "-cp", cp, "benchmark.testdriver.TestDriver", "-runs", str(a.runs), "-w", "0",
                    "-idir", a.data, "-ucf", USECASE_FILE, "-o", os.devnull, f"http://127.0.0.1:{a.port}/sparql"],
                   cwd=a.tools, capture_output=True, text=True)
server.shutdown()
if r.returncode != 0 or not captured:
    sys.exit(f"the test driver failed ({r.returncode}): {r.stdout[-500:]} {r.stderr[-500:]}")

seen = {n: [] for n in templates}
unmatched = 0
for q in captured:
    flat = " ".join(q.split())
    for n, rx in templates.items():
        if rx.fullmatch(flat):
            if q not in seen[n]:
                seen[n].append(q)
            break
    else:
        unmatched += 1
if unmatched:
    sys.exit(f"{unmatched} of {len(captured)} captured queries match no template of {mixdir}")
os.makedirs(a.out, exist_ok=True)
for n in sorted(seen):
    if n in ignored and not seen[n]:
        continue
    for i, q in enumerate(seen[n][: a.instances], 1):
        name = f"{a.usecase}-q{n}-{i}"
        with open(os.path.join(a.out, name + ".rq"), "w") as f:
            f.write(q.rstrip() + "\n")
        print(name)
