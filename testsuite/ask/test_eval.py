"""Credential-free regression tests for the initial evaluator."""

import importlib.machinery
import importlib.util
import json
import pathlib
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "eval_support"))
from evaluator import (
    POLICY_V1,
    Invalid,
    Unsupported,
    answer_set,
    capture_child,
    choose_clarification,
    digest,
    gold_terms,
    norm_term,
    parse_child,
    prf,
    score_sets,
    strict_json,
)

loader = importlib.machinery.SourceFileLoader(
    "evaluation_under_test", str(HERE.parents[1] / "scripts/eval-ask")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
eval_ask = importlib.util.module_from_spec(spec)
loader.exec_module(eval_ask)
PYTHON = str(pathlib.Path(sys.base_prefix) / "bin/python3")


def literal(value, **kw):
    return dict(type="literal", value=value, **kw)


def result(rows, names=("x", "y")):
    return {"head": {"vars": list(names)}, "results": {"bindings": rows}}


def iri(value):
    return {"type": "uri", "value": value}


class Scoring(unittest.TestCase):
    def test_global_columns(self):
        a = answer_set(
            result(
                [
                    {"x": literal("A"), "y": literal("B")},
                    {"x": literal("B"), "y": literal("C")},
                ]
            )
        )
        b = answer_set(
            result(
                [
                    {"x": literal("B"), "y": literal("A")},
                    {"x": literal("B"), "y": literal("C")},
                ]
            )
        )
        c = answer_set(
            result(
                [
                    {"x": literal("B"), "y": literal("A")},
                    {"x": literal("C"), "y": literal("B")},
                ]
            )
        )
        self.assertNotEqual(a, b)
        self.assertEqual(a, c)
        self.assertEqual(prf(a, c), (1, 1, 1))
        self.assertLess(prf(a, b)[2], 1)

    def test_extra_answer_columns_are_projected(self):
        gold = answer_set(result([{"p": iri("urn:a")}, {"p": iri("urn:b")}], ("p",)))
        # The right people with a name and a city beside each.
        wide = answer_set(
            result(
                [
                    {"n": literal("A"), "p": iri("urn:a"), "c": iri("urn:berlin")},
                    {"n": literal("B"), "p": iri("urn:b"), "c": iri("urn:berlin")},
                ],
                ("n", "p", "c"),
            )
        )
        ok, scores, detail = score_sets(gold, wide)
        self.assertTrue(ok)
        self.assertEqual(scores, (1, 1, 1))
        self.assertEqual((detail["mapping"], detail["extraColumns"]), ([1], 2))
        # The first policy, kept to reproduce old reports, refuses any other width.
        self.assertFalse(score_sets(gold, wide, policy=POLICY_V1)[0])
        with self.assertRaises(Invalid):
            score_sets(gold, wide, policy="no-such-policy")
        # Two rows for one person still project to the one gold row.
        twice = answer_set(
            result(
                [
                    {"p": iri("urn:a"), "n": literal("A")},
                    {"p": iri("urn:a"), "n": literal("Alias")},
                    {"p": iri("urn:b"), "n": literal("B")},
                ],
                ("p", "n"),
            )
        )
        self.assertTrue(score_sets(gold, twice)[0])
        # A stray person in the projection costs precision, a missing one recall.
        stray = answer_set(
            result(
                [
                    {"p": iri("urn:a"), "n": literal("A")},
                    {"p": iri("urn:b"), "n": literal("B")},
                    {"p": iri("urn:c"), "n": literal("C")},
                ],
                ("p", "n"),
            )
        )
        ok, scores, _ = score_sets(gold, stray)
        self.assertFalse(ok)
        self.assertEqual(scores[:2], (2 / 3, 1))
        one = answer_set(result([{"p": iri("urn:a"), "n": literal("A")}], ("p", "n")))
        self.assertEqual(score_sets(gold, one)[1][:2], (1, 0.5))
        # Terms stay exact: a label in place of the IRI is not the answer.
        label = answer_set(
            result([{"n": literal("urn:a"), "m": literal("x")}], ("n", "m"))
        )
        self.assertFalse(score_sets(gold, label)[0])
        # Two gold columns are matched to two distinct answer columns.
        pair = answer_set(result([{"p": iri("urn:a"), "q": iri("urn:a")}], ("p", "q")))
        pair_wide = answer_set(
            result(
                [{"x": literal("x"), "p": iri("urn:a"), "q": iri("urn:a")}],
                ("x", "p", "q"),
            )
        )
        self.assertTrue(score_sets(pair, pair_wide)[0])
        narrow = answer_set(result([{"p": iri("urn:a")}], ("p",)))
        self.assertEqual(score_sets(pair, narrow)[2]["status"], "missing_columns")

    def test_tuple_precision(self):
        a = answer_set(
            result(
                [
                    {"x": literal("A"), "y": literal("B")},
                    {"x": literal("B"), "y": literal("C")},
                ]
            )
        )
        b = answer_set(
            result(
                [
                    {"x": literal("A"), "y": literal("C")},
                    {"x": literal("B"), "y": literal("B")},
                ]
            )
        )
        self.assertEqual(prf(a, b), (0, 0, 0))

    def test_terms(self):
        self.assertNotEqual(
            norm_term(
                literal(
                    "1.0000001", datatype="http://www.w3.org/2001/XMLSchema#decimal"
                )
            ),
            norm_term(
                literal(
                    "1.0000002", datatype="http://www.w3.org/2001/XMLSchema#decimal"
                )
            ),
        )
        self.assertNotEqual(
            norm_term(literal("word", **{"xml:lang": "en"})),
            norm_term(literal("word", **{"xml:lang": "fr"})),
        )
        self.assertEqual(
            norm_term(literal("word", **{"xml:lang": "EN-us"})),
            norm_term(literal("word", **{"xml:lang": "en-US"})),
        )
        self.assertEqual(
            norm_term(literal("word")),
            norm_term(
                literal("word", datatype="http://www.w3.org/2001/XMLSchema#string")
            ),
        )
        self.assertNotEqual(
            norm_term(literal("1")), norm_term({"type": "uri", "value": "1"})
        )
        with self.assertRaises(Unsupported):
            answer_set(result([{"x": {"type": "bnode", "value": "a"}}]))

    def test_schema_and_unbound(self):
        self.assertNotEqual(
            answer_set(result([{"x": literal("A")}])),
            answer_set(result([{"x": literal("A"), "y": literal("A")}])),
        )
        self.assertNotEqual(
            answer_set(result([{"x": literal("A"), "y": literal("A")}])),
            answer_set(result([{"x": literal("A")}], ("x",))),
        )
        bad = [
            {},
            {"error": "oops"},
            {"head": {}, "boolean": 0},
            {"head": {}, "boolean": False, "results": {"bindings": []}},
            result([{"z": literal("x")}]),
            result([], ("x", "x")),
        ]
        for value in bad:
            with self.subTest(value=value), self.assertRaises(Invalid):
                answer_set(value)
        self.assertEqual(prf(answer_set(result([])), answer_set(result([]))), (1, 1, 1))
        self.assertEqual(prf(False, answer_set(result([]))), (0, 0, 0))
        self.assertEqual(
            prf(answer_set(result([])), answer_set(result([{"x": literal("a")}]))),
            (0, 0, 0),
        )
        with self.assertRaises(Unsupported):
            answer_set(result([], tuple(str(i) for i in range(9))))
        with self.assertRaises(Invalid):
            strict_json('{"x":1,"x":2}')
        with self.assertRaises(Invalid):
            strict_json('{"x":NaN}')

    def test_duplicates_and_bounded_search(self):
        a = answer_set(result([{"x": literal("a")}, {"x": literal("a")}]))
        self.assertEqual(a.duplicates, 1)
        b = answer_set(result([{"x": literal("a")}]))
        self.assertEqual(a, b)
        called = []
        score_sets(a, b, lambda: called.append(True))
        self.assertTrue(called)

        def expired():
            raise Invalid("expired")

        with self.assertRaises(Invalid):
            score_sets(a, b, expired)

    def test_metrics_pipeline_denominators(self):
        qs = [
            {"kind": "answerable", "complexity": 1},
            {"kind": "answerable", "complexity": 9},
            {"kind": "clarify"},
            {"kind": "unanswerable"},
        ]
        answers = [
            {"result": {"attempt": 2}, "usage": {"estimatedCost": 3}},
            {"outcome": "unanswerable"},
            {"outcome": "clarify"},
            {"outcome": "empty"},
        ]
        scores = [
            (True, (1, 1, 1), {"kind": "ask", "firstDraft": {"status": "incorrect"}}),
            (
                False,
                (0, 0, 0),
                {"kind": "missing_query", "firstDraft": {"status": "incorrect"}},
            ),
            (True, (1, 1, 1), {"kind": "clarify"}),
            (False, (0, 0, 0), {"kind": "unanswerable"}),
        ]
        rows = eval_ask.summarize_pair("p/m", qs, answers, scores)
        self.assertEqual(len(rows), 1)
        r = rows[0]
        self.assertEqual(r["executionAccuracy"]["denominator"], 2)
        self.assertEqual(
            r["repairRate"], {"numerator": 1, "denominator": 2, "value": 0.5}
        )
        self.assertEqual(r["repairConditional"]["denominator"], 2)
        self.assertIsNone(r["cliEstimatedCostUSD"])
        self.assertEqual(r["refusalAccuracy"]["value"], 0)
        for a in answers:
            a["usage"] = {"estimatedCost": 3}
        self.assertEqual(
            eval_ask.summarize_pair("p/m", qs, answers, scores)[0][
                "cliEstimatedCostUSD"
            ],
            12,
        )
        for answer in answers:
            answer["cliEstimatedCostKnown"] = False
            answer["usage"]["estimatedCost"] = 0
        self.assertIsNone(
            eval_ask.summarize_pair("p/m", qs, answers, scores)[0][
                "cliEstimatedCostUSD"
            ]
        )


class Processes(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="ask-controls-")
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)

    def child(self, source, name="child"):
        p = self.root / name
        p.write_text("#!" + PYTHON + "\n" + source)
        p.chmod(0o700)
        return str(p)

    def run_child(self, source, timeout=2, **kwargs):
        f = self.child(source)
        d = self.root / ("capture-" + str(len(list(self.root.iterdir()))))
        r = capture_child([f], d, time.monotonic() + timeout, **kwargs)
        return r, d

    def test_nonzero_noise_multiple_and_malformed(self):
        for source in [
            'print("{}")\nraise SystemExit(17)',
            'print("noise\\n{}")',
            'print("{}\\n{}")',
            'print("broken")',
        ]:
            r, d = self.run_child(source)
            with self.assertRaises(Invalid):
                parse_child(r, d)
        self.assertEqual(r["returncode"], 0)

    def test_caps_exact_and_plus_one(self):
        for channel in ("stdout", "stderr"):
            cap = 4 * 1024 * 1024 if channel == "stdout" else 1024 * 1024
            for size in (cap, cap + 1):
                r, d = self.run_child(
                    f'import os\nos.write({1 if channel == "stdout" else 2},b"x"*{size})',
                )
                self.assertEqual((d / channel).stat().st_size, cap)
                self.assertEqual(r["byteCap"], None if size == cap else channel)
                self.assertTrue(r["cleanup"])

    def test_deadline_and_descendant(self):
        r, d = self.run_child(
            "import os,time\nif os.fork()==0: time.sleep(30)\ntime.sleep(30)",
            timeout=0.1,
        )
        self.assertTrue(r["timeout"])
        self.assertTrue(r["cleanup"])
        self.assertLess(r["elapsedSeconds"], 3)
        with self.assertRaises(Invalid):
            parse_child(r, d)

    def fixture(self):
        binary = self.child(
            'import json\nprint(json.dumps({"head":{"vars":[]},"results":{"bindings":[]}}))'
        )
        data = self.root / "data.ttl"
        data.write_text("")
        q = self.root / "set.json"
        q.write_text(
            json.dumps(
                {
                    "data": "data.ttl",
                    "questions": [{"id": "one", "kind": "clarify", "question": "q"}],
                }
            )
        )
        cfg = self.root / "models.json"
        cfg.write_text(
            json.dumps({"models": {"providers": {"p": {"models": {"m": {}}}}}})
        )
        return eval_ask.parse_args(
            [
                "--sparkles",
                binary,
                "--sparkles-sha256",
                digest(binary),
                "--model-config",
                str(cfg),
                "--pair",
                "p/m",
                "--set",
                str(q),
                "--out",
                str(self.root / "report"),
            ]
        )

    def test_setup_refusals_no_invocations(self):
        for field, value in [
            ("deadline", float("nan")),
            ("deadline", float("inf")),
            ("deadline", -1),
            ("max_tokens", 0),
            ("pair", ["p/m", "p/m"]),
            ("pair", ["p/absent"]),
            ("only", "absent"),
            ("sparkles", "/absent"),
        ]:
            args = self.fixture()
            setattr(args, field, value)
            with self.subTest(field=field, value=value), self.assertRaises(Invalid):
                eval_ask.validate_args(args)
        args = self.fixture()
        pathlib.Path(args.out).mkdir()
        (pathlib.Path(args.out) / "KEEP").write_text("original")
        with self.assertRaises(FileExistsError):
            eval_ask.validate_args(args)
        self.assertEqual((pathlib.Path(args.out) / "KEEP").read_text(), "original")

    def test_duplicate_ids_and_old_slug_collision(self):
        args = self.fixture()
        data = json.loads(pathlib.Path(args.set).read_text())
        data["questions"] *= 2
        pathlib.Path(args.set).write_text(json.dumps(data))
        with self.assertRaises(Invalid):
            eval_ask.validate_args(args)
        data["questions"] = data["questions"][:1]
        pathlib.Path(args.set).write_text(json.dumps(data))
        binary = pathlib.Path(args.sparkles)
        binary.write_text(
            "#!" + PYTHON + '\nimport json\nprint(json.dumps({"outcome":"clarify"}))\n'
        )
        args.sparkles_sha256 = digest(binary)
        pathlib.Path(args.model_config).write_text(
            json.dumps(
                {"models": {"providers": {"p": {"models": {"a:b": {}, "a_b": {}}}}}}
            )
        )
        args.pair = ["p/a:b", "p/a_b"]
        out, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"])
        self.assertEqual(report["executed"], 2)
        ids = [c["case"] for c in report["cases"]]
        self.assertEqual(len(set(ids)), 2)
        self.assertTrue(all((out / (i + ".json")).is_file() for i in ids))

    def test_forwarded_limits_and_exit17(self):
        args = self.fixture()
        qs, out = eval_ask.validate_args(args)
        binary = pathlib.Path(args.sparkles)
        binary.write_text(
            "#!"
            + PYTHON
            + '\nimport json,sys\nprint(json.dumps({"outcome":"clarify"}))\nraise SystemExit(17)\n'
        )
        runner = eval_ask.Runner(args, qs, out)
        with self.assertRaises(Invalid):
            runner.ask("p/m", qs["questions"][0])
        status = json.loads(next(out.glob("*/status.json")).read_text())
        self.assertEqual(status["returncode"], 17)
        for flag, value in [
            ("--deadline", "300"),
            ("--timeout", "5"),
            ("--max-tokens", "32768"),
            ("--max-rows", "1000"),
        ]:
            self.assertEqual(status["argv"][status["argv"].index(flag) + 1], value)
        self.assertLessEqual(status["elapsedSeconds"], 2)

    def questions_fixture(self, kinds):
        args = self.fixture()
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"] = [
            {"id": f"q{i}", "kind": kind, "question": f"question {i}"}
            for i, kind in enumerate(kinds, 1)
        ]
        pathlib.Path(args.set).write_text(json.dumps(s))
        return args

    def fake_ask(self, args, body):
        binary = pathlib.Path(args.sparkles)
        binary.write_text("#!" + PYTHON + "\n" + body)
        args.sparkles_sha256 = digest(binary)

    def test_failed_ask_is_scored_and_does_not_end_the_run(self):
        args = self.questions_fixture(["clarify"] * 3)
        self.fake_ask(
            args,
            "import json,sys\n"
            'if "question 2" in sys.argv:\n'
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "    raise SystemExit(2)\n"
            'print(json.dumps({"outcome":"clarify"}))\n',
        )
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report["error"])
        self.assertEqual(report["scored"], 3)
        self.assertEqual(
            [c["detail"].get("status") for c in report["cases"]],
            [None, "provider_error", None],
        )
        row = report["rows"][0]
        self.assertEqual(row["endToEndSuccess"]["denominator"], 3)
        self.assertEqual(row["endToEndSuccess"]["numerator"], 2)
        self.assertEqual(row["failureStatuses"], {"provider_error": 1})

    def test_each_ask_failure_has_a_status(self):
        args = self.questions_fixture(["clarify"] * 4)
        args.deadline = 0.5
        grace = mock.patch.object(eval_ask, "QUERY_GRACE_SECONDS", 0.2)
        grace.start()
        self.addCleanup(grace.stop)
        self.fake_ask(
            args,
            "import json,sys,time\n"
            'q = sys.argv[sys.argv.index("--json") - 1]\n'
            'if q.endswith("1"):\n'
            "    time.sleep(30)\n"
            'if q.endswith("2"):\n'
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"},'
            '"usage":{"steps":[{"outcome":"refusal"}]}}))\n'
            "    raise SystemExit(2)\n"
            'if q.endswith("3"):\n'
            '    print(json.dumps({"outcome":"error","error":{"code":"timeout"}}))\n'
            "    raise SystemExit(2)\n"
            'print(json.dumps({"outcome":"checked"}))\n',
        )
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report["error"])
        self.assertEqual(
            [c["detail"]["status"] for c in report["cases"]],
            ["timeout", "refusal", "timeout", "invalid_output"],
        )

    def test_exceeding_the_ask_token_budget_is_its_own_status(self):
        args = self.questions_fixture(["clarify"])
        self.fake_ask(
            args,
            "import json\n"
            'print(json.dumps({"outcome":"error","error":{"code":"budget-exceeded"}}))\n'
            "raise SystemExit(2)\n",
        )
        _, report = eval_ask.run_matrix(args)
        self.assertEqual(report["cases"][0]["detail"]["status"], "budget_exceeded")
        self.assertEqual(report["rows"][0]["failureStatuses"], {"budget_exceeded": 1})
        self.assertEqual(report["askTokenBudget"], 32768)

    def test_in_flight_requests_settle_before_a_case_cost_is_read(self):
        order = []

        class Proxy:
            token, cap_refusals = "t", 0

            def set_case(self, case):
                order.append(("case", case is not None))

            def wait_idle(self):
                order.append("idle")

            def case_cost(self, case):
                order.append("cost")
                return 7

            def report(self):
                return {}

        args = self.questions_fixture(["clarify"])
        self.fake_ask(args, 'import json\nprint(json.dumps({"outcome":"clarify"}))\n')
        _, report = eval_ask.run_matrix(args, Proxy())
        self.assertLess(order.index("idle"), order.index("cost"))
        self.assertEqual(report["rows"][0]["ledgerCostUSD"], 7e-9)

    def test_unknown_exit_status_still_stops_the_run(self):
        args = self.questions_fixture(["clarify"] * 2)
        self.fake_ask(
            args,
            'import json\nprint(json.dumps({"outcome":"clarify"}))\nraise SystemExit(17)\n',
        )
        _, report = eval_ask.run_matrix(args)
        self.assertFalse(report["complete"])
        self.assertEqual(report["scored"], 0)
        self.assertEqual(report["error"], "Invalid")

    def test_failed_cleanup_of_a_query_child_is_a_harness_fault(self):
        args = self.questions_fixture(["answerable"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0]["gold"] = "EMPTY"
        pathlib.Path(args.set).write_text(json.dumps(s))
        self.fake_ask(
            args,
            "import json,sys\n"
            'if sys.argv[1] == "query":\n'
            '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":[]}}))\n'
            "else:\n"
            '    print(json.dumps({"outcome":"answered","result":{"query":"SELECT"}}))\n',
        )
        original = eval_ask.capture_child

        def capture(argv, directory, deadline, env=None):
            record = original(argv, directory, deadline, env)
            if argv[1] == "query" and directory.name.endswith("-query"):
                record["cleanup"] = False  # as after a failed process census
            return record

        eval_ask.capture_child = capture
        self.addCleanup(setattr, eval_ask, "capture_child", original)
        _, report = eval_ask.run_matrix(args)
        self.assertFalse(report["complete"])
        self.assertEqual(report["error"], "HarnessFault")
        statuses = [c["detail"].get("status") for c in report["cases"]]
        self.assertNotIn("candidate_query_failure", statuses)

    def test_answerable_failure_status_is_scored(self):
        args = self.questions_fixture(["answerable"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0]["gold"] = "EMPTY"
        pathlib.Path(args.set).write_text(json.dumps(s))
        self.fake_ask(
            args,
            "import json,sys\n"
            'if sys.argv[1] == "query":\n'
            '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":[]}}))\n'
            "else:\n"
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "    raise SystemExit(2)\n",
        )
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report["error"])
        self.assertEqual(report["cases"][0]["detail"]["status"], "provider_error")
        self.assertFalse(report["cases"][0]["correct"])

    def test_query_and_ask_children_get_grace_beyond_the_server_timeout(self):
        import evaluator

        args = self.questions_fixture(["answerable"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0]["gold"] = "EMPTY"
        pathlib.Path(args.set).write_text(json.dumps(s))
        args.timeout, args.deadline = 0.3, 0.3
        self.fake_ask(
            args,
            "import json,sys,time\n"
            "time.sleep(0.8)  # past the server timeout, within the grace\n"
            'if sys.argv[1] == "query":\n'
            '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":[]}}))\n'
            "else:\n"
            '    print(json.dumps({"outcome":"answered","result":{"query":"SELECT"}}))\n',
        )
        _, report = eval_ask.run_matrix(args)
        self.assertGreaterEqual(evaluator.QUERY_GRACE_SECONDS, 5)
        self.assertEqual(report["goldPreflight"][0]["status"], "complete")
        self.assertIsNone(report["cases"][0]["detail"].get("status"), report["cases"])

    def test_slow_gold_is_reported_as_a_gold_problem(self):
        args = self.questions_fixture(["answerable"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0]["gold"] = "SLOW"
        pathlib.Path(args.set).write_text(json.dumps(s))
        self.fake_ask(args, "raise SystemExit(1)\n")
        _, report = eval_ask.run_matrix(args)
        self.assertFalse(report["complete"])
        self.assertEqual(report["error"], "GoldFault")
        self.assertIn("q1", report["errorDetail"])
        self.assertEqual(report["goldPreflight"][0]["status"], "failed")
        self.assertEqual(report["executed"], 0)

    def test_run_with_a_shorter_query_timeout_than_the_gold_was_made_with(self):
        args = self.questions_fixture(["clarify"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["goldTimeoutSecs"] = 60
        pathlib.Path(args.set).write_text(json.dumps(s))
        args.timeout = 5
        with self.assertRaises(Invalid):
            eval_ask.validate_args(args)
        args = self.questions_fixture(["clarify"])
        s["goldTimeoutSecs"] = 5
        pathlib.Path(args.set).write_text(json.dumps(s))
        args.out = str(self.root / "second")
        eval_ask.validate_args(args)

    def test_systematic_provider_errors_stop_the_run_as_incomplete(self):
        args = self.questions_fixture(["clarify"] * 8)
        args.max_consecutive_errors = 3
        self.fake_ask(
            args,
            "import json\n"
            'print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "raise SystemExit(2)\n",
        )
        _, report = eval_ask.run_matrix(args)
        self.assertFalse(report["complete"])
        self.assertEqual(report["error"], "HarnessFault")
        self.assertIn("consecutive", report["errorDetail"])
        self.assertEqual(report["scored"], 3)
        # A success in between resets the count.
        args = self.questions_fixture(["clarify"] * 6)
        args.max_consecutive_errors = 3
        self.fake_ask(
            args,
            "import json,sys\n"
            'if int(sys.argv[sys.argv.index("--json") - 1][-1]) % 3:\n'
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "    raise SystemExit(2)\n"
            'print(json.dumps({"outcome":"clarify"}))\n',
        )
        args.out = str(self.root / "second")
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report["error"])

    def test_cap_refusal_is_excluded_and_stops_the_run(self):
        class Proxy:
            token, cap_refusals = "t", 0

            def set_case(self, case):
                pass

            def wait_idle(self):
                pass

            def case_cost(self, case):
                return 0

            def report(self):
                return {"capRefusals": self.cap_refusals}

        proxy = Proxy()
        args = self.questions_fixture(["clarify"] * 3)
        marker = self.root / "calls"
        self.fake_ask(
            args,
            "import json\n"
            f'open({str(marker)!r}, "a").write("x")\n'
            'print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "raise SystemExit(2)\n",
        )
        original = eval_ask.Runner.classify

        def classify(runner, record, directory):
            proxy.cap_refusals += 1
            return original(runner, record, directory)

        eval_ask.Runner.classify = classify
        self.addCleanup(setattr, eval_ask.Runner, "classify", original)
        _, report = eval_ask.run_matrix(args, proxy)
        self.assertFalse(report["complete"])
        self.assertTrue(report["budgetStopped"])
        self.assertEqual(marker.read_text(), "x")
        row = report["rows"][0]
        self.assertEqual(row["endToEndSuccess"]["denominator"], 0)
        self.assertTrue(row["incomplete"])
        self.assertEqual(report["unattemptedIds"], {"p/m": ["q1", "q2", "q3"]})
        self.assertEqual(report["cases"][0]["detail"]["status"], "budget_refused")

    def test_unadmitted_pair_is_not_run_and_the_cascade_is_withheld(self):
        args = self.questions_fixture(["clarify"] * 2)
        pathlib.Path(args.model_config).write_text(
            json.dumps({"models": {"providers": {"p": {"models": {"a": {}, "b": {}}}}}})
        )
        args.pair = ["p/a", "p/b"]
        self.fake_ask(args, 'import json\nprint(json.dumps({"outcome":"clarify"}))\n')
        _, report = eval_ask.run_matrix(args, admit=lambda pair, qs: pair == "p/a")
        self.assertFalse(report["complete"])
        self.assertEqual(report["executed"], 2)
        self.assertEqual(report["notRun"], [{"pair": "p/b", "reason": "budget"}])
        self.assertEqual([r["pair"] for r in report["rows"]], ["p/a"])
        self.assertEqual(report["unattemptedIds"], {"p/b": ["q1", "q2"]})

    def test_cascade_row_is_computed_from_pair_runs(self):
        args = self.questions_fixture(["clarify"] * 3)
        pathlib.Path(args.model_config).write_text(
            json.dumps({"models": {"providers": {"p": {"models": {"a": {}, "b": {}}}}}})
        )
        args.pair = ["p/a", "p/b"]
        # The first pair fails on question 2, so the cascade takes the second pair.
        self.fake_ask(
            args,
            "import json,sys\n"
            'q = sys.argv[sys.argv.index("--json") - 1]\n'
            'pair = sys.argv[sys.argv.index("--pair") + 1]\n'
            'if pair.endswith("p/a") and q.endswith("2"):\n'
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"},'
            '"usage":{"inputTokens":5}}))\n'
            "    raise SystemExit(2)\n"
            'print(json.dumps({"outcome":"clarify","usage":{"inputTokens":10,"outputTokens":1}}))\n',
        )
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report["error"])
        self.assertEqual(report["planned"], 6)
        self.assertEqual(report["executed"], 6)
        rows = {r["pair"]: r for r in report["rows"]}
        cascade = rows["p/a > p/b"]
        self.assertEqual(cascade["derivation"], "computed from the per-pair runs")
        self.assertEqual(cascade["answeredBy"], {"p/a": 2, "p/b": 1})
        self.assertEqual(cascade["endToEndSuccess"]["numerator"], 3)
        self.assertEqual(rows["p/a"]["endToEndSuccess"]["numerator"], 2)
        # The cascade pays for the failed first try as well as the second pair.
        self.assertEqual(cascade["cliInputTokens"], 35)

    def test_invalid_gold_prevents_ask(self):
        args = self.fixture()
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0].update(kind="answerable", gold="SELECT")
        pathlib.Path(args.set).write_text(json.dumps(s))
        # The real fake executable emits malformed query results, not a score stub.
        b = pathlib.Path(args.sparkles)
        b.write_text("#!" + PYTHON + '\nprint("{}")\n')
        args.sparkles_sha256 = digest(b)
        _, report = eval_ask.run_matrix(args)
        self.assertFalse(report["complete"])
        self.assertEqual(report["executed"], 0)
        status = json.loads(
            next(pathlib.Path(args.out).glob("*/status.json")).read_text()
        )
        self.assertEqual(status["argv"][1], "query")


class ProcessCensus(unittest.TestCase):
    def test_a_process_that_exits_during_the_census_is_not_a_failure(self):
        # A process can exit between open and read of its /proc stat file, which
        # raises ProcessLookupError. It aborted a dry run after 127 cases.
        from unittest import mock

        import evaluator

        real = pathlib.Path.read_text
        raised = []

        def read_text(path, *a, **kw):
            if path.name == "stat" and not raised:
                raised.append(path)
                raise ProcessLookupError(3, "No such process")
            return real(path, *a, **kw)

        with mock.patch.object(pathlib.Path, "read_text", read_text):
            states = evaluator.group_states(1)
        self.assertTrue(raised)
        self.assertIsInstance(states, list)
        # A torn stat line is skipped like a vanished process.
        with mock.patch.object(pathlib.Path, "read_text", lambda *a, **k: "1 (x"):
            self.assertEqual(evaluator.group_states(1), [])

    def test_capture_child_survives_the_race(self):
        from unittest import mock

        real = pathlib.Path.read_text
        calls = []

        def read_text(path, *a, **kw):
            if path.name == "stat" and len(calls) < 40:
                calls.append(path)
                raise ProcessLookupError(3, "No such process")
            return real(path, *a, **kw)

        with (
            tempfile.TemporaryDirectory(prefix="ask-census-") as d,
            mock.patch.object(pathlib.Path, "read_text", read_text),
        ):
            record = capture_child(
                [PYTHON, "-c", "print(1)"],
                pathlib.Path(d) / "child",
                time.monotonic() + 5,
            )
        self.assertTrue(calls)
        self.assertTrue(record["cleanup"])
        self.assertEqual(record["returncode"], 0)


class ExtraInvariants(unittest.TestCase):
    def test_empty_arity_is_not_a_match_and_nonfinite_exponents(self):
        # An answer narrower than the gold never matches, even when both are empty.
        self.assertEqual(
            prf(answer_set(result([], ("x", "y"))), answer_set(result([], ("x",)))),
            (0, 0, 0),
        )
        # A wider one is projected onto the gold's columns, so empty matches empty.
        self.assertEqual(
            prf(answer_set(result([], ("x",))), answer_set(result([], ("x", "y")))),
            (1, 1, 1),
        )
        with self.assertRaises(Invalid):
            strict_json('{"number":1e9999}')
        with self.assertRaises(Invalid):
            norm_term(literal("x", datatype=""))

    def test_exhausted_phase_never_spawns(self):
        with (
            tempfile.TemporaryDirectory(prefix="ask-expired-") as d,
            self.assertRaises(Invalid),
        ):
            capture_child(
                ["/not-an-executable"],
                pathlib.Path(d) / "child",
                time.monotonic() - 1,
            )


class Clarifications(unittest.TestCase):
    """The evaluator answers a clarification as a person who means the gold would."""

    GOLD = (
        "PREFIX res: <http://dbpedia.org/resource/>\n"
        "# res:Salt_Lake_City,_Utah is only in a comment\n"
        'SELECT ?tz WHERE { res:Salt_Lake_City <http://dbpedia.org/ontology/timeZone> ?tz .'
        ' ?tz <http://www.w3.org/2000/01/rdf-schema#label> "res:Diocese" .'
        " res:Washington\\,_D.C\\. ?p ?o . und:Berlin ?p ?o }"
    )

    @staticmethod
    def choices(*pairs):
        return {"choices": [{"label": label, "value": value} for label, value in pairs]}

    def test_gold_terms_follow_the_lexical_rules(self):
        iris, words = gold_terms(self.GOLD)
        self.assertIn("http://dbpedia.org/resource/Salt_Lake_City", iris)
        self.assertIn("http://dbpedia.org/resource/Washington,_D.C.", iris)
        self.assertIn("http://dbpedia.org/ontology/timeZone", iris)
        # A comment, a literal, an undeclared prefix and the declaration itself are not
        # names the query uses.
        self.assertNotIn("http://dbpedia.org/resource/Salt_Lake_City,_Utah", iris)
        self.assertNotIn("http://dbpedia.org/resource/Diocese", iris)
        self.assertNotIn("http://dbpedia.org/resource/", iris)
        self.assertFalse(any("Berlin" in i for i in iris))
        self.assertTrue({"salt", "lake", "city", "timezone", "diocese"} <= words)
        # An unescaped final dot ends the triple pattern.
        iris, _ = gold_terms("PREFIX r: <http://x/> SELECT * { r:a r:b r:c. }")
        self.assertEqual(iris, {"http://x/a", "http://x/b", "http://x/c"})

    def test_an_offered_iri_the_gold_names_wins(self):
        c = self.choices(
            ("Salt Lake City (Category)", "http://dbpedia.org/resource/Category:Salt_Lake_City"),
            ("Salt Lake City, Utah", "http://dbpedia.org/resource/Salt_Lake_City,_Utah"),
            ("Salt Lake City (dbo:City)", "http://dbpedia.org/resource/Salt_Lake_City"),
        )
        self.assertEqual(
            choose_clarification(c, self.GOLD),
            {
                "answer": "http://dbpedia.org/resource/Salt_Lake_City",
                "index": 2,
                "basis": "gold-iri",
            },
        )

    def test_words_decide_a_free_text_choice_and_ties_go_to_the_first(self):
        c = self.choices(("the mayor", "the mayor"), ("its time zone", "its time zone"))
        self.assertEqual(choose_clarification(c, self.GOLD)["index"], 1)
        self.assertEqual(choose_clarification(c, self.GOLD)["basis"], "gold-words")
        c = self.choices(("salt flats", "a"), ("salt mine", "b"))
        self.assertEqual(
            choose_clarification(c, self.GOLD),
            {"answer": "a", "index": 0, "basis": "gold-words-tie"},
        )
        c = self.choices(("red", "red"), ("blue", "blue"))
        self.assertEqual(
            choose_clarification(c, self.GOLD),
            {"answer": "red", "index": 0, "basis": "first-choice"},
        )

    def test_the_answer_is_always_an_offered_value(self):
        # The gold names Salt_Lake_City, which is not offered, so it never reaches the
        # pipeline.
        c = self.choices(("Berlin", "http://dbpedia.org/resource/Berlin"))
        out = choose_clarification(c, self.GOLD)
        self.assertEqual(out["answer"], "http://dbpedia.org/resource/Berlin")

    def test_relative_iris_resolve_against_the_base_in_force(self):
        gold = (
            "BASE <http://dbpedia.org/resource/>\n"
            "PREFIX rel: <sub/>\n"
            "SELECT ?x { <Berlin> <http://ex/p> ?x . rel:a ?p ?x }"
        )
        iris, _ = gold_terms(gold)
        self.assertEqual(
            iris,
            {
                "http://dbpedia.org/resource/Berlin",
                "http://ex/p",
                "http://dbpedia.org/resource/sub/a",
            },
        )
        c = self.choices(
            ("Berlin", "http://dbpedia.org/resource/Berlin,_New_Hampshire"),
            ("Berlin", "http://dbpedia.org/resource/Berlin"),
        )
        self.assertEqual(choose_clarification(c, gold)["index"], 1)
        self.assertEqual(choose_clarification(c, gold)["basis"], "gold-iri")

    def test_unusable_clarifications(self):
        for clarify in [
            None,
            {},
            {"choices": []},
            {"choices": ["plain"]},
            {"choices": [{"label": "x"}]},
            {"choices": [{"label": "x", "value": " "}]},
            {"choices": [{"label": "x", "value": "a\nb"}]},
            {"choices": [{"label": 3, "value": "a"}]},
            {"choices": [{"label": "x", "value": "v"}] * 17},
        ]:
            with self.subTest(clarify=clarify):
                self.assertIsNone(choose_clarification(clarify, self.GOLD))


class ClarifiedRuns(unittest.TestCase):
    setUp = Processes.setUp
    child = Processes.child
    fixture = Processes.fixture
    questions_fixture = Processes.questions_fixture
    fake_ask = Processes.fake_ask

    # The fake asks which "B" is meant, offering a value that starts with a dash and
    # the gold's IRI, and answers only when the argument names the gold's IRI.
    BODY = (
        "import json,sys\n"
        'if sys.argv[1] == "query":\n'
        '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":'
        '[{"x":{"type":"uri","value":"http://ex/v"}}]}}))\n'
        "    raise SystemExit(0)\n"
        'given = [a for a in sys.argv if a.startswith("--clarification")]\n'
        "if not given:\n"
        '    print(json.dumps({"outcome":"clarify","clarify":{"id":"mention",'
        '"question":"Which B?","choices":[{"label":"B (dash)","value":"-B"},'
        '{"label":"B","value":"http://ex/B"}]},'
        '"usage":{"inputTokens":3,"outputTokens":1,"estimatedCost":0.5}}))\n'
        'elif given == ["--clarification=http://ex/B"]:\n'
        '    print(json.dumps({"outcome":"answered","result":{"query":'
        '"SELECT ?x WHERE { <http://ex/B> <http://ex/p> ?x }"},'
        '"usage":{"inputTokens":10,"outputTokens":2,"estimatedCost":1.0}}))\n'
        "else:\n"
        '    print(json.dumps({"outcome":"answered","result":{"query":"SELECT ?x {}"}}))\n'
    )

    def run_set(self, kinds, policy=None):
        args = self.questions_fixture(kinds)
        s = json.loads(pathlib.Path(args.set).read_text())
        for q in s["questions"]:
            if q["kind"] == "answerable":
                q["gold"] = "SELECT ?x WHERE { <http://ex/B> <http://ex/p> ?x }"
        pathlib.Path(args.set).write_text(json.dumps(s))
        if policy:
            args.clarifications = policy
        self.fake_ask(args, self.BODY)
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report.get("errorDetail"))
        return args, report

    def test_an_answered_clarification_scores_the_second_ask(self):
        args, report = self.run_set(["answerable", "clarify"])
        self.assertEqual(report["clarificationPolicy"], "answer")
        first, second = report["cases"]
        self.assertTrue(first["correct"], first)
        self.assertTrue(second["correct"], second)  # a clarify question is not answered
        saved = json.loads(
            (pathlib.Path(args.out) / (first["case"] + ".json")).read_text()
        )
        answer = saved["answer"]
        self.assertEqual(answer["clarification"]["answer"], "http://ex/B")
        self.assertEqual(answer["clarification"]["basis"], "gold-iri")
        self.assertEqual(answer["clarification"]["question"], "Which B?")
        self.assertEqual(answer["usage"]["inputTokens"], 13)
        self.assertEqual(answer["usage"]["outputTokens"], 3)
        self.assertEqual(answer["usage"]["estimatedCost"], 1.5)
        self.assertGreater(
            answer["wallSeconds"], answer["clarification"]["firstExecution"]["elapsedSeconds"]
        )
        labels = sorted(p.name.split("-", 1)[1] for p in pathlib.Path(args.out).iterdir() if p.is_dir())
        self.assertEqual(labels.count("ask-clarified"), 1)
        row = report["rows"][0]
        self.assertEqual(row["clarificationsAnswered"]["numerator"], 1)
        self.assertEqual(row["clarificationBasis"], {"gold-iri": 1})
        self.assertEqual(row["correctAfterClarification"], 1)
        # Both questions asked, and only the clarify question was meant to.
        self.assertEqual(row["clarificationPrecision"]["numerator"], 1)
        self.assertEqual(row["clarificationPrecision"]["denominator"], 2)

    def test_a_cascade_keeps_the_clarification_of_a_pair_that_failed_after_it(self):
        args, _ = self.run_set(["answerable"])
        cfg = json.loads(pathlib.Path(args.model_config).read_text())
        cfg["models"]["providers"]["p"]["models"]["m2"] = {}
        pathlib.Path(args.model_config).write_text(json.dumps(cfg))
        args.pair = ["p/m", "p/m2"]
        args.out = str(pathlib.Path(args.out).with_name("cascade"))
        # The first pair asks, and fails once the clarification is answered. The second
        # answers without asking.
        self.fake_ask(
            args,
            "import json,sys\n"
            'if sys.argv[1] == "query":\n'
            '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":'
            '[{"x":{"type":"uri","value":"http://ex/v"}}]}}))\n'
            'elif "draft=p/m2" in sys.argv:\n'
            '    print(json.dumps({"outcome":"answered","result":{"query":'
            '"SELECT ?x WHERE { <http://ex/B> <http://ex/p> ?x }"}}))\n'
            'elif not any(a.startswith("--clarification") for a in sys.argv):\n'
            '    print(json.dumps({"outcome":"clarify","clarify":{"id":"mention",'
            '"question":"Which B?","choices":[{"label":"B","value":"http://ex/B"}]}}))\n'
            "else:\n"
            '    print(json.dumps({"outcome":"error","error":{"code":"provider-unavailable"}}))\n'
            "    raise SystemExit(2)\n",
        )
        _, report = eval_ask.run_matrix(args)
        self.assertTrue(report["complete"], report.get("errorDetail"))
        first, second, cascade = report["rows"]
        self.assertEqual(first["failureStatuses"], {"provider_error": 1})
        self.assertEqual(first["clarificationBasis"], {"gold-iri": 1})
        self.assertEqual(second["clarificationPrecision"]["denominator"], 0)
        self.assertEqual(cascade["executionAccuracy"]["numerator"], 1)
        self.assertEqual(cascade["clarificationPrecision"]["denominator"], 1)
        self.assertEqual(cascade["clarificationsAnswered"]["numerator"], 1)
        self.assertEqual(cascade["clarificationBasis"], {"gold-iri": 1})

    def test_the_score_policy_keeps_the_clarification_a_failure(self):
        args, report = self.run_set(["answerable"], policy="score")
        self.assertEqual(report["clarificationPolicy"], "score")
        case = report["cases"][0]
        self.assertFalse(case["correct"])
        self.assertEqual(case["detail"]["status"], "pipeline_clarify")
        self.assertEqual(report["rows"][0]["clarificationsAnswered"]["numerator"], 0)

    def test_an_unusable_clarification_is_scored_as_asked(self):
        args = self.questions_fixture(["answerable"])
        s = json.loads(pathlib.Path(args.set).read_text())
        s["questions"][0]["gold"] = "SELECT ?x WHERE { <http://ex/B> <http://ex/p> ?x }"
        pathlib.Path(args.set).write_text(json.dumps(s))
        self.fake_ask(
            args,
            "import json,sys\n"
            'if sys.argv[1] == "query":\n'
            '    print(json.dumps({"head":{"vars":["x"]},"results":{"bindings":[]}}))\n'
            "else:\n"
            '    print(json.dumps({"outcome":"clarify","clarify":{"choices":[]}}))\n',
        )
        _, report = eval_ask.run_matrix(args)
        case = report["cases"][0]
        self.assertEqual(case["detail"]["status"], "pipeline_clarify")
        self.assertEqual(report["rows"][0]["clarificationBasis"], {"unusable": 1})
        self.assertEqual(report["rows"][0]["clarificationsAnswered"]["numerator"], 0)


class FirstDraft(unittest.TestCase):
    setUp = Processes.setUp
    child = Processes.child
    fixture = Processes.fixture

    def test_empty_correct_repaired_is_not_failed_first(self):
        args = self.fixture()
        qs, out = eval_ask.validate_args(args)
        q = dict(qs["questions"][0], kind="answerable", gold="EMPTY")
        runner = eval_ask.Runner(args, qs, out)
        runner.gold_cache[q["id"]] = answer_set(result([], ("x",)))
        # Actual bounded fake-CLI reads under the same data identity. This is a
        # scoring fixture, not a claim that a real provider produced these drafts.
        b = pathlib.Path(args.sparkles)
        b.write_text(
            "#!"
            + PYTHON
            + '\nimport json,sys\nprint(json.dumps({"head":{"vars":["x"]},"results":{"bindings":[] if sys.argv[-1]=="EMPTY" else [{"x":{"type":"literal","value":"wrong"}}]}}))\n'
        )

        def answer(first, check=None):
            return {
                "outcome": "empty",
                "result": {"query": "EMPTY", "attempt": 2},
                "attempts": [
                    {"query": first, "check": check or {"ok": True, "issues": []}}
                ],
            }

        cases = [
            ("EMPTY", None, "correct"),
            ("WRONG", None, "incorrect"),
            (
                "WRONG",
                {
                    "ok": False,
                    "issues": [{"severity": "error", "code": "fixture-invalid"}],
                },
                "incorrect",
            ),
        ]
        for text, check, status in cases:
            a = answer(text, check)
            score = runner.score(q, a)
            self.assertTrue(score[0])
            self.assertEqual(score[2]["firstDraft"]["status"], status)
            row = eval_ask.summarize_pair("p/m", [q], [a], [score])[0]
            self.assertEqual(row["repairRate"]["numerator"], int(status == "incorrect"))
            self.assertEqual(row["repairInvoked"]["numerator"], 1)
        a = answer("WRONG", {"ok": True, "issues": [{"severity": "warning"}]})
        self.assertEqual(
            runner.score(q, a)[2]["firstDraft"]["reason"], "complete_strict_comparison"
        )
        # A question whose source scores row order is marked, since rows are sets here.
        self.assertNotIn("orderNotScored", runner.score(q, answer("EMPTY"))[2])
        ordered = dict(q, orderMatters=True)
        self.assertIs(runner.score(ordered, answer("EMPTY"))[2]["orderNotScored"], True)
        failed = dict(answer("EMPTY"), evalFailure="timeout")
        self.assertIs(runner.score(ordered, failed)[2]["orderNotScored"], True)
        # A real malformed child result is unknown, never silently failed-first.
        b.write_text("#!" + PYTHON + '\nprint("{}")\n')
        probe = runner.first_draft(q, answer("WRONG"))
        self.assertEqual(probe["status"], "unknown")
        self.assertEqual(probe["reason"], "probe_incomplete")
        score = (
            True,
            (1, 1, 1),
            {
                "kind": "select",
                "firstDraft": probe,
                "intersection": 0,
                "goldRows": 0,
                "gotRows": 0,
            },
        )
        row = eval_ask.summarize_pair("p/m", [q], [answer("WRONG")], [score])[0]
        self.assertEqual(row["firstDraftUnknown"]["numerator"], 1)
        self.assertEqual(row["repairConditional"]["denominator"], 0)

    def test_comment_endings_service_pre_spawn(self):
        from evaluator import OfflinePolicy

        args = self.fixture()
        qs, out = eval_ask.validate_args(args)
        runner = eval_ask.Runner(args, qs, out)
        executions = []

        def execute(argv, label, *unused):
            executions.append({"argv": argv, "label": label})
            return {"head": {}, "boolean": True}, {}

        runner.execute = execute
        for ending in ("\r", "\n", "\r\n"):
            for keyword in ("SERVICE", "sErViCe", "SERVICE SILENT"):
                text = (
                    "ASK { # actual comment"
                    + ending
                    + keyword
                    + " <urn:endpoint> {?s ?p ?o}}"
                )
                q = {"id": ending + keyword, "kind": "answerable", "gold": text}
                refused = False
                try:
                    runner.gold(q)
                except OfflinePolicy:
                    refused = True
                first = runner.first_draft(
                    q, {"attempts": [{"query": text, "check": {"issues": []}}]}
                )
                # Assert receipts first so the old CR-only scanner fails genuinely.
                self.assertEqual(executions, [])
                self.assertTrue(refused)
                self.assertEqual(first["status"], "unknown")
                self.assertEqual(first["reason"], "offline_policy_exclusion")
                row = eval_ask.summarize_pair(
                    "p/m",
                    [q],
                    [{"outcome": "empty"}],
                    [(True, (1, 1, 1), {"kind": "ask", "firstDraft": first})],
                )[0]
                self.assertEqual(row["repairConditional"]["denominator"], 0)
                self.assertEqual(row["firstDraftUnknown"]["numerator"], 1)

    def test_offline_service_and_names(self):
        from evaluator import OfflinePolicy, offline_query

        for text in [
            "SELECT ?x WHERE {SERVICE <https://example.invalid/q> {?x ?p ?o}}",
            "SELECT ?x WHERE {sErViCe ?endpoint {?x ?p ?o}}",
        ]:
            with self.assertRaises(OfflinePolicy):
                offline_query(text)
        for text in [
            "PREFIX ex:<urn:example:> SELECT ?service WHERE {?service a ex:Service}",
            'SELECT ?x WHERE {?x ?p "SERVICE"} # SERVICE ignored',
            "SELECT ?x WHERE {?x ?p <urn:SERVICE>}",
            "SELECT ?x WHERE {FILTER(?x<1) ?x ?p ?o}",
        ]:
            self.assertEqual(offline_query(text), text)
        for text in [
            r"SELECT ?x WHERE {SER\u0056ICE <http://127.0.0.1:1/> {?x ?p ?o}}",
            r'SELECT ?x WHERE {?x ?p "\u0053ERVICE"}',
            r"# \u000a SERVICE <http://127.0.0.1:1/> {}",
            "SELECT ?x WHERE {SERVICE SILENT <http://127.0.0.1:1/> {?x ?p ?o}}",
        ]:
            with self.assertRaises(OfflinePolicy):
                offline_query(text)
        for text in [
            "PREFIX :<urn:example:> SELECT ?service WHERE {?service a :SERVICE}",
            'SELECT ?x WHERE {?x ?p """a SERVICE long string"""}',
            "SELECT ?x WHERE {?x ?p 'a SERVICE string'}",
            "# SERVICE\n SELECT ?x WHERE {?x ?p <urn:service>}",
        ]:
            self.assertEqual(offline_query(text), text)
        args = self.fixture()
        qs, out = eval_ask.validate_args(args)
        runner = eval_ask.Runner(args, qs, out)
        with self.assertRaises(OfflinePolicy):
            runner.query("SELECT ?x WHERE {SERVICE <http://127.0.0.1:1/> {?x ?p ?o}}")
        self.assertEqual(list(out.iterdir()), [])


class RecordShapes(unittest.TestCase):
    def test_invalid_optional_record_fields_are_not_scored(self):
        from evaluator import validate_ask_record

        for field, value in [
            ("usage", []),
            ("result", "bad"),
            ("attempts", [False]),
            ("usage", {"steps": [None]}),
            ("usage", {"inputTokens": True}),
            ("usage", {"estimatedCost": -1}),
            ("result", {"query": None}),
            ("attempts", [{"check": {"issues": [True]}}]),
        ]:
            with self.subTest(field=field, value=value), self.assertRaises(Invalid):
                validate_ask_record({"outcome": "clarify", field: value})
        self.assertEqual(
            validate_ask_record({"outcome": "clarify"}), {"outcome": "clarify"}
        )


if __name__ == "__main__":
    unittest.main()
