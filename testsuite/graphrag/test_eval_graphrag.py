"""scripts/eval-graphrag without a provider: its run file, its data, its statistics and,
given SPARKLES_BINARY, a whole dry run against the fake API."""

import copy
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(ROOT / "testsuite/ask/eval_support"))
import eval_support as es  # noqa: E402
from evaluator import Invalid  # noqa: E402

loader = importlib.machinery.SourceFileLoader(
    "graphrag_under_test", str(ROOT / "scripts/eval-graphrag")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
harness = importlib.util.module_from_spec(spec)
loader.exec_module(harness)

RUN = {
    "schema": 1,
    "binary": "/abs/sparkles",
    "binarySha256": "0" * 64,
    "ledger": "/abs/ledger/ledger.jsonl",
    "out": "/abs/out",
    "localCapNanodollars": 30_000_000_000,
    "carryover": {"name": "fresh", "nanodollars": 0},
    "effort": "medium",
    "maxOutputTokens": 8192,
    "requestTimeoutSecs": 180,
    "contextTokens": 200000,
    "answerModel": "claude-sonnet-5-5",
    "judgeModel": "claude-opus-5-5",
    "generatorModel": "claude-opus-5-5",
    "askDeadlineSecs": 300,
    "projectionMargin": 1.5,
    "priorNanodollars": {
        "localAsk": 1,
        "globalAsk": 1,
        "modelCall": 1,
        "buildRequest": 1,
    },
    "questions": "testsuite/graphrag/questions.json",
    "localQuestions": 20,
    "phases": ["acceptance", "evaluate"],
}


class RunFile(unittest.TestCase):
    def load(self, run):
        with tempfile.TemporaryDirectory() as d:
            path = pathlib.Path(d) / "run.json"
            path.write_text(json.dumps(run))
            return harness.load_run(path)

    def test_a_complete_run_file_loads(self):
        self.assertEqual(self.load(RUN)["judgeModel"], "claude-opus-5-5")

    def test_refusals(self):
        for change in (
            {"judgeModel": "claude-sonnet-5-5"},
            {"answerModel": "gpt-4"},
            {"localCapNanodollars": 50_000_000_001},
            {"carryover": None},
            {"phases": []},
            {"phases": ["judge"]},
            {"priorNanodollars": {"localAsk": 1}},
            {"projectionMargin": 0.5},
            {"contextTokens": 0},
            {"binary": "relative/sparkles"},
            {"questions": None},
            {"extra": 1},
        ):
            with self.subTest(change=change), self.assertRaises(Invalid):
                self.load(dict(copy.deepcopy(RUN), **change))

    def test_generation_needs_no_questions_file(self):
        run = dict(copy.deepcopy(RUN), phases=["generate"], questions=None)
        self.assertEqual(self.load(run)["phases"], ["generate"])


class Data(unittest.TestCase):
    def test_two_clusters(self):
        lines = es.two_clusters(size=10).splitlines()
        # ten labels and 45 edges per cluster, and the bridge
        self.assertEqual(len(lines), 2 * (10 + 45) + 1)
        self.assertEqual(len(es.two_clusters(size=4, extra=3).splitlines()), 2 * 10 + 1 + 3)

    def test_demo_turtle_folds_in_the_sample(self):
        sample = {
            "prefixes": {"res": "http://example.org/resource/", "ex": "http://example.org/ontology#"},
            "documents": [
                {
                    "entities": [{"key": "_:new", "label": "New \"one\"", "types": ["ex:Project"]}],
                    "facts": [{"s": "res:A", "p": "ex:contributesTo", "o": "_:new"}],
                }
            ],
        }
        text = es.demo_turtle("@prefix ex: <http://example.org/ontology#> .\n", sample)
        self.assertIn('res:ingested-new rdfs:label "New \\"one\\"" .', text)
        self.assertIn("res:ingested-new a ex:Project .", text)
        self.assertIn("res:A ex:contributesTo res:ingested-new .", text)

    def test_chunks_and_bm25(self):
        sample = {
            "documents": [
                {"id": "a", "title": "Storage", "text": "The storage team ships.\n\nNo blockers."},
                {"id": "b", "title": "Cache", "text": "The result cache project grows."},
            ]
        }
        chunks = es.chunks(sample)
        self.assertEqual([c["id"] for c in chunks], ["a#0", "a#1", "b#0"])
        self.assertEqual(es.bm25(chunks, "result cache", k=1)[0]["id"], "b#0")
        # Ties keep the source order.
        self.assertEqual([c["id"] for c in es.bm25(chunks, "zzz", k=3)], ["a#0", "a#1", "b#0"])


class Statistics(unittest.TestCase):
    def test_markers_are_stripped(self):
        self.assertEqual(es.strip_markers("Work is on storage [2] and caching [1, 3]."), "Work is on storage and caching.")

    def test_both_orders_count_from_the_first_answer(self):
        # Question 1: X wins as A and as B. Question 2: X wins as A, loses as B.
        scores = es.judge_scores(
            [("q1", True, "A"), ("q1", False, "B"), ("q2", True, "A"), ("q2", False, "A"), ("q3", True, "tie"), ("q3", False, "tie")]
        )
        self.assertEqual(scores, {"q1": 1.0, "q2": 0.5, "q3": 0.5})

    def test_bootstrap_is_deterministic_and_brackets_the_mean(self):
        values = [0.0, 0.5, 1.0, 1.0, 0.5, 0.0, 1.0]
        a, b = es.bootstrap(values), es.bootstrap(values)
        self.assertEqual(a, b)
        self.assertLessEqual(a["low"], a["mean"])
        self.assertLessEqual(a["mean"], a["high"])
        self.assertIsNone(es.bootstrap([]))

    def test_stable_sample(self):
        items = [{"id": str(i)} for i in range(10)]
        self.assertEqual(
            es.stable_sample(items, lambda x: x["id"], 3),
            es.stable_sample(list(reversed(items)), lambda x: x["id"], 3),
        )

    def test_fill_follows_the_schema(self):
        self.assertEqual(
            es.fill(harness.judge_schema())["comprehensiveness"]["winner"], "A"
        )
        value = es.fill(
            {
                "type": "object",
                "properties": {
                    "scope": {"type": "string", "enum": ["local", "global"]},
                    "cites": {"type": "array", "items": {"type": "integer"}},
                    "rating": {"type": "integer", "minimum": 0},
                },
            }
        )
        self.assertEqual(value, {"scope": "global", "cites": [1], "rating": 5})


class Answers(unittest.TestCase):
    def test_a_marker_after_the_full_stop_belongs_to_the_sentence_before(self):
        self.assertEqual(
            harness.claims("Storage builds. [1] Research builds [2, 3]. No marker here."),
            [
                {"sentence": "Storage builds.", "points": [1]},
                {"sentence": "Research builds.", "points": [2, 3]},
                {"sentence": "No marker here.", "points": []},
            ],
        )

    def test_an_ask_that_chose_no_scope_is_unclassified(self):
        def mode(**kw):
            return harness.mode_of(dict({"returncode": 0, "outcome": "answered"}, **kw))

        self.assertEqual(mode(mode="global"), "global")
        self.assertEqual(mode(mode="local"), "local")
        # A query that failed its repairs still came from a draft that chose local.
        self.assertEqual(mode(mode="local", outcome="failed"), "local")
        self.assertEqual(
            mode(mode="local", outcome="clarify", clarify={"id": "mention"}), "unclassified"
        )
        self.assertEqual(mode(mode="local", outcome="clarify", clarify={"id": "draft"}), "local")
        self.assertEqual(mode(outcome="error", returncode=2), "unclassified")
        self.assertEqual(mode(mode="local", returncode=2), "unclassified")

    def test_only_a_local_answer_or_draft_clarification_answers_locally(self):
        def local(**kw):
            return harness.answered_locally(dict({"returncode": 0, "mode": "local"}, **kw))

        self.assertTrue(local(outcome="answered"))
        self.assertTrue(local(outcome="clarify", clarify={"id": "draft"}))
        self.assertFalse(local(outcome="clarify", clarify={"id": "mention"}))
        self.assertFalse(local(outcome="failed"))
        self.assertFalse(local(outcome="answered", mode="global"))

    def test_every_marker_must_reach_cited_findings(self):
        out = {
            "mode": "global",
            "summary": {"text": "Work is on storage [1] and caching [2].", "citations": [1, 2]},
            "points": [
                {"n": 1, "findings": ["r#f1"]},
                {"n": 2, "findings": ["r#f2"]},
            ],
            "reports": [
                {"findings": [{"iri": "r#f1", "cites": ["<<( <a> <b> <c> )>>"]}, {"iri": "r#f2", "cites": []}]}
            ],
        }
        self.assertEqual(harness.cited_points(out)["unlinked"], 1)
        out["reports"][0]["findings"][1]["cites"] = ["r#f9"]
        self.assertEqual(harness.cited_points(out)["unlinked"], 0)
        out["summary"]["text"] = "No markers."
        self.assertEqual(harness.cited_points(out)["markers"], 0)


class ByteBound(unittest.TestCase):
    def test_only_community_requests_are_measured(self):
        with tempfile.TemporaryDirectory() as d:
            path = pathlib.Path(d) / "replies.jsonl"
            report = {"type": "object", "properties": {"rating": {"type": "integer"}}}
            other = {"type": "object", "properties": {"points": {"type": "array"}}}
            lines = [
                {
                    "request": {
                        "model": "m",
                        "messages": [{"role": "user", "content": "x" * 1000}],
                        "output_config": {"format": {"type": "json_schema", "schema": s}},
                    },
                    "reply": {"usage": {"input_tokens": n}},
                }
                for s, n in ((report, 300), (other, 999))
            ]
            path.write_text("".join(json.dumps(x) + "\n" for x in lines))
            m = harness.byte_bound_measure(path)
            self.assertEqual(m["requests"], 1)
            self.assertEqual(m["maxInputTokens"], 300)
            self.assertLess(m["maxInputOverBound"], 1)
            self.assertIsNone(harness.byte_bound_measure(pathlib.Path(d) / "absent"))


@unittest.skipUnless(os.environ.get("SPARKLES_BINARY"), "set SPARKLES_BINARY to a release binary")
class DryRun(unittest.TestCase):
    def test_every_phase_runs_against_the_fake(self):
        import hashlib

        binary = os.environ["SPARKLES_BINARY"]
        with tempfile.TemporaryDirectory() as d:
            questions = pathlib.Path(d) / "q.json"
            questions.write_text(
                json.dumps({"questions": [{"id": "g1", "kind": "global", "question": "What are the main areas of work?"}]})
            )
            run = dict(
                copy.deepcopy(RUN),
                binary=binary,
                binarySha256=hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),
                questions=str(questions),
                localQuestions=2,
                phases=["acceptance", "generate", "evaluate"],
                priorNanodollars={k: 1_000_000 for k in harness.PRIORS},
            )
            path = pathlib.Path(d) / "run.json"
            path.write_text(json.dumps(run))
            p = subprocess.run(
                [sys.executable, "-I", str(ROOT / "scripts/eval-graphrag"), "--run", str(path), "--dry-run"],
                capture_output=True,
                text=True,
                timeout=900,
                cwd=d,
            )
            self.assertEqual(p.returncode, 0, p.stderr)
            summary = json.loads(pathlib.Path(p.stdout.split("summary in ")[1].strip()).read_text())
            examples = summary["phases"]["acceptance"]["examples"]
            for name in ("A1", "A3", "A4", "A6", "A7", "A8"):
                self.assertTrue(examples[name]["pass"], (name, examples[name]))
            ev = summary["phases"]["evaluate"]
            self.assertEqual(ev["questions"], 1)
            self.assertEqual(ev["levelsUsed"]["global-coarse"], [0])
            self.assertEqual(ev["classification"]["unclassified"], 0)
            self.assertTrue(ev["citations"]["global-fine"]["citedFacts"] > 0)
            self.assertEqual(len(ev["winRates"]), 6)
            self.assertEqual(summary["ledger"]["unresolvedNanodollars"], 0)


if __name__ == "__main__":
    unittest.main()
