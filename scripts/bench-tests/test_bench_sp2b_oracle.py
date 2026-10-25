"""Independent SP2Bench oracle regressions for DISTINCT pairs and earliest-author years."""

import importlib.util
import pathlib
import tempfile
import unittest

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "bench-sp2b/oracle.py"
SPEC = importlib.util.spec_from_file_location("sp2b_oracle", SCRIPT)
oracle = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(oracle)


class SP2BOracleTests(unittest.TestCase):
    def tables(self, triples):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "source.nt"
            path.write_text("\n".join(f"{s} {p} {o} ." for s, p, o in triples) + "\n")
            return oracle.source_tables(path)

    def test_q4_distinct_names_across_shared_journals(self):
        triples = [(doc, oracle.RDF_TYPE, oracle.ARTICLE) for doc in ("<urn:a>", "<urn:b>", "<urn:c>")]
        triples += [("<urn:a>", oracle.CREATOR, "_:alice"), ("<urn:b>", oracle.CREATOR, "_:bob"),
                    ("<urn:c>", oracle.CREATOR, "_:another-alice"),
                    ("_:alice", oracle.NAME, '"Alice"'), ("_:bob", oracle.NAME, '"Bob"'),
                    ("_:another-alice", oracle.NAME, '"Alice"')]
        triples += [(doc, oracle.JOURNAL, journal) for doc in ("<urn:a>", "<urn:b>", "<urn:c>") for journal in ("<urn:j1>", "<urn:j2>")]
        tables, names, _ = self.tables(triples + triples[:2])
        self.assertEqual(list(oracle.q4_rows(tables, names)), [
            (("lit", "Alice", oracle.XSD + "string"), ("lit", "Bob", oracle.XSD + "string"))])

    def test_q6_checks_all_author_years_and_preserves_outer_join_multiplicity(self):
        integer = lambda year: f'"{year}"^^<{oracle.XSD}integer>'
        triples = [("<urn:class1>", oracle.SUBCLASS, oracle.DOCUMENT), ("<urn:class2>", oracle.SUBCLASS, oracle.DOCUMENT),
                   ("<urn:old>", oracle.RDF_TYPE, "<urn:class1>"), ("<urn:new>", oracle.RDF_TYPE, "<urn:class1>"),
                   ("<urn:new>", oracle.RDF_TYPE, "<urn:class2>"),
                   ("<urn:old>", oracle.ISSUED, integer(1999)), ("<urn:new>", oracle.ISSUED, integer(2000)),
                   ("<urn:old>", oracle.CREATOR, "_:alice"), ("<urn:new>", oracle.CREATOR, "_:alice"),
                   ("<urn:new>", oracle.CREATOR, "_:bob"), ("<urn:new>", oracle.CREATOR, "_:bob2"),
                   ("_:alice", oracle.NAME, '"Alice"'), ("_:bob", oracle.NAME, '"Bob"'), ("_:bob2", oracle.NAME, '"Bob"')]
        tables, names, years = self.tables(triples)
        rows = list(oracle.q6_rows(tables, names, years))
        self.assertEqual(len(rows), 5)
        self.assertEqual(sum(row[1][1] == "Alice" for row in rows), 1)
        self.assertEqual(sum(row[1][1] == "Bob" for row in rows), 4)
        self.assertTrue(all(row[0] == ("uri", "urn:new") for row in rows if row[1][1] == "Bob"))

    def test_source_assumptions_fail_explicitly(self):
        with self.assertRaisesRegex(ValueError, "xsd:string"):
            self.tables([("_:a", oracle.NAME, '"Name"@en')])
        with self.assertRaisesRegex(ValueError, "xsd:integer"):
            self.tables([("<urn:a>", oracle.ISSUED, '"2000"')])
