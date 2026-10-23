"""Tests of scripts/bench-record.py and `bench-regressions.py compare`, on fixture results.

    python3 -m unittest discover -s scripts/bench-tests
"""

import copy
import importlib.util
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
        self.assertEqual(rec["record"], 2)
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

    def test_unfinished_hyperfine_outputs_are_not_published(self):
        fixture(self.res)
        for name in ("q1.hf", "q1.qlever.hf", "q1.new", "throughput-q1.new"):
            write(self.res, name + ".json", {"results": [hyperfine("sparkles", [0.001])]})
        out = os.path.join(self.tmp.name, "record.json")
        result = build(self.res, out)
        self.assertEqual(result.returncode, 0, result.stderr)
        queries = {r["query"] for r in jload(out)["results"]}
        self.assertFalse(any(q.endswith((".hf", ".new")) for q in queries), queries)

    def test_qmph_variants_and_their_reasons_are_read(self):
        fixture(self.res)
        write(self.res, "qmph.json", {"qlever": {"4": {"qmph": None, "query": "explore-mix", "status": "error",
                                                       "status_reasons": ["HTTP 406"]}}})
        write(self.res, "qmph-select.json", {"qlever": {"4": {"qmph": 99.0, "query": "explore-mix-select",
                                                                   "status": "ok"}}})
        out = os.path.join(self.tmp.name, "record.json")
        p = build(self.res, out)
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        std = self.find(rec, "qlever", "explore-mix", "qmph")
        self.assertEqual((std["status"], std["status_reasons"]), ("error", ["HTTP 406"]))
        self.assertEqual(self.find(rec, "qlever", "explore-mix-select", "qmph")["qmph"], 99.0)
        # the variant file is not taken for a query of its own
        self.assertFalse(any(r["query"] == "qmph-select" for r in rec["results"]))
        self.assertEqual(subprocess.run([sys.executable, RECORD, "validate", out]).returncode, 0)

    def test_answers_only_runs_carry_their_fingerprints(self):
        write(self.res, "answers.json", {
            "q1": {"sparkles": {"rows": 3, "exact": "a", "value": "a"}, "oxigraph": {"rows": 3, "exact": "a", "value": "a"},
                   "qlever": {"rows": 2, "exact": "b", "value": "b"}, "jena-fuseki": {"rows": "error"}}})
        out = os.path.join(self.tmp.name, "answers-only.json")
        p = build(self.res, out, queries="q1")
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        r = {x["engine"]: x for x in rec["results"]}
        self.assertTrue(all(x["mode"] == "answer" for x in r.values()))
        self.assertEqual(r["sparkles"]["answer"]["hash"], r["oxigraph"]["answer"]["hash"])
        self.assertEqual(r["sparkles"]["status"], "ok")
        self.assertEqual(r["qlever"]["status"], "wrong-answer")
        self.assertEqual(r["jena"]["status"], "error")
        self.assertEqual(subprocess.run([sys.executable, RECORD, "validate", out], capture_output=True).returncode, 0)

    def test_unchecked_answers_never_vote_or_claim_agreement(self):
        unchecked = {"rows": 6, "exact": "unchecked", "value": "unchecked"}
        engines = ("sparkles", "jena-fuseki", "oxigraph")
        for instance in (1, 2):
            write(self.res, f"describe-{instance}.json", {
                "results": [hyperfine(e, [0.01]) for e in engines]})
        write(self.res, "answers.json", {
            **{f"describe-{i}": {e: unchecked for e in engines} for i in (1, 2)},
            "mixed": {"sparkles": unchecked, "jena-fuseki": unchecked,
                      "oxigraph": {"rows": 1, "exact": "checked", "value": "checked"}},
        })
        out = os.path.join(self.tmp.name, "record.json")
        p = build(self.res, out, **{"pool-instances": True})
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        for e in ("sparkles", "jena", "oxigraph"):
            row = self.find(rec, e, "describe")
            self.assertIsNone(row["answer"])
            self.assertEqual(row["status"], "ok")
            self.assertEqual(len(row["samples_ms"]), 2)
        for e in ("sparkles", "jena"):
            self.assertIsNone(self.find(rec, e, "mixed", "answer")["answer"])
        checked = self.find(rec, "oxigraph", "mixed", "answer")
        self.assertIsNone(checked["answer"]["agrees"])
        self.assertEqual(checked["status"], "ok")
        self.assertEqual(subprocess.run([sys.executable, RECORD, "validate", out], capture_output=True).returncode, 0)

    def test_build_without_queries_finds_answer_only_queries(self):
        write(self.res, "answers.json", {"q1": {"sparkles": {"rows": 3, "exact": "x", "value": "x"}}})
        out = os.path.join(self.tmp.name, "r.json")
        p = build(self.res, out)
        self.assertEqual(p.returncode, 0, p.stderr)
        rs = jload(out)["results"]
        self.assertEqual([(r["engine"], r["query"], r["mode"], r["status"], r["answer"]["rows"]) for r in rs],
                         [("sparkles", "q1", "answer", "ok", 3)])

    def test_pooled_answer_only_instances_give_one_result_per_template(self):
        ans = {"rows": 3, "exact": "x", "value": "x"}
        write(self.res, "answers.json", {"t-1": {"sparkles": ans}, "t-2": {"sparkles": ans}})
        out = os.path.join(self.tmp.name, "r.json")
        p = build(self.res, out, **{"pool-instances": True})
        self.assertEqual(p.returncode, 0, p.stderr)
        rs = jload(out)["results"]
        self.assertEqual([(r["query"], r["mode"], r["status"], r["instances"]) for r in rs], [("t", "answer", "ok", 2)])
        self.assertEqual(rs[0]["answer"]["instances"], 2)
        self.assertIsNone(rs[0]["median_ms"])
        self.assertEqual(subprocess.run([sys.executable, RECORD, "validate", out], capture_output=True).returncode, 0)

    def test_pooled_hashes_keep_which_instance_gave_which_answer(self):
        def pooled(name, t1, t2):
            d = os.path.join(self.tmp.name, name + "-dir")
            os.mkdir(d)
            ans = lambda v: {"rows": 1, "exact": v, "value": v}
            write(d, "answers.json", {"t-1": {"sparkles": ans(t1)}, "t-2": {"sparkles": ans(t2)}})
            out = os.path.join(self.tmp.name, name + ".json")
            p = build(d, out, **{"pool-instances": True})
            self.assertEqual(p.returncode, 0, p.stderr)
            return out

        old, same, swapped = pooled("old", "a", "b"), pooled("same", "a", "b"), pooled("swap", "b", "a")
        hash_of = lambda path: jload(path)["results"][0]["answer"]["hash"]
        self.assertEqual(hash_of(old), hash_of(same))
        self.assertNotEqual(hash_of(old), hash_of(swapped))
        cmp = lambda a, b: subprocess.run([sys.executable, REGRESSIONS, "compare", a, b], capture_output=True, text=True)
        self.assertEqual(cmp(old, same).returncode, 0)
        p = cmp(old, swapped)
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("ANSWER CHANGED sparkles t", p.stdout)

    def test_a_server_side_timeout_is_a_timeout(self):
        # an HTTP error (curl exit 22) after the request limit is the server's own timeout
        write(self.res, "slow.json", {"results": [hyperfine("jena-fuseki", [5.0, 5.1], [22, 22]),
                                                  hyperfine("oxigraph", [0.2, 0.2], [22, 22])]})
        write(self.res, "cold.json", {"jena-fuseki": {"slow": "error"}})
        write(self.res, "cold-runs.json", {"jena-fuseki": {"slow": {"times": ["timeout", "error"], "ready": [0.1, 0.1]}}})
        out = os.path.join(self.tmp.name, "r.json")
        p = build(self.res, out, **{"max-time": 5})
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        self.assertEqual(self.find(rec, "jena", "slow")["status"], "timeout")
        self.assertEqual(self.find(rec, "oxigraph", "slow")["status"], "error")
        # one cold run timed out and the other failed otherwise: not all timeouts
        self.assertEqual(self.find(rec, "jena", "slow", "cold")["status"], "error")

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
                            "--engine", f"jena-fuseki={binary}", "--data", f"d={data}|CC0|scale=1", "--binary-commit", "abc123",
                            "--memory-limit", "12G", "--workdir", self.tmp.name], capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        m = jload(path)
        self.assertEqual(m["sparkles"]["version"], "tool 1.2")
        self.assertEqual(m["sparkles"]["binary_commit"], "abc123")
        self.assertIn("scripts_commit", m["sparkles"])
        self.assertNotIn("commit", m["sparkles"])
        self.assertEqual(len(m["sparkles"]["sha256"]), 64)
        self.assertEqual(m["engines"]["jena"]["license"], "Apache-2.0")
        self.assertEqual(m["data"]["d"]["license"], "CC0")
        self.assertEqual(m["memory_limit"], "12G")
        self.assertIsNone(m["ended"])
        subprocess.run([sys.executable, RECORD, "finish", path], check=True)
        self.assertIsNotNone(jload(path)["ended"])

    def reuse_fixture(self, prior_host="forge", prior_version="5.1.0"):
        fixture(self.res)
        cur = {"host": {"name": "forge"}, "sparkles": {"binary_commit": "new"},
               "engines": {"jena": {"version": "5.1.0", "sha256": "aa"}}}
        prior = {"host": {"name": prior_host}, "started": "2026-10-18T00:00:00Z", "ended": "2026-10-19T00:00:00Z",
                 "sparkles": {"binary_commit": "old", "scripts_commit": "s1"},
                 "engines": {"jena": {"version": prior_version, "sha256": "aa"}, "oxigraph": {"version": "0.5.11"}}}
        man, pri = os.path.join(self.tmp.name, "m.json"), os.path.join(self.tmp.name, "prior.json")
        write_path(man, cur)
        write_path(pri, prior)
        out = os.path.join(self.tmp.name, "record.json")
        p = build(self.res, out, manifest=man, reuse=f"jena-fuseki={pri}")
        return p, out, pri

    def test_reused_engines_are_named_and_marked(self):
        p, out, _ = self.reuse_fixture()
        self.assertEqual(p.returncode, 0, p.stderr)
        rec = jload(out)
        self.assertEqual(rec["manifest"]["reused"]["jena"]["started"], "2026-10-18T00:00:00Z")
        self.assertEqual(rec["manifest"]["reused"]["jena"]["engine"]["version"], "5.1.0")
        for r in rec["results"]:
            self.assertEqual(r.get("reused", False), r["engine"] == "jena", r)

    def test_reuse_across_hosts_or_versions_is_refused(self):
        p, _, _ = self.reuse_fixture(prior_host="atlas")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("atlas", p.stderr)
        p, _, _ = self.reuse_fixture(prior_version="5.0.0")
        self.assertNotEqual(p.returncode, 0)
        self.assertIn("version changed", p.stderr)


