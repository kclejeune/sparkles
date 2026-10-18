"""Tests of the long write load (scripts/bench-longwrite/longrun.py), its record mode in
scripts/bench-record.py, and the OWL2Bench query extraction (scripts/bench-owl2bench/queries.py).

    python3 -m unittest discover -s scripts/bench-tests
"""

import http.server
import importlib.util
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.parse

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LONGRUN = os.path.join(SCRIPTS, "bench-longwrite", "longrun.py")
RECORD = os.path.join(SCRIPTS, "bench-record.py")
QUERIES = os.path.join(SCRIPTS, "bench-owl2bench", "queries.py")
TRIPLE = re.compile(r'<(http://example\.org/lw/\d+/(\d+))> <http://example\.org/lwValue> "\d+"')

spec = importlib.util.spec_from_file_location("longrun", LONGRUN)
longrun = importlib.util.module_from_spec(spec)
spec.loader.exec_module(longrun)


def jload(path, raw=False):
    with open(path) as f:
        return f.read() if raw else json.load(f)


class Store(http.server.ThreadingHTTPServer):
    """A stand-in SPARQL server: it applies the INSERT DATA and DELETE DATA of the load to a
    map of subjects to values, fails every `fail_every`-th update without applying it, answers
    a SPARQL JSON request with the subjects and values, and any other query with TSV."""

    daemon_threads = True

    def __init__(self, fail_every=0):
        super().__init__(("127.0.0.1", 0), Handler)
        self.subjects, self.lock, self.updates, self.fail_every = {}, threading.Lock(), 0, fail_every


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def reply(self, body, ctype):
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

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
                for s, v in TRIPLE.findall(u):
                    if u.startswith("INSERT"):
                        st.subjects[s] = v
                    else:
                        st.subjects.pop(s, None)
            self.reply(b"", "text/plain")
            return
        with st.lock:
            items = sorted(st.subjects.items())
        if "sparql-results+json" in self.headers.get("Accept", ""):
            rows = [{"s": {"type": "uri", "value": s}, "o": {"type": "literal", "value": v}} for s, v in items]
            self.reply(json.dumps({"head": {"vars": ["s", "o"]}, "results": {"bindings": rows}}).encode(),
                       "application/sparql-results+json")
        else:
            self.reply(("?s\n" + "".join(f"<{s}>\n" for s, _ in items)).encode(), "text/tab-separated-values")


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

    def cmd(self, url, out, seconds=2, *extra):
        return [sys.executable, LONGRUN, "run", "--query-url", url + "/sparql", "--update-url", url + "/update",
                "--queries", self.qdir, "--names", "count", "--seconds", str(seconds), "--readers", "1",
                "--writers", "2", "--batch", "5", "--live-max", "40", "--sample", "0.5", "--disk-every", "0.5",
                "--out", out, *extra]

    def run_load(self, url, out, *extra):
        p = subprocess.run(self.cmd(url, out, 2, *extra), capture_output=True, text=True, timeout=120)
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
        self.assertEqual(r["missing"] + r["extra"] + r["uncertain"] + r["malformed"], 0)
        d = jload(out)
        self.assertTrue(all(s["minute"] >= 0 for s in d["series"]))
        self.assertEqual(sum(s["writes"] for s in d["series"]), totals["writes"])
        self.assertIsNone(d["stopped_early"])
        # deletes keep each writer near its limit: 40 live triples and one batch of 5
        self.assertLessEqual(len(srv.subjects), 2 * (40 + 5))

    def test_idle_baseline_is_subtracted(self):
        _, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        self.run_load(url, out, "--store", self.tmp.name, "--baseline", "0.5")
        d = jload(out)
        if d["device"] is None:
            self.skipTest("the temporary directory is not on a block device")
        b = d["device_baseline"]
        self.assertGreaterEqual(b["seconds"], 0.5)
        if d["device_writes"] is not None:
            idle = b["device_writes"] / b["seconds"] * d["device_seconds"]
            self.assertAlmostEqual(d["device_writes_net"], d["device_writes"] - idle, delta=0.3 * idle + 1)  # the seconds are rounded

    def test_failed_writes_are_uncertain_not_wrong(self):
        srv, url = self.serve(fail_every=7)
        out = os.path.join(self.tmp.name, "run.json")
        totals = self.run_load(url, out)
        self.assertGreater(totals["write_errors"], 0)
        code, r = self.check(url, out)
        self.assertEqual(code, 0, r)
        self.assertGreater(r["uncertain"], 0)

    def test_a_lost_write_and_a_wrong_value_are_found(self):
        srv, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        self.run_load(url, out)
        with srv.lock:
            k = sorted(srv.subjects)[0]
            del srv.subjects[k]
            srv.subjects["http://example.org/lw/9/9"] = "9"
            k2 = sorted(srv.subjects)[0]
            srv.subjects[k2] = "not-the-value"
        code, r = self.check(url, out)
        self.assertEqual(code, 1)
        self.assertEqual((r["missing"], r["extra"], r["malformed"]), (2, 1, 1))

    def test_a_dead_server_is_not_flooded_and_stops_the_run(self):
        # a port nothing listens on: the workers back off, and the run ends early
        import socket
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        url = f"127.0.0.1:{port}/ds"
        out = os.path.join(self.tmp.name, "run.json")
        t = time.time()
        p = subprocess.run(self.cmd(url, out, 60, "--port", str(port), "--uncertain-max", "20"),
                           capture_output=True, text=True, timeout=120)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertLess(time.time() - t, 40)
        d = jload(out)
        self.assertIn("stopped listening", d["stopped_early"])
        # backoff: a few dozen attempts per worker, not hundreds of thousands
        self.assertLess(d["totals"]["read_errors"] + d["totals"]["write_errors"], 200)
        self.assertLessEqual(len(d["state"]["uncertain"]), 2 * 20)
        self.assertGreater(d["state"]["uncertain_overflow"], 0)

    def test_workers_die_with_the_coordinator(self):
        srv, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        p = subprocess.Popen(self.cmd(url, out, 60), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
        self.addCleanup(lambda: subprocess.run(["pkill", "-9", "-s", str(p.pid)], check=False))
        time.sleep(4)
        kids = subprocess.run(["pgrep", "-P", str(p.pid)], capture_output=True, text=True).stdout.split()
        self.assertGreaterEqual(len(kids), 3)
        p.send_signal(signal.SIGKILL)
        p.wait()
        time.sleep(1)
        alive = [k for k in kids if os.path.exists(f"/proc/{k}") and "Z" not in
                 open(f"/proc/{k}/stat").read().split(")")[-1].split()[0]]
        self.assertEqual(alive, [])

    def test_sigterm_ends_the_run_with_its_state(self):
        srv, url = self.serve()
        out = os.path.join(self.tmp.name, "run.json")
        p = subprocess.Popen(self.cmd(url, out, 60), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                             start_new_session=True)
        self.addCleanup(lambda: subprocess.run(["pkill", "-9", "-s", str(p.pid)], check=False))
        time.sleep(5)
        p.send_signal(signal.SIGTERM)
        stdout, stderr = p.communicate(timeout=60)
        self.assertEqual(p.returncode, 0, stderr)
        self.assertIn("signal", json.loads(stdout)["stopped_early"])
        code, r = self.check(url, out)
        self.assertEqual(code, 0, r)


DISKSTATS = """ 259       0 nvme0n1 4426961 1271426 393548044 1097395 331719370 0 6627093880 194598128 0 61099853 223216203 1001064 0 1611637504 4266333 21241991 23254346
 259       2 nvme0n1p2 4426700 1271426 393539308 1097343 331701110 0 6627093880 194587610 0 61107316 195684953 1001064 0 1611637504 4266333 0 0
 253       0 dm-0 5697968 0 393536860 2083064 331747400 0 6627093880 2186488628 0 61254150 2193004725 1001064 0 1611637504 3989819 0 0
"""


class DeviceTests(unittest.TestCase):
    def test_diskstats_fields(self):
        st = longrun.parse_diskstats(DISKSTATS)
        self.assertEqual(st["nvme0n1"], (331719370, 21241991))
        self.assertEqual(st["dm-0"], (331747400, 0))
        old = longrun.parse_diskstats(" 8 0 sda 1 2 3 4 5 6 7 8 9 10 11\n")
        self.assertEqual(old["sda"], (5, None))

    def test_whole_disks_follow_slaves_and_partitions(self):
        with tempfile.TemporaryDirectory() as d:
            dev = os.path.join(d, "devices")
            os.makedirs(os.path.join(dev, "nvme0n1", "nvme0n1p2"))
            open(os.path.join(dev, "nvme0n1", "nvme0n1p2", "partition"), "w").close()
            sysb = os.path.join(d, "block")
            os.makedirs(sysb)
            os.symlink(os.path.join(dev, "nvme0n1"), os.path.join(sysb, "nvme0n1"))
            os.symlink(os.path.join(dev, "nvme0n1", "nvme0n1p2"), os.path.join(sysb, "nvme0n1p2"))
            for name, slave in (("dm-0", "nvme0n1p2"), ("dm-2", "dm-0")):
                os.makedirs(os.path.join(dev, name, "slaves"))
                open(os.path.join(dev, name, "slaves", slave), "w").close()
                os.symlink(os.path.join(dev, name), os.path.join(sysb, name))
            self.assertEqual(longrun.whole_disks("dm-2", sysb), ["nvme0n1"])
            self.assertEqual(longrun.whole_disks("nvme0n1p2", sysb), ["nvme0n1"])
            self.assertEqual(longrun.whole_disks("nvme0n1", sysb), ["nvme0n1"])


def series(n):
    return [{"minute": m, "writes": 100, "triples_written": 5000, "write_errors": 0, "write_timeouts": 0,
             "write_p50_ms": 1.0 + m, "write_p99_ms": 4.0, "reads": 200, "read_errors": 0, "read_timeouts": 0,
             "read_p50_ms": 2.0, "read_p99_ms": 6.0, "rss_mib": 100.0, "rss_max_mib": 120.0, "disk_mib": 10.0,
             "device_writes": 150, "device_flushes": 100, "compacting": m == 1} for m in range(n)]


def run_file(path, writes=300, write_errors=0):
    with open(path, "w") as f:
        json.dump({
            "settings": {"seconds": 180, "readers": 16, "writers": 2}, "device": "dm-0", "flush_disks": ["nvme0n1"],
            "device_writes": 450, "device_flushes": 300, "device_writes_net": 300, "device_flushes_net": 150,
            "device_baseline": {"seconds": 60.0, "device_writes": 50, "device_flushes": 50}, "stopped_early": None,
            "totals": {"writes": writes, "triples_written": 50 * writes, "write_errors": write_errors, "reads": 600,
                       "read_errors": 0, "read_timeouts": 0},
            "series": series(3), "compactions": [{"seen_at_s": 70.0, "mode": "full", "seconds": 0.2, "lockMs": 4.0}],
            "compaction_counters": {"automaticRuns": 1, "failures": 0}, "state": {"live": [], "uncertain": []}}, f)


OK = '{"ok": true, "expected": 5, "found": 5, "uncertain": 0, "missing": 0, "extra": 0, "malformed": 0}'
LOST = '{"ok": false, "expected": 5, "found": 4, "uncertain": 0, "missing": 1, "extra": 0, "malformed": 0}'


class LongwriteRecordTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.res = os.path.join(self.tmp.name, "results")
        os.mkdir(self.res)
        self.lw = os.path.join(self.res, "longwrite.json")

    def merge(self, engine, run=None, check=OK, crash=OK, reopen=OK, answers="ok", **kw):
        path = os.path.join(self.res, f"run-{engine}.json")
        if run is not False:
            run_file(path, **(run or {}))
        p = subprocess.run([sys.executable, LONGRUN, "merge", "--into", self.lw, "--engine", engine, "--run",
                            path if run is not False else "", "--check", check, "--check-crash", crash,
                            "--check-reopen", reopen, "--restart-s", "0.5", "--crash-restart-s", "2.5",
                            "--answers", answers, "--answers-reason", kw.get("why", "")],
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        return jload(self.lw)[engine]

    def test_statuses(self):
        self.assertEqual(self.merge("sparkles")["status"], "ok")
        # QLever keeps updates in memory: losing them in a restart is its documented class
        self.assertEqual(self.merge("qlever", crash=LOST, reopen=LOST)["status"], "ok")
        v = self.merge("oxigraph", crash=LOST)
        self.assertEqual(v["status"], "error")
        self.assertIn("after SIGKILL", v["status_reasons"][0])
        self.assertEqual(self.merge("jena", reopen=LOST)["status"], "error")
        v = self.merge("fluree", run={"writes": 0, "write_errors": 500}, check='{"ok": true, "expected": 0}')
        self.assertEqual(v["status"], "error")
        self.assertIn("no write succeeded", v["status_reasons"])
        v = self.merge("sparkles", run={"writes": 1000, "write_errors": 20})
        self.assertIn("20 of 1020 write requests failed", v["status_reasons"])
        v = self.merge("sparkles", check='{"ok": false, "indeterminate": true}')
        self.assertIn("state check indeterminate", v["status_reasons"])
        self.assertEqual(self.merge("sparkles", run=False)["status"], "error")
        self.assertEqual(self.merge("sparkles", answers="wrong-answer")["status"], "wrong-answer")
        v = self.merge("qlever", answers="error", why="failed path-plus")
        self.assertEqual(v["status"], "error")
        self.assertEqual(v["status_reasons"], ["not run: it failed path-plus of the read set"])

    def test_record_and_summary(self):
        self.merge("sparkles")
        self.merge("qlever", crash=LOST, reopen=LOST)
        self.merge("oxigraph", reopen=LOST)
        out = os.path.join(self.tmp.name, "record.json")
        p = subprocess.run([sys.executable, RECORD, "build", "--suite", "longwrite", "--scale", "t", "--results",
                            self.res, "--out", out, "--queries"], capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        self.assertEqual(rec["record"], 2)
        self.assertEqual(len(rec["results"]), 6)
        w = next(r for r in rec["results"] if (r["engine"], r["query"]) == ("sparkles", "longwrite-write"))
        self.assertEqual((w["mode"], w["clients"], w["durability"], w["status"]), ("longrun", 2, "synced", "ok"))
        # the first minute is left out of the samples
        self.assertEqual(w["samples_ms"], [2.0, 3.0])
        self.assertEqual(len(w["series"]), 3)
        self.assertAlmostEqual(w["qps"], 300 / 180, places=2)
        r = next(r for r in rec["results"] if (r["engine"], r["query"]) == ("qlever", "longwrite-read"))
        self.assertEqual((r["clients"], r["durability"], r["status"]), (16, None, "ok"))
        o = next(r for r in rec["results"] if (r["engine"], r["query"]) == ("oxigraph", "longwrite-write"))
        self.assertEqual(o["status"], "error")
        v = subprocess.run([sys.executable, RECORD, "validate", out], capture_output=True, text=True)
        self.assertEqual(v.returncode, 0, v.stderr)
        s = subprocess.run([sys.executable, LONGRUN, "summary", self.lw], capture_output=True, text=True)
        self.assertEqual(s.returncode, 0, s.stderr)
        self.assertIn("| sparkles | synced | ok | 1.7 |", s.stdout)
        self.assertIn("| 1.00 / 0.50 | 1 | ok | ok | ok | 2.5 | 0.5 |", s.stdout)
        self.assertEqual((w["crash_restart_s"], w["device_writes_net"]), (2.5, 300))
        self.assertIn("| qlever | memory | ok |", s.stdout)
        self.assertIn("failed (1 missing, 0 extra)", s.stdout)


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
