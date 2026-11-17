"""Check scripts/rl-reference/owl2rl.rls on small graphs with Nemo: lists that do not end
at rdf:nil, literal objects of scm-int, blank nodes as properties and reflexive
owl:sameAs. Check rdfs.rls on generalized intermediates and container membership
properties. NMO names the nmo executable, and without it or nmo on PATH the tests are
skipped."""

import importlib.util
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HERE = ROOT / "scripts" / "rl-reference"
spec = importlib.util.spec_from_file_location("rl_terms", HERE / "rl_terms.py")
rl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rl)

NMO = os.environ.get("NMO") or shutil.which("nmo")
RDF = "http://www.w3.org/1999/02/22-rdf-syntax-ns#"
RDFS = "http://www.w3.org/2000/01/rdf-schema#"
OWL = "http://www.w3.org/2002/07/owl#"


def closure(nt, program="owl2rl.rls"):
    """The triples that the program, by default the W3C OWL 2 RL program, derives from the
    N-Triples text nt."""
    with tempfile.TemporaryDirectory() as d:
        d = Path(d)
        (d / "in.nt").write_text(nt)
        (d / "enc").mkdir()
        rl.encode(str(d / "in.nt"), str(d / "enc"))
        subprocess.run(
            [NMO, "-q", "--overwrite-results", "--import-dir", str(d / "enc"), "--export-dir", str(d / "out"),
             str(HERE / program)],
            check=True, capture_output=True,
        )
        rl.decode(str(d / "enc"), str(d / "out" / "derived.nt"), str(d / "derived.nt"))
        return set((d / "derived.nt").read_text().splitlines())


@unittest.skipUnless(NMO, "set NMO or put nmo on PATH")
class Program(unittest.TestCase):
    def test_a_list_without_rdf_nil_has_no_members(self):
        got = closure(
            f"<urn:C> <{OWL}unionOf> _:l .\n_:l <{RDF}first> <urn:D> .\n<urn:x> <{RDF}type> <urn:D> .\n"
        )
        self.assertNotIn(f"<urn:x> <{RDF}type> <urn:C> .", got)
        got = closure(
            f"<urn:C> <{OWL}unionOf> _:l .\n_:l <{RDF}first> <urn:D> .\n_:l <{RDF}rest> <{RDF}nil> .\n"
            f"<urn:x> <{RDF}type> <urn:D> .\n"
        )
        self.assertIn(f"<urn:x> <{RDF}type> <urn:C> .", got)

    def test_a_literal_member_of_an_intersection_is_a_superclass(self):
        got = closure(f'<urn:C> <{OWL}intersectionOf> _:l .\n_:l <{RDF}first> "v" .\n_:l <{RDF}rest> <{RDF}nil> .\n')
        self.assertIn(f'<urn:C> <{RDFS}subClassOf> "v" .', got)

    def test_no_blank_node_becomes_a_property(self):
        got = closure(
            f"<urn:R> <{OWL}hasValue> <urn:v> .\n<urn:R> <{OWL}onProperty> _:p .\n<urn:x> <{RDF}type> <urn:R> .\n"
        )
        self.assertFalse([t for t in got if t.startswith("<urn:x> _:")], got)

    def test_no_reflexive_same_as(self):
        got = closure(
            f"<{OWL}sameAs> <{RDF}type> <{OWL}TransitiveProperty> .\n<urn:a> <{OWL}sameAs> <urn:b> .\n"
        )
        self.assertIn(f"<urn:b> <{OWL}sameAs> <urn:a> .", got)
        self.assertNotIn(f"<urn:a> <{OWL}sameAs> <urn:a> .", got)
        self.assertNotIn(f"<urn:b> <{OWL}sameAs> <urn:b> .", got)


@unittest.skipUnless(NMO, "set NMO or put nmo on PATH")
class Rdfs(unittest.TestCase):
    def test_a_blank_node_superproperty_carries_its_domain(self):
        # (urn:s _:b urn:o) is generalized and not written, but rdfs2 types urn:s through it
        got = closure(
            f"<urn:p> <{RDFS}subPropertyOf> _:b .\n_:b <{RDFS}domain> <urn:C> .\n<urn:s> <urn:p> <urn:o> .\n",
            "rdfs.rls",
        )
        self.assertIn(f"<urn:s> <{RDF}type> <urn:C> .", got)
        self.assertFalse([t for t in got if t.split(" ")[1].startswith("_:")], got)

    def test_no_literal_becomes_a_subject(self):
        got = closure(
            f'<urn:p> <{RDFS}range> <urn:C> .\n<urn:C> <{RDFS}subClassOf> <urn:D> .\n<urn:s> <urn:p> "v" .\n',
            "rdfs.rls",
        )
        self.assertIn(f"<urn:C> <{RDF}type> <{RDFS}Class> .", got)
        self.assertFalse([t for t in got if t.startswith('"')], got)

    def test_container_membership_properties(self):
        got = closure(f"<urn:s> <{RDF}_7> <urn:o> .\n<urn:s> <{RDF}_07> <urn:o> .\n", "rdfs.rls")
        self.assertIn(f"<{RDF}_7> <{RDF}type> <{RDFS}ContainerMembershipProperty> .", got)
        self.assertIn(f"<urn:s> <{RDFS}member> <urn:o> .", got)
        self.assertNotIn(f"<{RDF}_07> <{RDF}type> <{RDFS}ContainerMembershipProperty> .", got)


if __name__ == "__main__":
    unittest.main()