class NixPackageTests(unittest.TestCase):
    def test_store_paths_give_package_and_version(self):
        spec = importlib.util.spec_from_file_location("bench_record", RECORD)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        self.assertEqual(mod.nix_package("/nix/store/abc123-qlever-0.5.48/bin/qlever-server"),
                         ("/nix/store/abc123-qlever-0.5.48", "qlever-0.5.48", "0.5.48"))
        self.assertEqual(mod.nix_package("/nix/store/abc123-apache-jena-fuseki-5.1.0/bin/fuseki-server")[2], "5.1.0")
        self.assertIsNone(mod.nix_package("/usr/bin/true"))


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

    def qmph_record(self, name, qmph, qps=None):
        rec = {"record": 1, "suite": "t", "scale": "s", "manifest": {}, "settings": {}, "results": [
            {"engine": "sparkles", "query": "mix", "mode": "qmph", "clients": 4, "durability": None,
             "samples_ms": [], "median_ms": None, "p95_ms": None, "answer": None, "status": "ok", "qmph": qmph},
            {"engine": "sparkles", "query": "star", "mode": "throughput", "clients": 16, "durability": None,
             "samples_ms": [], "median_ms": None, "p95_ms": None, "answer": None, "status": "ok", "qps": qps or 100.0}]}
        path = os.path.join(self.tmp.name, name + ".json")
        write_path(path, rec)
        return path

    def test_throughput_drop_is_a_regression_and_a_rise_is_not(self):
        a = self.qmph_record("qa", 10000.0)
        p = self.compare(a, self.qmph_record("qb", 8500.0))
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("REGRESSION sparkles mix qmph x4: 10000.0 -> 8500.0 QMpH", p.stdout)
        self.assertEqual(self.compare(a, self.qmph_record("qc", 12000.0)).returncode, 0)
        self.assertEqual(self.compare(a, self.qmph_record("qd", 9400.0)).returncode, 0)

    def test_a_throughput_drop_to_zero_is_a_regression(self):
        a = self.qmph_record("qa", 100.0)
        p = self.compare(a, self.qmph_record("qz", 0.0))
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("REGRESSION", p.stdout)

    def test_two_small_throughput_drops_in_a_row(self):
        a, b, c = self.qmph_record("qa", 10000.0), self.qmph_record("qb", 9400.0), self.qmph_record("qc", 8800.0)
        p = self.compare(a, b, c)
        self.assertEqual(p.returncode, 1, p.stdout)
        self.assertIn("two consecutive", p.stdout)

    def test_unknown_record_versions_are_refused(self):
        a = self.qmph_record("v1", 100.0)
        rec = jload(a)
        rec["record"] = 3
        b = os.path.join(self.tmp.name, "v3.json")
        write_path(b, rec)
        p = self.compare(a, b)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)
        self.assertIn("record version 3", p.stderr)
        v = subprocess.run([sys.executable, RECORD, "validate", b], capture_output=True, text=True)
        self.assertEqual(v.returncode, 1)
        self.assertIn("record version 3", v.stderr)
        # version 1 is still read
        self.assertEqual(self.compare(a, a).returncode, 0)

    def test_changed_answer_hash(self):
        a, b = self.rec("a"), self.rec("b", wrong=True)
        p = self.compare(a, b)
        self.assertEqual(p.returncode, 1)
        self.assertIn("ANSWER CHANGED oxigraph q1", p.stdout)
        self.assertEqual(self.compare(a, b, **{"no-fail": True}).returncode, 0)


