"""Tests of the long write load (scripts/bench-longwrite/longrun.py), its record mode in
scripts/bench-record.py, and the OWL2Bench query extraction (scripts/bench-owl2bench/queries.py).

    python3 -m unittest discover -s scripts/bench-tests
"""

import http.server
import json
import os
import re
import subprocess
import sys
import tempfile
import threading
import unittest
import urllib.parse

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LONGRUN = os.path.join(SCRIPTS, "bench-longwrite", "longrun.py")
RECORD = os.path.join(SCRIPTS, "bench-record.py")
QUERIES = os.path.join(SCRIPTS, "bench-owl2bench", "queries.py")
def jload(path, raw=False):
    with open(path) as f:
        return f.read() if raw else json.load(f)


TRIPLE = re.compile(r'<(http://example\.org/lw/\d+/\d+)> <http://example\.org/lwValue> "\d+"')


class Store(http.server.ThreadingHTTPServer):
    """A stand-in SPARQL server: it applies the INSERT DATA and DELETE DATA of the load to a
    set of subjects, fails every `fail_every`-th update without applying it, and answers any
    query with the subjects as TSV."""

    daemon_threads = True

    def __init__(self, fail_every=0):
        super().__init__(("127.0.0.1", 0), Handler)
        self.subjects, self.lock, self.updates, self.fail_every = set(), threading.Lock(), 0, fail_every


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length", 0))
        form = urllib.parse.parse_qs(self.rfile.read(n).decode())
        st = self.server
        if "update" in form:
            u = form["update"][0]
            with st.lock:
                st.updates += 1
                if st.fail_every and st.updates % st.fail_every == 0:
                    self.send_response(500)
                    self.end_headers()
                    return
                subs = TRIPLE.findall(u)
                if u.startswith("INSERT"):
                    st.subjects.update(subs)
                else:
                    st.subjects.difference_update(subs)
            body = b""
        else:
            with st.lock:
                body = ("?s\n" + "".join(f"<{s}>\n" for s in sorted(st.subjects))).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/tab-separated-values")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class LongrunTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        qdir = os.path.join(self.tmp.name, "queries")
        os.mkdir(qdir)
        with open(os.path.join(qdir, "count.rq"), "w") as f:
            f.write("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }")
        self.qdir = qdir

    def serve(self, **kw):
        srv = Store(**kw)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        self.addCleanup(srv.server_close)
        self.addCleanup(srv.shutdown)
        return srv, f"127.0.0.1:{srv.server_address[1]}/ds"

    def run_load(self, url, out, *extra):
        cmd = [sys.executable, LONGRUN, "run", "--query-url", url + "/sparql", "--update-url", url + "/update",
               "--queries", self.qdir, "--names", "count", "--seconds", "2", "--readers", "1", "--writers", "2",
               "--batch", "5", "--live-max", "40", "--sample", "0.5", "--out", out, *extra]
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=120)
        self.assertEqual(p.returncode, 0, p.stderr)
        return json.loads(p.stdout)

    def check(self, url, out):
        p = subprocess.run([sys.executable, LONGRUN, "check", "--query-url", url + "/sparql", "--state", out],
                           capture_output=True, text=True, timeout=60)
        return p.returncode, json.loads(p.stdout)

    def test_the_writers_state_matches_the_server(self):
        srv, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        totals = self.run_load(url, out)
        self.assertGreater(totals["writes"], 0)
        self.assertGreater(totals["reads"], 0)
        code, r = self.check(url, out)
        self.assertEqual(code, 0, r)
        self.assertEqual(r["found"], len(srv.subjects))
        self.assertEqual(r["missing"] + r["extra"] + r["uncertain"], 0)
        d = jload(out)
        self.assertTrue(all(s["minute"] >= 0 for s in d["series"]))
        self.assertEqual(sum(s["writes"] for s in d["series"]), totals["writes"])
        # deletes keep each writer near its limit: 40 live triples and one batch of 5
        self.assertLessEqual(len(srv.subjects), 2 * (40 + 5))

    def test_failed_writes_are_uncertain_not_wrong(self):
        srv, url = self.serve(fail_every=7)
        out = os.path.join(self.tmp.name, "run.json")
        totals = self.run_load(url, out)
        self.assertGreater(totals["write_errors"], 0)
        code, r = self.check(url, out)
        self.assertEqual(code, 0, r)
        self.assertGreater(r["uncertain"], 0)

    def test_a_lost_write_is_found(self):
        srv, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        self.run_load(url, out)
        with srv.lock:
            srv.subjects.pop()
            srv.subjects.add("http://example.org/lw/9/9")
        code, r = self.check(url, out)
        self.assertEqual(code, 1)
        self.assertEqual((r["missing"], r["extra"]), (1, 1))


