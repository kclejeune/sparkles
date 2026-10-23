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


GROUP_AVG = ("SELECT ?o (AVG(?a) AS ?avg) (COUNT(?p) AS ?n) WHERE { ?p ex:worksFor ?o ; foaf:age ?a } "
             "GROUP BY ?o ORDER BY DESC(?n) ?o LIMIT 10")


class Probe:
    """A fake `run` over the full sorted solution sequence of a query: a list of rows of term dicts.
    It slices that sequence by the LIMIT and OFFSET of the probe text and records the texts."""

    def __init__(self, vars_, full):
        self.vars, self.full, self.texts = vars_, full, []

    def __call__(self, text):
        self.texts.append(text)
        n, o = ba.slice_of(text)
        rows = self.full[o:] if n is None else self.full[o:o + n]
        return result(self.vars, *rows)


def pair_rows(*pairs):
    return [[lit(str(x), "integer"), lit(y)] for x, y in pairs]


def pair_probe(*pairs):
    return Probe(["x", "y"], pair_rows(*pairs))


class SliceTests(unittest.TestCase):
    Q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x LIMIT 2"

    def rec(self, query, pairs, run=None):
        res = result(["x", "y"], *pair_rows(*pairs))
        return ba.select_record(query, res, False, run)

    def test_rows_tied_at_the_limit_boundary_are_compared_by_key_only(self):
        full = [(1, "a"), (2, "b"), (2, "c")]
        a = self.rec(self.Q, [(1, "a"), (2, "b")], pair_probe(*full))
        b = self.rec(self.Q, [(1, "a"), (2, "c")], pair_probe(*full))
        for k in ("exact", "value", "order"):
            self.assertEqual(a[k], b[k], k)
        self.assertEqual(a["boundary_rows"], 1)
        self.assertEqual(a["rows"], 2)

    def test_unique_keys_at_the_limit_leave_nothing_out(self):
        full = [(1, "a"), (2, "b"), (3, "c")]
        run = pair_probe(*full)
        a = self.rec(self.Q, [(1, "a"), (2, "b")], run)
        b = self.rec(self.Q, [(1, "a"), (2, "z")], pair_probe(*full))
        self.assertNotEqual(a["value"], b["value"])
        self.assertNotEqual(a["exact"], b["exact"])
        self.assertEqual(a["boundary_rows"], 0)
        self.assertEqual(len(run.texts), 1)

    def test_group_avg_keeps_its_last_row(self):
        def avg(v):
            return lit(v, "decimal")

        def row(i, a):
            return [u(f"urn:org/{i:02d}"), avg(a), lit(str(20 - i), "integer")]

        head = ["o", "avg", "n"]
        full = [row(i, "30") for i in range(11)]
        recs = []
        for last in ("30", "999"):
            rows = [row(i, "30") for i in range(9)] + [row(9, last)]
            recs.append(ba.select_record(GROUP_AVG, result(head, *rows), False, Probe(head, full)))
        self.assertNotEqual(recs[0]["value"], recs[1]["value"])
        self.assertNotEqual(recs[0]["exact"], recs[1]["exact"])
        self.assertEqual(recs[0]["boundary_rows"], 0)

    def test_offset_zero_is_no_cut(self):
        q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x OFFSET 0 LIMIT 5"
        run = pair_probe((1, "a"))
        r = self.rec(q, [(1, "a")], run)
        self.assertEqual(r["boundary_rows"], 0)
        self.assertEqual(r["exact"], self.rec("SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x", [(1, "a")])["exact"])
        self.assertEqual(run.texts, [])

    def test_a_row_away_from_the_boundary_still_counts(self):
        full = [(1, "a"), (2, "b"), (2, "c")]
        a = self.rec(self.Q, [(1, "z"), (2, "b")], pair_probe(*full))
        b = self.rec(self.Q, [(1, "a"), (2, "b")], pair_probe(*full))
        self.assertNotEqual(a["value"], b["value"])

    def test_a_boundary_key_with_another_number_of_rows_differs(self):
        q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x LIMIT 3"
        a = self.rec(q, [(1, "a"), (2, "b"), (2, "c")])
        b = self.rec(q, [(1, "a"), (2, "b"), (3, "c")])
        self.assertNotEqual(a["order"], b["order"])

    def test_fewer_rows_than_the_limit_leave_nothing_out(self):
        run = pair_probe((1, "a"))
        r = self.rec(self.Q, [(1, "a")], run)
        self.assertEqual(r["boundary_rows"], 0)
        self.assertEqual(r["exact"], self.rec("SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x", [(1, "a")])["exact"])
        self.assertEqual(run.texts, [])

    def test_the_data_ending_at_the_cut_is_no_tie(self):
        run = pair_probe((1, "a"), (2, "b"))
        r = self.rec(self.Q, [(1, "a"), (2, "b")], run)
        self.assertEqual(r["boundary_rows"], 0)
        self.assertEqual(len(run.texts), 1)

    def test_with_an_offset_the_rows_tied_with_the_first_key_are_left_out(self):
        q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x OFFSET 1 LIMIT 5"
        full = [(1, "p"), (1, "a"), (1, "b"), (2, "c")]
        a = self.rec(q, [(1, "a"), (1, "b"), (2, "c")], pair_probe(*full))
        b = self.rec(q, [(1, "x"), (1, "y"), (2, "c")], pair_probe(*full))
        self.assertEqual(a["boundary_rows"], 2)
        self.assertEqual(a["value"], b["value"])
        self.assertEqual(a["order"], b["order"])

    def test_with_an_offset_and_no_tie_nothing_is_left_out(self):
        q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x OFFSET 1 LIMIT 5"
        full = [(0, "p"), (1, "a"), (1, "b"), (2, "c")]
        a = self.rec(q, [(1, "a"), (1, "b"), (2, "c")], pair_probe(*full))
        b = self.rec(q, [(1, "x"), (1, "b"), (2, "c")], pair_probe(*full))
        self.assertEqual(a["boundary_rows"], 0)
        self.assertNotEqual(a["value"], b["value"])

    def test_an_offset_without_a_limit_probes_two_rows(self):
        q = "SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x OFFSET 1"
        full = [(1, "p"), (1, "a"), (2, "c")]
        run = pair_probe(*full)
        r = self.rec(q, [(1, "a"), (2, "c")], run)
        self.assertEqual(r["boundary_rows"], 1)
        self.assertEqual(len(run.texts), 1)

    def test_both_clauses_in_either_order(self):
        full = [(1, "p"), (1, "a"), (2, "b"), (2, "c")]
        for tail in ("OFFSET 1 LIMIT 2", "LIMIT 2 OFFSET 1"):
            q = f"SELECT ?x ?y {{ ?x ?p ?y }} ORDER BY ?x {tail}"
            r = self.rec(q, [(1, "a"), (2, "b")], pair_probe(*full))
            self.assertEqual(r["boundary_rows"], 2, tail)

    def test_a_failed_probe_leaves_everything_in(self):
        def run(_):
            raise OSError("down")

        r = self.rec(self.Q, [(1, "a"), (2, "b")], run)
        self.assertEqual(r["boundary_probe"], "failed")
        self.assertEqual(r["boundary_rows"], 0)
        self.assertEqual(r["exact"], self.rec("SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x", [(1, "a"), (2, "b")])["exact"])

    def test_a_probe_that_disagrees_with_the_answer_leaves_nothing_out(self):
        r = self.rec(self.Q, [(1, "a"), (2, "b")], pair_probe((1, "a"), (3, "b"), (3, "c")))
        self.assertEqual(r["boundary_rows"], 0)
        self.assertNotIn("boundary_probe", r)

    def test_queries_without_a_slice_or_with_unordered_keys_are_unchanged(self):
        r = self.rec("SELECT ?x ?y { ?x ?p ?y } ORDER BY ?x", [(1, "a"), (2, "b")])
        self.assertNotIn("boundary_rows", r)
        run = pair_probe((1, "a"), (2, "b"))
        r = self.rec("SELECT ?x ?y { ?x ?p ?y } ORDER BY ABS(?x) LIMIT 2", [(1, "a"), (2, "b")], run)
        self.assertNotIn("boundary_rows", r)
        self.assertEqual(run.texts, [])

    def test_without_a_run_nothing_is_left_out(self):
        r = self.rec(self.Q, [(1, "a"), (2, "b")])
        self.assertEqual(r["boundary_rows"], 0)

    def test_slice_of(self):
        self.assertEqual(ba.slice_of("SELECT * { ?s ?p ?o } ORDER BY ?s LIMIT 7"), (7, 0))
        self.assertEqual(ba.slice_of("SELECT * { ?s ?p ?o } ORDER BY ?s OFFSET 3"), (None, 3))
        self.assertEqual(ba.slice_of("SELECT * { ?s ?p ?o } ORDER BY ?s OFFSET 0 LIMIT 3"), (3, 0))
        self.assertEqual(ba.slice_of("SELECT * { ?s ?p ?o } ORDER BY ?s OFFSET 4"), (None, 4))
        self.assertEqual(ba.slice_of("SELECT * { ?s ?p ?o } LIMIT 7"), (None, 0))
        self.assertEqual(ba.slice_of("SELECT * { { SELECT ?s { ?s ?p ?o } ORDER BY ?s LIMIT 7 } }"), (None, 0))