class LibTests(unittest.TestCase):
    """Shell functions of scripts/bench-lib.sh, called through bash."""

    def bash(self, queries, script, **extra_env):
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "queries"))
            for name, text in queries.items():
                write_text(os.path.join(d, "queries", name + ".rq"), text)
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles", **extra_env)
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

    def test_fractional_request_limits_round_the_server_timeout_up(self):
        self.assertEqual(self.bash({}, 'echo "$SERVER_TIMEOUT"', MAX_TIME="0.5"), "16")
        self.assertEqual(self.bash({}, 'echo "$SERVER_TIMEOUT"', MAX_TIME="2.2", TIMEOUT_MARGIN="0"), "3")
        self.assertEqual(self.bash({}, 'echo "$SERVER_TIMEOUT"', MAX_TIME="300"), "315")

    def test_an_invalid_request_limit_stops_the_run(self):
        with tempfile.TemporaryDirectory() as d:
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            for bad in ("abc", "0", "-1"):
                env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles", MAX_TIME=bad)
                p = subprocess.run(["bash", "-c", f'source "{lib}"; echo reached'], cwd=d, env=env,
                                   capture_output=True, text=True)
                self.assertNotEqual(p.returncode, 0, bad)
                self.assertIn("MAX_TIME must be a positive number", p.stderr)
                self.assertNotIn("reached", p.stdout)

    def test_suite_settings_in_the_environment_do_not_leak_into_bench_sh_runs(self):
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles",
                       COUNT_ONLY="q1", NO_CHECK="q2", DATAFILE="other.nt", LOADRES="elsewhere")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            p = subprocess.run(["bash", "-c", f'source "{lib}"; echo "[$COUNT_ONLY] [$NO_CHECK] [$DATAFILE] [$LOADRES]"'],
                               cwd=d, env=env, capture_output=True, text=True)
        self.assertEqual(p.stdout.strip(), "[export-500k] [] [data.nt] [results]")

    def test_timed_request_and_cold_request_ask_for_the_same_format(self):
        qs = {"a": "ASK { ?s ?p ?o }"}
        out = self.bash(qs, "q localhost:1/x a")
        self.assertIn("Accept: application/sparql-results+json", out)
        with open(os.path.join(SCRIPTS, "bench-lib.sh")) as f:
            lib = f.read()
        self.assertIn('-H "Accept: $(accept_of "$n")"', lib)


