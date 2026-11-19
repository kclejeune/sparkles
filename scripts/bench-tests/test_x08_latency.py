"""Tests of scripts/x08-gates/latency.sh and combine.py, in the pool form and the
base-and-tip form, with a stub server in place of sparkles.

    python3 -m unittest discover -s scripts/bench-tests -p test_x08_latency.py
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent.parent
GATES = SCRIPTS / "x08-gates"
STUB = Path(__file__).resolve().parent / "fixtures" / "stub-sparkles.py"


def free_port():
    """A port no other test or run holds, so that suites can run side by side."""
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def samples(path, ratio, rounds=12, a_ms=0.1, query="q"):
    lines = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
    for r in range(1, rounds + 1):
        for rep in range(3):
            lines.append(f"{r}\t{r}\tA\t{query}\t{rep}\t{a_ms}\t1")
            lines.append(f"{r}\t{r}\tB\t{query}\t{rep}\t{a_ms * ratio}\t1")
    Path(path).write_text("\n".join(lines) + "\n")
    return path


def combine(*args):
    return subprocess.run([sys.executable, str(GATES / "combine.py"), *map(str, args)],
                          capture_output=True, text=True)


def answers(*args):
    return subprocess.run([sys.executable, str(GATES / "answers.py"), *map(str, args)], capture_output=True, text=True)


class AnswersTests(unittest.TestCase):
    def run_dir(self, root, name, query, rows):
        d = Path(root) / name
        (d / "results").mkdir(parents=True)
        (d / "queries").mkdir()
        (d / "queries" / "q.rq").write_text(query)
        samples = ["round\tepoch\tvariant\tquery\trep\texec_ms\twall_ms"]
        warm, untimed = [], []
        for e, v in ((1, "A"), (2, "B")):
            samples.append(f"1\t{e}\t{v}\tq\t1\t0.1\t1")
            untimed.append(f"{v}\tq\t1\t{rows[v]}\tdistinct:1 schema:ab")
            warm.append(f"{e}\t{v}\tq\t1\t1 rows, schema ab, distinct 1, {rows[v]}")
        (d / "results" / "samples.tsv").write_text("\n".join(samples) + "\n")
        (d / "results" / "answers.tsv").write_text("\n".join(untimed) + "\n")
        (d / "results" / "warm-answers.tsv").write_text("\n".join(warm) + "\n")
        return d

    def test_an_unordered_limit_may_return_other_rows(self):
        with tempfile.TemporaryDirectory() as d:
            q = "SELECT ?s WHERE { ?s <urn:p> <urn:o> } LIMIT 1"
            run = self.run_dir(d, "r", q, {"A": "x", "B": "y"})
            r = answers("--queries", "q", run)
            self.assertEqual(r.returncode, 0, r.stdout)
            run = self.run_dir(d, "s", "SELECT ?s WHERE { ?s <urn:p> <urn:o> }", {"A": "x", "B": "y"})
            r = answers("--queries", "q", run)
            self.assertEqual(r.returncode, 1, r.stdout)

    def test_only_a_top_level_limit_without_order_by_is_an_unordered_slice(self):
        with tempfile.TemporaryDirectory() as d:
            for i, (query, ok) in enumerate((
                    ('SELECT ("LIMIT" AS ?x) WHERE {}', False),
                    ("SELECT ?s WHERE { ?s ?p ?o } LIMIT 1 # ORDER BY ?s", True),
                    ("SELECT ?s WHERE { ?s ?p ?o } ORDER BY ?s LIMIT 1", False),
                    ("SELECT ?s WHERE { ?s <urn:limit> ?o }", False),
                    ('SELECT ("""alpha " LIMIT 1 " omega""" AS ?x) WHERE {}', False),
                    ('SELECT ?s ("""a " ORDER BY ?s " b""" AS ?x) WHERE { ?s ?p ?o } LIMIT 1', True),
                    ("SELECT ?s ('''it's LIMIT 2''' AS ?x) WHERE { ?s ?p ?o }", False),
                    ("PREFIX ex: <urn:ex:> SELECT ?x (ex:a\\#b AS ?v) WHERE { ?x ?p ?o } ORDER BY ?x\nLIMIT 1",
                     False))):
                run = self.run_dir(d, f"r{i}", query, {"A": "x", "B": "y"})
                self.assertEqual(answers("--queries", "q", run).returncode, 0 if ok else 1, query)

    def test_the_order_digests_lex_queries_as_the_checker_does(self):
        gate = SCRIPTS / "bench-gate.py"
        with tempfile.TemporaryDirectory() as d:
            def digest(query, rows):
                (Path(d) / "q.rq").write_text(query)
                (Path(d) / "a.tsv").write_text("?x\n" + "".join(f"{r}\n" for r in rows))
                r = subprocess.run([sys.executable, str(gate), "order-digest", Path(d) / "q.rq", Path(d) / "a.tsv"],
                                   capture_output=True, text=True)
                self.assertEqual(r.returncode, 0, r.stderr)
                return r.stdout.strip()
            # ORDER BY inside a long literal: an unordered slice, so only counts are digested
            q = 'SELECT ?x ("""alpha " ORDER BY " omega""" AS ?v) WHERE { VALUES ?x { "a" "b" } } LIMIT 1'
            self.assertTrue(digest(q, ['"a"']).startswith("distinct:"))
            self.assertEqual(digest(q, ['"a"']), digest(q, ['"b"']))
            # a real ORDER BY after an escaped hash or a hash in a long literal keeps its order
            for q in ('PREFIX ex: <urn:ex:> SELECT ?x (ex:a\\#b AS ?v) WHERE { VALUES ?x { "a" "b" } } ORDER BY ?x\nLIMIT 2',
                      'SELECT ?x ("""alpha " # text " omega""" AS ?v) WHERE { VALUES ?x { "a" "b" } } ORDER BY ?x LIMIT 2'):
                self.assertTrue(digest(q, ['"a"', '"b"']).startswith("keys:"), q)
                self.assertNotEqual(digest(q, ['"a"', '"b"']), digest(q, ['"b"', '"a"']), q)

    def test_a_staged_query_missing_from_every_run_fails(self):
        with tempfile.TemporaryDirectory() as d:
            run = self.run_dir(d, "r", "ASK {}", {"A": "x", "B": "x"})
            self.assertEqual(answers("--queries", "q", run).returncode, 0)
            r = answers("--queries", "q other", run)
            self.assertEqual(r.returncode, 1)
            self.assertIn("other queries than those staged", r.stdout)


class CombineTests(unittest.TestCase):
    def test_base_and_tip_labels_estimate_the_tip_over_the_base(self):
        with tempfile.TemporaryDirectory() as d:
            # the tip is 10% slower and the second position costs 2%
            second = samples(Path(d) / "s.tsv", 1.10 * 1.02, a_ms=0.1)
            first = samples(Path(d) / "f.tsv", 1.02 / 1.10, a_ms=0.11)
            r = combine("--labels", "base,tip", second, first)
            self.assertEqual(r.returncode, 1, r.stdout)
            self.assertIn("| query | base ms | tip/base |", r.stdout)
            self.assertIn("| q | 0.1000 | 1.1000 |", r.stdout)
            self.assertIn("q: tip/base 1.1000", r.stdout)
            second = samples(Path(d) / "s.tsv", 1.02, a_ms=0.1)
            first = samples(Path(d) / "f.tsv", 1.02, a_ms=0.1)
            r = combine("--labels", "base,tip", second, first)
            self.assertEqual(r.returncode, 0, r.stdout)

    def test_the_pool_form_keeps_its_table(self):
        with tempfile.TemporaryDirectory() as d:
            r = combine(samples(Path(d) / "s.tsv", 1.0), samples(Path(d) / "f.tsv", 1.0))
            self.assertEqual(r.returncode, 0, r.stdout)
            self.assertIn("| query | pool off ms | on/off |", r.stdout)
            self.assertIn("| pool-on-second ratio | pool-on-first ratio |", r.stdout)


@unittest.skipUnless(all(shutil.which(t) for t in ("taskset", "chrt", "zstd", "curl")) and (os.cpu_count() or 1) >= 5,
                     "needs taskset, chrt, zstd, curl and five CPUs")
class LatencyTests(unittest.TestCase):
    def work(self, d):
        # a space in the path, which the digest commands must survive
        w = Path(d) / "work dir"
        (w / "queries").mkdir(parents=True)
        (w / "queries" / "count-all.rq").write_text("SELECT (COUNT(*) AS ?c) WHERE { ?s ?p ?o }")
        (w / "data.nt.zst").write_bytes(subprocess.run(["zstd", "-q", "-c"], input=b"", capture_output=True).stdout)
        shutil.copy(SCRIPTS / "nsc-bench-remote.sh", w)
        bins = {}
        for name in ("base", "tip"):
            p = Path(d) / name / "sparkles"
            p.parent.mkdir()
            p.write_text(STUB.read_text().replace('IDENTITY = "stub"', f'IDENTITY = "{name}"'))
            p.chmod(0o755)
            bins[name] = p
        return w, bins

    def test_the_base_and_tip_form_runs_each_binary_in_both_positions(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            extra = Path(d) / "ask-shape.rq"
            extra.write_text("ASK { ?s ?p ?o }")
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"],
                 "--base-execution", "eager", "--queries", f"count-all {extra}", "--rounds", "10", "--reps", "2",
                 "--port", str(free_port()), w], capture_output=True, text=True, timeout=600)
            # the stub answers in a fixed time, so the gate passes
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            self.assertIn("| query | base ms | tip/base |", r.stdout)
            self.assertIn("| ask-shape |", r.stdout)
            seen = {}
            for run in ("tip-second", "tip-first", "same"):
                for line in (w / "runs" / run / "requests.tsv").read_text().splitlines():
                    identity, owner, store, mode, _ = line.split("\t")
                    seen.setdefault(run, set()).add((store, identity, owner, mode))
            self.assertEqual(seen["tip-second"], {("stores/A", "base", "base", "eager"), ("stores/B", "tip", "tip", "-")})
            self.assertEqual(seen["tip-first"], {("stores/A", "tip", "tip", "-"), ("stores/B", "base", "base", "eager")})
            self.assertEqual(seen["same"], {("stores/A", "tip", "tip", "-"), ("stores/B", "tip", "tip", "-")})
            self.assertIn("execution eager", (w / "runs-manifest.txt").read_text())
            self.assertIn("the answers agree", r.stdout)
            self.assertIn("diagnostic run (--force)", r.stdout)
            # each binary loaded its own store, and the run against itself one
            for run, stores in (("tip-second", ["A", "B"]), ("tip-first", ["A", "B"]), ("same", ["A"])):
                loads = (w / "runs" / run / "results" / "load.tsv").read_text().splitlines()
                self.assertEqual([l.split("\t")[0] for l in loads], stores)

    def test_an_inherited_environment_cannot_change_the_arms(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            env = dict(os.environ, BINARY_OF_B="A", SHARE_STORE="1", EXECUTION="streaming")
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"], "--queries",
                 "count-all", "--rounds", "10", "--reps", "1", "--port", str(free_port()), w],
                capture_output=True, text=True, timeout=600, env=env)
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
            ids = {tuple(l.split("\t")[:2]) for l in (w / "runs" / "tip-second" / "requests.tsv").read_text().splitlines()}
            self.assertEqual(ids, {("base", "base"), ("tip", "tip")})

    def test_disagreeing_answers_fail_the_gate(self):
        with tempfile.TemporaryDirectory() as d:
            w, bins = self.work(d)
            bins["tip"].write_text(bins["tip"].read_text().replace('b"?c\\n1\\n"', 'b"?c\\n2\\n"'))
            r = subprocess.run(
                ["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"], "--queries",
                 "count-all", "--rounds", "10", "--reps", "1", "--port", str(free_port()), w],
                capture_output=True, text=True, timeout=600)
            self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
            self.assertIn("the untimed answers differ", r.stdout)

    def test_refusals(self):
        with tempfile.TemporaryDirectory() as d:
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", "/bin/true", d],
                               capture_output=True, text=True)
            self.assertIn("needs --base and --tip", r.stderr)
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--force", "--base", "/bin/true", "--tip",
                                "/bin/true", "--rounds", "4", d], capture_output=True, text=True)
            self.assertIn("at least 10 rounds", r.stderr)
            w, bins = self.work(d)
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", bins["base"], "--tip", bins["tip"], w],
                               capture_output=True, text=True)
            self.assertIn("names no compilers", r.stderr)
            r = subprocess.run(["bash", str(GATES / "latency.sh"), "--base", "/bin/true", "--tip", "/bin/true", w],
                               capture_output=True, text=True)
            self.assertIn("needs --base-build-config and --tip-build-config", r.stderr)
            (Path(d) / "count-all.rq").write_text("ASK {}")
            hidden = Path(d) / ".hidden.rq"
            hidden.write_text("ASK {}")
            for q, message in (("/no/such.rq", "no query file"), (f"{bins['tip']}", "must end in .rq"),
                               (str(hidden), "must start with a letter or digit"),
                               (f"count-all {d}/count-all.rq", "two queries are named count-all")):
                r = subprocess.run(["bash", str(GATES / "latency.sh"), "--force", "--base", bins["base"], "--tip", bins["tip"],
                                    "--queries", q, w], capture_output=True, text=True)
                self.assertIn(message, r.stderr)


if __name__ == "__main__":
    unittest.main()