class ProbeQueryTests(unittest.TestCase):
    def test_the_clauses_are_replaced_in_place(self):
        q = "SELECT ?x { ?s ?p ?x } ORDER BY ?x LIMIT 5 OFFSET 2"
        p = ba.probe_query(q, 1, 7)
        self.assertEqual(ba.slice_of(p), (7, 1))
        self.assertTrue(p.startswith("SELECT ?x { ?s ?p ?x } ORDER BY ?x "))
        self.assertEqual(p.count("LIMIT"), 1)

    def test_a_trailing_values_block_stays_after_the_new_clauses(self):
        q = "SELECT ?x { ?s ?p ?x } ORDER BY ?x LIMIT 2 VALUES ?z { 1 }"
        p = ba.probe_query(q, 0, 3)
        self.assertEqual(ba.slice_of(p), (3, 0))
        self.assertLess(p.index("LIMIT 3"), p.index("VALUES"))
        self.assertTrue(p.rstrip().endswith("VALUES ?z { 1 }"))

    def test_the_word_limit_in_an_iri_or_a_string_is_not_a_clause(self):
        q = 'SELECT ?x { ?s <urn:limit 9> "limit 8 offset 3" } ORDER BY ?x LIMIT 2'
        p = ba.probe_query(q, 4, 5)
        self.assertEqual(ba.slice_of(p), (5, 4))
        self.assertIn('"limit 8 offset 3"', p)
        self.assertLess(p.index("ORDER BY"), p.index("LIMIT 5"))

    def test_blanking_keeps_positions(self):
        q = 'SELECT ?x { ?s <urn:a> "b c" } # note\nORDER BY ?x LIMIT 2'
        start, tail = ba._order_by_tail(q)
        self.assertEqual(q[:start].rstrip()[-8:], "ORDER BY")
        self.assertEqual(len(q) - start, len(tail))