class SuiteTests(unittest.TestCase):
    """The order of steps of suite_run in scripts/bench-suite-lib.sh, with every step stubbed."""

    def test_each_timed_server_answers_every_selected_query_first(self):
        stubs = """
        resolve_tools() { :; }; suite_manifest() { :; }; load_all() { :; }; start_all() { :; }
        answer_check() { :; }; stop_all() { :; }; cold() { :; }; python3() { :; }
        start() { echo "start $1"; URL[$1]=u; }
        stop() { echo "stop $1"; }
        suite_time() { echo "time $ENGINES"; }
        q() { echo "echo warm $1 $2"; }
        log() { :; }
        """
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles jena")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            suite = os.path.join(SCRIPTS, "bench-suite-lib.sh")
            script = (f'source "{lib}"; source "{suite}"; {stubs}\n'
                      "NAMES=(a b); ENGINES='sparkles jena'; SUITE=s SCALE=1 SKIP_LOAD=1; suite_run")
            p = subprocess.run(["bash", "-c", script], cwd=d, env=env, capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.split("\n")[:-1], [
            "start sparkles", "warm u a", "warm u b", "time sparkles", "stop sparkles",
            "start jena", "warm u a", "warm u b", "time jena", "stop jena"])

    def test_an_engine_that_does_not_start_is_left_out_and_the_run_fails(self):
        stubs = """
        resolve_tools() { :; }; suite_manifest() { :; }; load_all() { :; }; start_all() { :; }
        answer_check() { :; }; stop_all() { :; }; cold() { :; }; python3() { :; }
        start() { echo "start $1"; if [ "$1" = jena ]; then start_failed "jena: no"; return 1; fi; URL[$1]=u; }
        stop() { echo "stop $1"; }
        suite_time() { echo "time $ENGINES"; }
        after() { echo "after $ENGINES"; }
        q() { echo "echo warm $1 $2"; }
        log() { :; }
        """
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="jena sparkles")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            suite = os.path.join(SCRIPTS, "bench-suite-lib.sh")
            script = (f'source "{lib}"; source "{suite}"; {stubs}\n'
                      "NAMES=(a); ENGINES='jena sparkles'; SUITE=s SCALE=1 SKIP_LOAD=1 SUITE_AFTER=after; "
                      "set -e; suite_run; echo unreachable")
            p = subprocess.run(["bash", "-c", script], cwd=d, env=env, capture_output=True, text=True)
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertEqual(p.stdout.split("\n")[:-1], [
            "start jena", "start sparkles", "warm u a", "time sparkles", "stop sparkles", "after sparkles"])

    def test_an_engine_that_cannot_be_restarted_fails_the_run(self):
        stubs = """
        resolve_tools() { :; }; suite_manifest() { :; }; load_all() { :; }; start_all() { :; }
        answer_check() { :; }; stop_all() { :; }; cold() { :; }; python3() { :; }
        start() { if [ -f started-$1 ]; then return 1; fi; touch started-$1; URL[$1]=u; }
        stop() { :; }
        answering() { return 1; }
        suite_time() { revive "$ENGINES" q1 r; }
        q() { echo true; }
        log() { :; }
        """
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="jena")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            suite = os.path.join(SCRIPTS, "bench-suite-lib.sh")
            script = (f'source "{lib}"; source "{suite}"; {stubs}\n'
                      "NAMES=(a); ENGINES='jena'; SUITE=s SCALE=1 SKIP_LOAD=1; set -e; suite_run; echo unreachable")
            p = subprocess.run(["bash", "-c", script], cwd=d, env=env, capture_output=True, text=True)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertNotIn("unreachable", p.stdout)

    def test_a_server_that_died_before_a_query_is_restarted_and_the_query_timed(self):
        stubs = """
        log() { :; }
        answering() { [ -f up ]; }
        revive() { echo "revive $1 $2"; touch up; }
        q() { echo "[ -f up ] || (exit 7)"; }
        hyperfine() { echo "hyperfine"; echo '{"results": []}' > "${@: -1}"; }
        """
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="fluree", CLIENT_CPU="none")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            script = f'source "{lib}"; {stubs}\nNAME[fluree]=fluree; URL[fluree]=u; WARMUP=1 RUNS=1; mkdir -p r; time_query count-all r true'
            p = subprocess.run(["bash", "-c", script], cwd=d, env=env, capture_output=True, text=True)
            self.assertEqual(p.returncode, 0, p.stderr)
            self.assertEqual(p.stdout.split("\n")[:2], ["revive fluree before count-all", "hyperfine"])
            self.assertFalse(os.path.exists(os.path.join(d, "r", "count-all.new.json")) and
                             "first_request_failed" in open(os.path.join(d, "r", "count-all.new.json")).read())