def series(n):
    return [{"minute": m, "writes": 100, "triples_written": 5000, "write_errors": 0, "write_timeouts": 0,
             "write_p50_ms": 1.0 + m, "write_p99_ms": 4.0, "reads": 200, "read_errors": 0, "read_timeouts": 0,
             "read_p50_ms": 2.0, "read_p99_ms": 6.0, "rss_mib": 100.0, "rss_max_mib": 120.0, "disk_mib": 10.0,
             "device_writes": 150, "device_flushes": 1, "compacting": m == 1} for m in range(n)]


def longwrite(ok_reopen=True):
    run = lambda dur, ok: {
        "settings": {"seconds": 180, "readers": 16, "writers": 2}, "device": "dm-0", "device_writes": 450,
        "device_flushes": 3, "totals": {"writes": 300, "triples_written": 15000, "write_errors": 0, "reads": 600,
                                        "read_errors": 0, "read_timeouts": 0},
        "series": series(3), "compactions": [{"seen_at_s": 70.0, "mode": "full", "seconds": 0.2, "lockMs": 4.0}],
        "compaction_counters": {"automaticRuns": 1, "failures": 0}, "durability": dur,
        "check": {"ok": ok}, "check_reopen": {"ok": ok_reopen}, "restart_ready_s": 0.5}
    return {"sparkles": run("synced", True), "qlever": run("memory", False)}


class LongwriteRecordTests(unittest.TestCase):
    def test_record_and_summary(self):
        with tempfile.TemporaryDirectory() as d:
            res = os.path.join(d, "results")
            os.mkdir(res)
            with open(os.path.join(res, "longwrite.json"), "w") as f:
                json.dump(longwrite(), f)
            # the raw run files sit next to it and are not queries
            with open(os.path.join(res, "run-sparkles.json"), "w") as f:
                json.dump({"series": []}, f)
            out = os.path.join(d, "record.json")
            p = subprocess.run([sys.executable, RECORD, "build", "--suite", "longwrite", "--scale", "t", "--results",
                                res, "--out", out, "--queries"], capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            rec = jload(out)
            self.assertEqual(len(rec["results"]), 4)
            w = next(r for r in rec["results"] if (r["engine"], r["query"]) == ("sparkles", "longwrite-write"))
            self.assertEqual((w["mode"], w["clients"], w["durability"], w["status"]), ("longrun", 2, "synced", "ok"))
            self.assertEqual(w["samples_ms"], [1.0, 2.0, 3.0])
            self.assertEqual(len(w["series"]), 3)
            self.assertAlmostEqual(w["qps"], 300 / 180, places=2)
            r = next(r for r in rec["results"] if (r["engine"], r["query"]) == ("qlever", "longwrite-read"))
            self.assertEqual((r["clients"], r["durability"], r["status"]), (16, None, "error"))
            v = subprocess.run([sys.executable, RECORD, "validate", out], capture_output=True, text=True)
            self.assertEqual(v.returncode, 0, v.stderr)
            s = subprocess.run([sys.executable, LONGRUN, "summary", os.path.join(res, "longwrite.json")],
                               capture_output=True, text=True)
            self.assertEqual(s.returncode, 0, s.stderr)
            self.assertIn("| sparkles | synced | 1.7 |", s.stdout)
            self.assertIn("| 1.50 / 0.01 | 1 | ok | ok |", s.stdout)


class Owl2benchQueryTests(unittest.TestCase):
    def test_profiles_select_the_queries(self):
        text = ["OWL2Bench  SPARQL  Queries", ""]
        for i in range(1, 23):
            profiles = "EL, QL, DL" if i == 1 else ("DL" if i in (17, 18) else "EL, QL, RL, DL")
            text += [f"SELECT  DISTINCT  ?x  WHERE {{ ?x  rdf:type  :C{i} }}", f"Description: query {i}.",
                     f"Profile: {profiles}", ""]
        with tempfile.TemporaryDirectory() as d:
            src = os.path.join(d, "q.txt")
            with open(src, "w") as f:
                f.write("\n".join(text))
            p = subprocess.run([sys.executable, QUERIES, src, os.path.join(d, "out"), "rl"], capture_output=True,
                               text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            names = p.stdout.split()
            self.assertEqual(len(names), 19)
            self.assertNotIn("q1", names)
            self.assertNotIn("q17", names)
            q = jload(os.path.join(d, "out", "q2.rq"), raw=True)
            self.assertIn("PREFIX : <http://benchmark/OWL2Bench#>", q)
            self.assertIn("SELECT DISTINCT ?x WHERE { ?x rdf:type :C2 }", q)
            with open(src, "w") as f:
                f.write("\n".join(text[:-8]))
            p = subprocess.run([sys.executable, QUERIES, src, os.path.join(d, "out2"), "RL"], capture_output=True,
                               text=True)
            self.assertNotEqual(p.returncode, 0)


if __name__ == "__main__":
    unittest.main()
