"""Tests of the synthetic set generator's exclusion of evaluation entities."""
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).parent / "synth"))
import synth  # noqa: E402

R = "http://dbpedia.org/resource/"


class GoldResources(unittest.TestCase):
    def test_full_and_prefixed_names(self):
        g = "SELECT ?x WHERE { <http://dbpedia.org/resource/Berlin> dbo:x ?x . res:Paris dbo:y dbr:Rome . }"
        self.assertEqual(synth.gold_resources(g), {R + "Berlin", R + "Paris", R + "Rome"})

    def test_names_outside_ascii_are_whole(self):
        g = "ASK { dbr:François_Hollande dbo:x dbr:Düsseldorf_Airport }"
        self.assertEqual(synth.gold_resources(g), {R + "François_Hollande", R + "Düsseldorf_Airport"})

    def test_escapes_and_trailing_dot(self):
        g = "SELECT ?x WHERE { dbr:Mercury_\\(planet\\) dbo:x ?x . ?x dbo:y res:St._Louis. }"
        self.assertEqual(synth.gold_resources(g), {R + "Mercury_(planet)", R + "St._Louis"})

    def test_other_prefixes_are_not_resources(self):
        self.assertEqual(synth.gold_resources("SELECT ?x WHERE { ?x dbo:res ?y . ?y xdbr:Z ?z }"), set())


if __name__ == "__main__":
    unittest.main()