@unittest.skipUnless(os.path.exists("/proc/sys/net/ipv4/ip_local_port_range"), "Linux only")
class PortTests(unittest.TestCase):
    """A client connection in TIME-WAIT on a server's port makes the server's bind fail even with
    SO_REUSEADDR, and a listening-socket check does not see it (BSBM batch 1 on forge)."""

    def lib(self, script, **extra):
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles", **extra)
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            return subprocess.run(["bash", "-c", f'source "{lib}"; {script}'], cwd=d, env=env,
                                  capture_output=True, text=True)

    def test_a_time_wait_socket_holds_the_port_until_it_goes(self):
        import socket
        import time
        srv = socket.socket()
        srv.bind(("127.0.0.1", 0))
        srv.listen()
        c = socket.socket()
        c.bind(("127.0.0.1", 0))
        port = c.getsockname()[1]
        c.connect(srv.getsockname())
        a, _ = srv.accept()
        c.close()  # the side that closes first keeps TIME-WAIT
        time.sleep(0.1)
        a.close()
        srv.close()
        s = socket.socket()
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        with self.assertRaises(OSError):
            s.bind(("127.0.0.1", port))
        s.close()
        p = self.lib(f'[ -z "$(pid_on {port})" ] && echo no-listener; wait_port {port} || echo held', PORT_WAIT="1")
        self.assertEqual(p.stdout.split(), ["no-listener", "held"], p.stderr)
        self.assertIn("TIME-WAIT", p.stderr)

    def test_a_servers_own_time_wait_sockets_do_not_hold_its_port(self):
        import socket
        import time
        srv = socket.socket()
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(("127.0.0.1", 0))
        srv.listen()
        port = srv.getsockname()[1]
        c = socket.socket()
        c.connect(("127.0.0.1", port))
        a, _ = srv.accept()
        a.close()  # the server closes first, so its side keeps TIME-WAIT on the port
        time.sleep(0.1)
        c.close()
        srv.close()
        p = self.lib(f"wait_port {port} && echo free", PORT_WAIT="0")
        self.assertEqual(p.stdout.strip(), "free", p.stderr)

    def test_a_free_port_needs_no_wait(self):
        import socket
        s = socket.socket()
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
        s.close()
        p = self.lib(f"wait_port {port} && echo free", PORT_WAIT="0")
        self.assertEqual(p.stdout.strip(), "free", p.stderr)

    def test_server_ports_in_the_ephemeral_range_are_warned_about(self):
        with open("/proc/sys/net/ipv4/ip_local_port_range") as f:
            lo, hi = map(int, f.read().split())
        self.assertIn("ephemeral port range", self.lib("true", PORT_BASE=str(lo + 10)).stderr)
        if lo > 28404:
            self.assertNotIn("ephemeral", self.lib("true", PORT_BASE="28300").stderr)

    def test_evict_reads_names_with_spaces_and_newlines(self):
        with tempfile.TemporaryDirectory() as d:
            store = os.path.join(d, "sparkles.db")
            os.mkdir(store)
            for n in ("a b", "c\nd"):
                write_text(os.path.join(store, n), "x" * 4096)
            env = dict(os.environ, ROOT=os.path.dirname(SCRIPTS), WORK=d, ENGINES="sparkles")
            lib = os.path.join(SCRIPTS, "bench-lib.sh")
            p = subprocess.run(["bash", "-c", f'source "{lib}"; evict sparkles && echo done'], cwd=d, env=env,
                               capture_output=True, text=True)
        self.assertEqual((p.stdout.strip(), p.stderr), ("done", ""))


if __name__ == "__main__":
    unittest.main()