class Handler(http.server.BaseHTTPRequestHandler):
    bodies = []

    def log_message(self, *args):
        pass

    def do_POST(self):
        n = int(self.headers.get("Content-Length") or 0)
        sent = self.rfile.read(n).decode()
        Handler.bodies.append((self.path, sent))
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
        if self.path == "/ties":
            # the main query gets two rows with the same key, the probe (LIMIT 3) gets three
            count = 3 if "LIMIT+3" in sent else 2
            vals = ["5"] * count
        elif self.path == "/asc":
            vals = ["1", "2"]
        else:
            vals = ["2", "1"]
        body = json.dumps({"head": {"vars": ["x"]}, "results": {"bindings": [
            {"x": {"type": "literal", "value": v}} for v in vals]}}).encode()
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

    def answer(self, path, query, max_time="5", **env):
        qf = os.path.join(self.tmp.name, "q.rq")
        with open(qf, "w") as f:
            f.write(query)
        out = os.path.join(self.tmp.name, "answers.json")
        port = self.server.server_address[1]
        t0 = time.monotonic()
        subprocess.run([sys.executable, PATH, f"127.0.0.1:{port}{path}", qf, out, "q", "e"],
                       env=dict(os.environ, MAX_TIME=max_time, **env), capture_output=True, text=True)
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

    def test_order_digest_tells_opposite_orders_apart(self):
        # the same rows in opposite orders: the multisets agree and the sequences do not
        down, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x } ORDER BY ?x")
        up, _ = self.answer("/asc", "SELECT ?x { ?s ?p ?x } ORDER BY ?x")
        self.assertNotEqual(up["order"], down["order"])
        self.assertEqual(up["exact"], down["exact"])
        self.assertEqual(up["value"], down["value"])

    def test_the_probe_request_is_wired_through_main(self):
        Handler.bodies.clear()
        rec, _ = self.answer("/ties", "SELECT ?x ?y { ?s ?p ?x } ORDER BY ?x LIMIT 2")
        self.assertEqual(rec["boundary_rows"], 2)
        self.assertEqual(len(Handler.bodies), 2)
        self.assertIn("LIMIT+3", Handler.bodies[1][1])
        self.assertNotIn("boundary_probe", rec)

    def test_raw_responses_are_kept_on_request(self):
        raw = os.path.join(self.tmp.name, "raw")
        plain, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x }")
        self.assertFalse(os.path.exists(raw))
        kept, _ = self.answer("/ok", "SELECT ?x { ?s ?p ?x }", ANSWERS_RAW_DIR=raw)
        self.assertEqual(plain, kept)
        with open(os.path.join(raw, "q.e.json")) as f:
            body = json.load(f)
        self.assertEqual([b["x"]["value"] for b in body["results"]["bindings"]], ["2", "1"])


if __name__ == "__main__":
    unittest.main()
