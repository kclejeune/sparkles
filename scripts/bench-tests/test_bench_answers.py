"""Tests of scripts/bench-answers.py: the canonical forms of answers and the request deadline.

    python3 -m unittest discover -s scripts/bench-tests
"""

import http.server
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest

SCRIPTS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PATH = os.path.join(SCRIPTS, "bench-answers.py")
spec = importlib.util.spec_from_file_location("bench_answers", PATH)
ba = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ba)


def rows(*lines):
    return [tuple(ba.term(t, True) for t in line) for line in lines]


def b(label):
    return {"type": "bnode", "value": label}


def u(v):
    return {"type": "uri", "value": v}


class CanonTests(unittest.TestCase):
    def test_blank_node_labels_do_not_matter(self):
        one = rows([b("x1"), u("p"), u("a")], [b("x1"), u("q"), u("b")], [b("x2"), u("p"), u("a")])
        two = rows([b("zz"), u("p"), u("a")], [b("zz"), u("q"), u("b")], [b("yy"), u("p"), u("a")])
        self.assertEqual(ba.canon(one), ba.canon(two))

    def test_different_blank_node_structure_differs(self):
        # one node with both properties against two nodes with one property each
        one = rows([b("x"), u("p"), u("a")], [b("x"), u("q"), u("b")])
        two = rows([b("x"), u("p"), u("a")], [b("y"), u("q"), u("b")])
        self.assertNotEqual(ba.canon(one), ba.canon(two))

    def test_blank_nodes_are_not_all_the_same_value(self):
        # the old form mapped every blank node to one value
        one = rows([b("x"), b("x")])
        two = rows([b("x"), b("y")])
        self.assertNotEqual(ba.canon(one), ba.canon(two))

    def test_blank_nodes_inside_triple_terms_are_canonicalized(self):
        def tt(label):
            return {"type": "triple", "value": {"subject": b(label), "predicate": u("p"), "object": u("o")}}

        self.assertEqual(ba.canon(rows([tt("a")])), ba.canon(rows([tt("b")])))
        shared = rows([tt("a")], [tt("a")])
        apart = rows([tt("a")], [tt("b")])
        self.assertNotEqual(ba.canon(shared), ba.canon(apart))

    def test_without_blank_nodes_the_digest_is_the_plain_one(self):
        r = rows([u("a"), u("b")], [u("c"), u("d")])
        self.assertEqual(ba.canon(r), ba.digest(r))


class NTriplesTests(unittest.TestCase):
    def test_duplicates_spacing_and_escapes_are_not_part_of_the_graph(self):
        one = ("<http://x/s> <http://x/p> \"caf\\u00e9\" .\n<http://x/s> <http://x/p> \"caf\\u00e9\" .\n"
               "<http://x/s>   <http://x/p>  \"x\"^^<http://www.w3.org/2001/XMLSchema#string>   .\n")
        two = "<http://x/s> <http://x/p> \"café\" .\n<http://x/s> <http://x/p> \"x\" .\n"
        self.assertEqual(ba.canon(ba.nt_triples(one, True)), ba.canon(ba.nt_triples(two, True)))
        self.assertEqual(len(ba.nt_triples(one, True)), 2)

    def test_a_blank_node_label_inside_a_literal_is_left_alone(self):
        one = ba.nt_triples('<http://x/s> <http://x/p> "see _:b1" .\n', False)
        two = ba.nt_triples('<http://x/s> <http://x/p> "see _:b2" .\n', False)
        self.assertNotEqual(ba.canon(one), ba.canon(two))

    def test_value_normalization_does_not_merge_triples(self):
        xsd = "http://www.w3.org/2001/XMLSchema#"
        one = f'<urn:s> <urn:p> "1"^^<{xsd}integer> .\n'
        full = one + f'<urn:s> <urn:p> "1.0"^^<{xsd}decimal> .\n'
        a = ba.graph_record(full, False, False)
        d = ba.graph_record(one, False, False)
        self.assertEqual(a["rows"], 2)
        self.assertEqual(d["rows"], 1)
        self.assertNotEqual(a["value"], d["value"])
        self.assertNotEqual(a["exact"], d["exact"])

    def test_graph_record_modes(self):
        text = '<urn:s> <urn:p> "1" .\n<urn:s> <urn:p> "1" .\n'
        self.assertEqual(ba.graph_record(text, True, False), {"rows": 1})
        r = ba.graph_record(text, False, True)
        self.assertEqual((r["rows"], r["exact"], r["value"]), (1, "unchecked", "unchecked"))

    def test_construct_blank_nodes_by_structure(self):
        one = ba.nt_triples("_:a <http://x/p> _:b .\n_:b <http://x/q> \"1\" .\n", False)
        two = ba.nt_triples("_:m <http://x/p> _:n .\n_:n <http://x/q> \"1\" .\n", False)
        self.assertEqual(ba.canon(one), ba.canon(two))


