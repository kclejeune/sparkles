"""Tests of scripts/bench-record.py and `bench-regressions.py compare`, on fixture results.

    python3 -m unittest discover -s scripts/bench-tests
"""

import copy
import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RECORD = os.path.join(SCRIPTS, "bench-record.py")
REGRESSIONS = os.path.join(SCRIPTS, "bench-regressions.py")


def hyperfine(engine, times, codes=None):
    return {"command": engine, "mean": sum(times) / len(times), "stddev": 0.0, "times": times,
            "exit_codes": codes or [0] * len(times)}


def jload(path):
    with open(path) as f:
        return json.load(f)


def write_text(path, text):
    with open(path, "w") as f:
        f.write(text)


def write_path(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f)


def write(d, name, obj):
    with open(os.path.join(d, name), "w") as f:
        json.dump(obj, f)


def fixture(d, slow=1.0, wrong=False):
    """A results directory with two queries, three engines and one wrong engine answer."""
    s = lambda base: [base * slow * (1 + 0.01 * i) for i in range(5)]
    write(d, "q1.json", {"results": [hyperfine("sparkles", s(0.010)), hyperfine("jena-fuseki", s(0.030)),
                                     hyperfine("oxigraph", s(0.020))]})
    write(d, "q2.json", {"results": [hyperfine("sparkles", s(0.100)), hyperfine("jena-fuseki", [0.5, 0.5], [0, 28]),
                                     hyperfine("oxigraph", s(0.200))]})
    ans = lambda v, rows=3: {"rows": rows, "exact": v, "value": v}
    write(d, "answers.json", {
        "q1": {"sparkles": ans("aaa"), "jena-fuseki": ans("aaa"), "oxigraph": ans("bbb" if wrong else "aaa")},
        "q2": {"sparkles": ans("ccc"), "jena-fuseki": {"rows": "error"}, "oxigraph": ans("ccc")},
    })
    write(d, "load-details.json", {"sparkles": {"seconds": 1.5, "max_rss_kib": 1000, "index_bytes": 5}})
    write(d, "rss.json", {"sparkles": 100, "oxigraph": 200})
    write(d, "cold.json", {"sparkles": {"q1": 0.05}})
    write(d, "cold-runs.json", {"sparkles": {"q1": {"times": [0.04, 0.05, "error"], "ready": [0.3, 0.3, 0.3]}}})
    write(d, "qmph.json", {"sparkles": {"1": {"qmph": 1234.5, "query": "explore-mix"}}})


def build(d, out, **kw):
    cmd = [sys.executable, RECORD, "build", "--suite", "test", "--scale", "tiny", "--results", d, "--out", out]
    cmd += sum(([f"--{k}"] if v is True else [f"--{k}", str(v)] for k, v in kw.items()), [])
    return subprocess.run(cmd, capture_output=True, text=True)


class RecordTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.res = os.path.join(self.tmp.name, "results")
        os.mkdir(self.res)

    def record(self, **kw):
        fixture(self.res, **kw)
        out = os.path.join(self.tmp.name, "record.json")
        p = build(self.res, out)
        self.assertEqual(p.returncode, 0, p.stderr)
        return jload(out), out

    def find(self, rec, engine, query, mode="warm"):
        return next(r for r in rec["results"] if (r["engine"], r["query"], r["mode"]) == (engine, query, mode))

    def test_shape_and_validation(self):
        rec, out = self.record()
        self.assertEqual(rec["record"], 1)
        self.assertEqual({r["engine"] for r in rec["results"]}, {"sparkles", "jena", "oxigraph"})
        r = self.find(rec, "sparkles", "q1")
        self.assertEqual(len(r["samples_ms"]), 5)
        self.assertAlmostEqual(r["median_ms"], 10.2, places=1)
        self.assertEqual(r["answer"]["agrees"], True)
        self.assertEqual(rec["load"]["sparkles"]["seconds"], 1.5)
        self.assertEqual(rec["memory"]["oxigraph"]["rss_after_mib"], 200)
        self.assertEqual(subprocess.run([sys.executable, RECORD, "validate", out]).returncode, 0)

    def test_timeout_error_and_cold_and_qmph(self):
        rec, _ = self.record()
        self.assertEqual(self.find(rec, "jena", "q2")["status"], "timeout")
        self.assertIsNone(self.find(rec, "jena", "q2")["answer"])
        cold = self.find(rec, "sparkles", "q1", "cold")
        self.assertEqual(cold["status"], "error")
        self.assertEqual(len(cold["samples_ms"]), 2)
        self.assertEqual(cold["ready_s"], [0.3, 0.3, 0.3])
        q = self.find(rec, "sparkles", "explore-mix", "qmph")
        self.assertEqual(q["qmph"], 1234.5)

    def test_wrong_answer_is_marked(self):
        rec, _ = self.record(wrong=True)
        self.assertEqual(self.find(rec, "oxigraph", "q1")["status"], "wrong-answer")
        self.assertEqual(self.find(rec, "oxigraph", "q1")["answer"]["agrees"], False)
        self.assertEqual(self.find(rec, "sparkles", "q1")["status"], "ok")

    def test_instances_pool_into_one_result_per_template(self):
        write(self.res, "t-1.json", {"results": [hyperfine("sparkles", [0.001, 0.002, 0.003])]})
        write(self.res, "t-2.json", {"results": [hyperfine("sparkles", [0.004, 0.005, 0.006])]})
        write(self.res, "answers.json", {"t-1": {"sparkles": {"rows": 1, "exact": "a", "value": "a"}},
                                         "t-2": {"sparkles": {"rows": 2, "exact": "b", "value": "b"}}})
        out = os.path.join(self.tmp.name, "pooled.json")
        p = build(self.res, out, **{"pool-instances": True})
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        self.assertEqual([r["query"] for r in rec["results"]], ["t"])
        r = rec["results"][0]
        self.assertEqual(r["instances"], 2)
        self.assertEqual(len(r["samples_ms"]), 6)
        self.assertEqual(r["median_ms"], 3.5)
        self.assertEqual(r["p95_ms"], 6)
        self.assertEqual(r["answer"]["rows"], 3)

    def test_validate_rejects_a_broken_record(self):
        rec, out = self.record()
        bad = copy.deepcopy(rec)
        bad["results"][0]["status"] = "fine"
        del bad["results"][1]["clients"]
        path = os.path.join(self.tmp.name, "bad.json")
        write_path(path, bad)
        p = subprocess.run([sys.executable, RECORD, "validate", path], capture_output=True, text=True)
        self.assertEqual(p.returncode, 1)
        self.assertIn("status", p.stderr)
        self.assertIn("clients", p.stderr)

    def test_manifest(self):
        path = os.path.join(self.tmp.name, "manifest.json")
        binary = os.path.join(self.tmp.name, "bin")
        write_text(binary, "#!/bin/sh\necho 'tool 1.2'\n")
        os.chmod(binary, 0o755)
        data = os.path.join(self.tmp.name, "data.nt")
        write_text(data, "<a> <b> <c> .\n")
        p = subprocess.run([sys.executable, RECORD, "manifest", "--out", path, "--suite", "t", "--sparkles", binary,
                            "--engine", f"jena-fuseki={binary}", "--data", f"d={data}|CC0|scale=1",
                            "--memory-limit", "12G", "--workdir", self.tmp.name], capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        m = jload(path)
        self.assertEqual(m["sparkles"]["version"], "tool 1.2")
        self.assertEqual(len(m["sparkles"]["sha256"]), 64)
        self.assertEqual(m["engines"]["jena"]["license"], "Apache-2.0")
        self.assertEqual(m["data"]["d"]["license"], "CC0")
        self.assertEqual(m["memory_limit"], "12G")
        self.assertIsNone(m["ended"])
        subprocess.run([sys.executable, RECORD, "finish", path], check=True)
        self.assertIsNotNone(jload(path)["ended"])


class CompareTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def rec(self, name, slow_q1=1.0, slow_q2=1.0, wrong=False):
        d = os.path.join(self.tmp.name, name + "-dir")
        os.mkdir(d)
        fixture(d, wrong=wrong)
        # q1 and q2 of Sparkles are scaled separately
        for q, k in (("q1", slow_q1), ("q2", slow_q2)):
            j = jload(os.path.join(d, q + ".json"))
            for r in j["results"]:
                if r["command"] == "sparkles":
                    r["times"] = [t * k for t in r["times"]]
            write(d, q + ".json", j)
        out = os.path.join(self.tmp.name, name + ".json")
        p = build(d, out)
        self.assertEqual(p.returncode, 0, p.stderr)
        return out

    def compare(self, *recs, **kw):
        cmd = [sys.executable, REGRESSIONS, "compare", *recs] + sum(([f"--{k}"] if v is True else [f"--{k}", str(v)] for k, v in kw.items()), [])
        return subprocess.run(cmd, capture_output=True, text=True)

    def test_same_record_has_no_findings(self):
        a = self.rec("a")
        p = self.compare(a, a)
        self.assertEqual(p.returncode, 0, p.stdout)
        self.assertIn("0 finding(s)", p.stdout)

    def test_regression_found(self):
        a, b = self.rec("a"), self.rec("b", slow_q1=1.15)
        p = self.compare(a, b)
        self.assertEqual(p.returncode, 1, p.stdout)
        regressions = [l for l in p.stdout.splitlines() if l.startswith("REGRESSION")]
        self.assertEqual(len(regressions), 1, p.stdout)
        self.assertIn("sparkles q1", regressions[0])

    def test_overlapping_ranges_are_not_a_regression(self):
        a = self.rec("a")
        # 8% slower: below the threshold of one comparison, and the chain rule needs three records
        b = self.rec("b", slow_q1=1.08)
        self.assertEqual(self.compare(a, b).returncode, 0)

    def test_two_consecutive_small_slowdowns(self):
        a, b, c = self.rec("a"), self.rec("b", slow_q1=1.06), self.rec("c", slow_q1=1.13)
        p = self.compare(a, b, c)
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("two consecutive", p.stdout)

    def test_changed_answer_hash(self):
        a, b = self.rec("a"), self.rec("b", wrong=True)
        p = self.compare(a, b)
        self.assertEqual(p.returncode, 1)
        self.assertIn("ANSWER CHANGED oxigraph q1", p.stdout)
        self.assertEqual(self.compare(a, b, **{"no-fail": True}).returncode, 0)


class LibTests(unittest.TestCase):
    """Shell functions of scripts/bench-lib.sh, called through bash."""

    def bash(self, queries, script):
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "queries"))
            for name, text in queries.items():
                write_text(os.path.join(d, "queries", name + ".rq"), text)
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            p = subprocess.run(["bash", "-c", f'source "{lib}"; {script}'], cwd=d, env=env,
                               capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            return p.stdout.strip()

    def test_accept_follows_the_query_form(self):
        qs = {"s": "PREFIX a: <http://x/select#> SELECT * { ?s ?p ?o }", "a": "# ask\nASK { ?s ?p ?o }",
              "c": "CONSTRUCT { ?s ?p ?o } WHERE { ?s ?p ?o }", "d": "DESCRIBE <http://x/>"}
        out = self.bash(qs, "for n in s a c d; do accept_of $n; done")
        self.assertEqual(out.split(), ["text/tab-separated-values", "application/sparql-results+json",
                                       "application/n-triples", "application/n-triples"])

    def test_timed_request_and_cold_request_ask_for_the_same_format(self):
        qs = {"a": "ASK { ?s ?p ?o }"}
        out = self.bash(qs, "q localhost:1/x a")
        self.assertIn("Accept: application/sparql-results+json", out)
        with open(os.path.join(SCRIPTS, "bench-lib.sh")) as f:
            lib = f.read()
        self.assertIn('-H "Accept: $(accept_of "$n")"', lib)


if __name__ == "__main__":
    unittest.main()
