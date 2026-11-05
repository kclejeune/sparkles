import contextlib
import copy
import importlib.util
import io
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import tempfile
import threading
from types import SimpleNamespace
import unittest
from unittest import mock


spec = importlib.util.spec_from_file_location(
    "qerror", Path(__file__).resolve().parents[1] / "bench-estimates" / "qerror.py"
)
qerror = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qerror)


def uri(value):
    return {"type": "uri", "value": value}


def graph(value):
    return {
        "queryType": "CONSTRUCT",
        "triples": [[uri("urn:s"), uri("urn:p"), uri(value)]],
        "meta": {"sentRows": 1, "totalRows": 1},
    }


def response(kind="SELECT", **fields):
    count = 1 if kind == "ASK" else len(fields.get("rows", [])) if kind == "SELECT" else len(fields.get("triples", [])) + len(fields.get("quads", []))
    return {"queryType": kind, **fields, "meta": {"sentRows": count, "totalRows": count}}


def literal(value, **fields):
    return {"type": "literal", "value": value, **fields}


def bnode(value):
    return {"type": "bnode", "value": value}


def record(value):
    digest, count = qerror.digest(value)
    return {"digestFormat": 1, "queryType": value["queryType"], "digest": digest, "rows": count}


class QerrorAnswerTests(unittest.TestCase):
    def test_directional_literals_are_explicitly_unsupported(self):
        for direction in ("ltr", "rtl", "up", None):
            value = response(vars=["x"], rows=[[literal("text", **{"xml:lang": "en", "its:dir": direction})]])
            with self.subTest(direction=direction), self.assertRaisesRegex(ValueError, "direction"):
                qerror.digest(value)

    def test_post_refuses_eager_describe_truncation_header(self):
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                body = json.dumps(response("DESCRIBE", triples=[])).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/x-sparkles+json")
                self.send_header("sparkles-describe-truncated", "true")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            with self.assertRaisesRegex(ValueError, "truncat"):
                qerror.post(f"http://127.0.0.1:{server.server_port}/sparql", "DESCRIBE <urn:s>", 2)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_graph_content_is_compared(self):
        self.assertNotEqual(qerror.digest(graph("urn:a")), qerror.digest(graph("urn:b")))

    def test_error_response_is_rejected(self):
        with self.assertRaises(ValueError):
            qerror.digest({"error": "query failed"})

    def test_missing_fingerprints_are_not_same(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ["base", "candidate"]:
                (root / name).write_text(json.dumps({"query": "q", "nodes": []}) + "\n")
            output = io.StringIO()
            with contextlib.redirect_stdout(output):
                qerror.compare(SimpleNamespace(base=root / "base", cand=root / "candidate", md=None))
            self.assertIn("unverified", output.getvalue())

    def test_select_schema_order_and_duplicates(self):
        a = response(vars=["s", "o"], rows=[[uri("urn:s"), literal("x")]])
        b = response(vars=["o", "s"], rows=[[literal("x"), uri("urn:s")]])
        self.assertEqual(qerror.digest(a), qerror.digest(b))
        doubled = response(vars=["s", "o"], rows=a["rows"] * 2)
        self.assertNotEqual(qerror.digest(a), qerror.digest(doubled))
        renamed = response(vars=["different", "o"], rows=a["rows"])
        self.assertNotEqual(qerror.digest(a), qerror.digest(renamed))

    def test_rdf_literal_identity(self):
        def select(term):
            return response(vars=["x"], rows=[[term]])
        self.assertEqual(qerror.digest(select(literal("x"))), qerror.digest(select(literal("x", datatype=qerror.XSD_STRING))))
        upper = literal("x", **{"xml:lang": "EN"})
        lower = literal("x", datatype=qerror.RDF_LANG_STRING, **{"xml:lang": "en"})
        self.assertEqual(qerror.digest(select(upper)), qerror.digest(select(lower)))
        self.assertNotEqual(qerror.digest(select(literal("1"))), qerror.digest(select(literal("01"))))
        self.assertNotEqual(qerror.digest(select(literal("1"))), qerror.digest(select(literal("1", datatype="urn:datatype"))))

    def test_ask_boolean_and_empty_kind_distinctions(self):
        true = response("ASK", boolean=True)
        false = response("ASK", boolean=False)
        self.assertEqual(qerror.digest(true)[1], 1)
        self.assertNotEqual(qerror.digest(true), qerror.digest(false))
        empty = [response(vars=[], rows=[]), response("CONSTRUCT", triples=[]), response("DESCRIBE", triples=[])]
        self.assertEqual(len({qerror.digest(r)[0] for r in empty}), 3)
        for value in [None, 0, 1, "false"]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                qerror.digest(response("ASK", boolean=value))

    def test_graph_set_and_default_quad_representation(self):
        a = graph("urn:o")
        duplicated = response("CONSTRUCT", triples=a["triples"] * 2)
        streamed = response("CONSTRUCT", triples=[], quads=[a["triples"][0] + [None]])
        self.assertEqual(qerror.digest(a), qerror.digest(duplicated))
        self.assertEqual(qerror.digest(a), qerror.digest(streamed))
        named = response("CONSTRUCT", quads=[a["triples"][0] + [uri("urn:g")]])
        self.assertNotEqual(qerror.digest(a), qerror.digest(named))

    def test_blank_node_relabeling_and_shared_identity(self):
        a = response("CONSTRUCT", triples=[[bnode("a"), uri("urn:p"), bnode("b")], [bnode("b"), uri("urn:p"), literal("x")]])
        b = response("CONSTRUCT", triples=[[bnode("y"), uri("urn:p"), literal("x")], [bnode("x"), uri("urn:p"), bnode("y")]])
        self.assertEqual(qerror.digest(a), qerror.digest(b))
        distinct = response(vars=["x"], rows=[[bnode("a")], [bnode("b")]])
        shared = response(vars=["x"], rows=[[bnode("a")], [bnode("a")]])
        self.assertNotEqual(qerror.digest(distinct), qerror.digest(shared))
        same_graph_node = response("CONSTRUCT", quads=[[bnode("a"), uri("urn:p"), literal("x"), bnode("a")]])
        other_graph_node = response("CONSTRUCT", quads=[[bnode("a"), uri("urn:p"), literal("x"), bnode("b")]])
        self.assertNotEqual(qerror.digest(same_graph_node), qerror.digest(other_graph_node))

    def test_regular_nonisomorphic_graphs_differ(self):
        def cycles(sizes):
            triples, offset = [], 0
            for size in sizes:
                for i in range(size):
                    triples.append([bnode(str(offset + i)), uri("urn:p"), bnode(str(offset + (i + 1) % size))])
                offset += size
            return response("CONSTRUCT", triples=triples)
        self.assertNotEqual(qerror.digest(cycles([6])), qerror.digest(cycles([3, 3])))

    def test_canonicalization_budget_is_checked_before_permutations(self):
        cases = [response(vars=["x"], rows=[[bnode(str(i))] for i in range(9)]),
                 response(vars=["x", "i"], rows=[[bnode(str(i % 8)), literal(str(i))] for i in range(25)])]
        with mock.patch.object(qerror.itertools, "permutations", side_effect=AssertionError("search started")):
            for value in cases:
                with self.subTest(value=value), self.assertRaisesRegex(ValueError, "budget"):
                    qerror.digest(value)

    def test_incomplete_and_truncated_native_answers_reject(self):
        original = graph("urn:o")
        for field, value in [("sentRows", 0), ("totalRows", None), ("totalRows", True), ("status", "stopped"), ("status", "failed"), ("describeTruncated", True)]:
            changed = copy.deepcopy(original)
            changed["meta"][field] = value
            with self.subTest(field=field, value=value), self.assertRaises(ValueError):
                qerror.digest(changed)
        original["describeTruncated"] = True
        with self.assertRaises(ValueError):
            qerror.digest(original)

    def test_malformed_and_unsupported_answers_reject(self):
        cases = [response("UNKNOWN"), {"queryType": {}}, response(vars=["x"], rows=[[]]),
                 response(vars=["x", "x"], rows=[[None, None]]), response("CONSTRUCT", triples=[[uri("urn:s")]]),
                 response("CONSTRUCT", triples=[[literal("bad"), uri("urn:p"), uri("urn:o")]]),
                 response(vars=["x"], rows=[[{"type": "triple", "value": {}}]]),
                 response(vars=["x"], rows=[[{"type": {}, "value": "x"}]])]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                qerror.digest(value)

    def test_versioned_comparison_and_legacy_unverified(self):
        a = record(graph("urn:a"))
        b = record(graph("urn:b"))
        self.assertTrue(qerror.answers_equal(a, dict(a)))
        self.assertFalse(qerror.answers_equal(a, b))
        self.assertIsNone(qerror.answers_equal({"digest": "old"}, {"digest": "old"}))
        self.assertIsNone(qerror.answers_equal(a, {**a, "error": "failed"}))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "record.jsonl"
            for version in [2, True, "1"]:
                path.write_text(json.dumps({"query": "q", "digestFormat": version}) + "\n")
                with self.subTest(version=version), self.assertRaisesRegex(ValueError, "unsupported answer"):
                    qerror.load(path)

    def test_run_records_error_stops_own_server_and_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            queries = root / "queries"
            queries.mkdir()
            (queries / "q.rq").write_text("CONSTRUCT {} WHERE {}")
            args = SimpleNamespace(runs=1, disable="", threads=0, out=str(root / "run.jsonl"), sparkles="unused", loc="unused", port=1, timeout=1, queries=queries, only="")
            server = mock.Mock()
            server.poll.return_value = None
            with mock.patch.object(qerror.subprocess, "Popen", return_value=server), mock.patch.object(qerror.subprocess, "run"), mock.patch.object(qerror.urllib.request, "urlopen"), mock.patch.object(qerror, "post", return_value={"error": "failed"}), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as stopped:
                    qerror.run(args)
            self.assertNotIn(stopped.exception.code, (None, 0))
            server.send_signal.assert_called_once()
            server.wait.assert_called_once_with(timeout=60)
            value = json.loads(Path(args.out).read_text())
            self.assertIn("error", value)
            self.assertNotIn("digest", value)

    def test_successful_run_records_format_and_actual_graph_count(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            queries = root / "queries"
            queries.mkdir()
            (queries / "q.rq").write_text("CONSTRUCT {} WHERE {}")
            args = SimpleNamespace(runs=1, disable="", threads=0, out=str(root / "run.jsonl"), sparkles="unused", loc="unused", port=1, timeout=1, queries=queries, only="")
            server = mock.Mock()
            server.poll.return_value = None
            with mock.patch.object(qerror.subprocess, "Popen", return_value=server), mock.patch.object(qerror.subprocess, "run"), mock.patch.object(qerror.urllib.request, "urlopen"), mock.patch.object(qerror, "post", return_value=graph("urn:o")), contextlib.redirect_stderr(io.StringIO()):
                qerror.run(args)
            value = json.loads(Path(args.out).read_text())
            self.assertEqual(value["digestFormat"], 1)
            self.assertEqual(value["queryType"], "CONSTRUCT")
            self.assertEqual(value["rows"], 1)
            self.assertTrue(qerror.answers_equal(value, value))


if __name__ == "__main__":
    unittest.main()