class OrderTests(unittest.TestCase):
    def test_plain_variable_keys(self):
        q = "SELECT ?a ?b WHERE { ?a ?p ?b } ORDER BY DESC(?b) ?a LIMIT 5"
        self.assertEqual(ba.order_keys(q, ["a", "b"]), ["b", "a"])

    def test_expressions_and_unprojected_keys_are_not_checked(self):
        self.assertIsNone(ba.order_keys("SELECT ?a { ?a ?p ?b } ORDER BY DESC(ABS(?a))", ["a"]))
        self.assertIsNone(ba.order_keys("SELECT ?a { ?a ?p ?b } ORDER BY ?b", ["a"]))

    def test_an_order_by_inside_a_subquery_does_not_count(self):
        q = "SELECT ?a { { SELECT ?a { ?a ?p ?b } ORDER BY ?a LIMIT 3 } }"
        self.assertIsNone(ba.order_keys(q, ["a"]))

    def test_no_order_by(self):
        self.assertIsNone(ba.order_keys("SELECT * { ?s ?p ?o }", ["s", "p", "o"]))


def result(vars_, *rows):
    """A SPARQL JSON result with one binding per row of (variable-ordered) term dicts."""
    return {"head": {"vars": list(vars_)},
            "results": {"bindings": [dict(zip(vars_, r)) for r in rows]}}


def lit(v, dt=None, lang=None):
    t = {"type": "literal", "value": v}
    if dt:
        t["datatype"] = ba.XSD + dt
    if lang:
        t["xml:lang"] = lang
    return t


class OrderableTests(unittest.TestCase):
    def test_iri_keys_get_an_order_digest(self):
        res = result(["x"], [u("urn:b")], [u("urn:a")])
        self.assertIn("order", ba.select_record("SELECT ?x { ?s ?p ?x } ORDER BY ?x", res, False))

    def test_language_tagged_keys_do_not(self):
        res = result(["x"], [lit("a", lang="en")], [lit("b", lang="en")])
        self.assertNotIn("order", ba.select_record("SELECT ?x { ?s ?p ?x } ORDER BY ?x", res, False))

    def test_mixed_literal_classes_do_not(self):
        res = result(["x"], [lit("1", "integer")], [lit("a")])
        self.assertNotIn("order", ba.select_record("SELECT ?x { ?s ?p ?x } ORDER BY ?x", res, False))

    def test_unknown_datatypes_and_blank_nodes_do_not(self):
        q = "SELECT ?x { ?s ?p ?x } ORDER BY ?x"
        self.assertNotIn("order", ba.select_record(q, result(["x"], [lit("a", "anyURI")]), False))
        self.assertNotIn("order", ba.select_record(q, result(["x"], [b("n")]), False))

    def test_iris_mix_with_one_literal_class_and_unbound(self):
        res = result(["x"], [u("urn:a")], [lit("a")], [lit("b", "string")])
        self.assertIn("order", ba.select_record("SELECT ?x { ?s ?p ?x } ORDER BY ?x", res, False))
        res = result(["x", "y"], [u("urn:a"), lit("2024-01-01", "date")], [lit("b"), lit("x")])
        self.assertNotIn("order", ba.select_record("SELECT ?x ?y { ?s ?p ?x } ORDER BY ?x ?y", res, False))
        res = {"head": {"vars": ["x"]}, "results": {"bindings": [{}, {"x": u("urn:a")}]}}
        self.assertIn("order", ba.select_record("SELECT ?x { ?s ?p ?x } ORDER BY ?x", res, False))


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(n)
        if self.path == "/slow":
            # a body that keeps arriving, so no single wait is long
            self.send_response(200)
            self.send_header("Content-Type", "application/sparql-results+json")
            self.end_headers()
            try:
                for _ in range(200):
                    self.wfile.write(b" ")
                    self.wfile.flush()
                    time.sleep(0.05)
            except OSError:
                pass
            return
        body = json.dumps({"head": {"vars": ["x"]}, "results": {"bindings": [
            {"x": {"type": "literal", "value": "2"}}, {"x": {"type": "literal", "value": "1"}}]}}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/sparql-results+json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


class RequestTests(unittest.TestCase):
    def setUp(self):
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def answer(self, path, query, max_time="5"):
        qf = os.path.join(self.tmp.name, "q.rq")
        with open(qf, "w") as f:
            f.write(query)
        out = os.path.join(self.tmp.name, "answers.json")
        port = self.server.server_address[1]
        t0 = time.monotonic()
        subprocess.run([sys.executable, PATH, f"127.0.0.1:{port}{path}", qf, out, "q", "e"],
                       env=dict(os.environ, MAX_TIME=max_time), capture_output=True, text=True)
        with open(out) as f:
            return json.load(f)["q"]["e"], time.monotonic() - t0

    def test_the_whole_request_has_a_deadline(self):
        rec, took = self.answer("/slow", "SELECT ?x { ?s ?p ?x }", max_time="1")
        self.assertEqual(rec["rows"], "error")
        self.assertIn("no complete answer", rec["error"])
        self.assertLess(took, 5)

    def test_order_digest_follows_the_returned_sequence(self):
        up, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x } ORDER BY ?x")
        down, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x } ORDER BY DESC(?x)")
        self.assertIn("order", up)
        # the stub returns 2 then 1 whatever the query says, so the two sequences are the same
        self.assertEqual(up["order"], down["order"])
        self.assertEqual(up["value"], down["value"])
        none, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x }")
        self.assertNotIn("order", none)


if __name__ == "__main__":
    unittest.main()
