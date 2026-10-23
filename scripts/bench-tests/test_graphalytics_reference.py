"""Offline reference-harness controls. These tests do not execute graph algorithms."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/graphalytics-reference.py"
SPEC = importlib.util.spec_from_file_location("graphalytics_reference", SCRIPT)
REF = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REF)


def bindings(values, algorithm):
    return {"head": {"vars": ["node", "value"]}, "results": {"bindings": [
        {"node": {"type": "uri", "value": REF.VERTEX + str(node)},
         "value": ({"type": "literal", "value": "inf"}
                   if algorithm == "bfs" and value == REF.INFINITY else
                   {"type": "literal", "value": str(value),
                    "datatype": REF.XSD + ("double" if algorithm == "pr" else "integer")})}
        for node, value in values.items()]}}


class GraphalyticsReferenceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="graphalytics reference ")
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.fixtures = self.work / "fixtures"
        shutil.copytree(ROOT / "testsuite/graphalytics", self.fixtures)
        self.manifest = self.fixtures / "manifest.json"
        self.data = json.loads(self.manifest.read_text())

    def save(self):
        self.manifest.write_text(json.dumps(self.data))

    def change_file(self, key, text, algorithm=None, case_index=0):
        case = self.data["cases"][case_index]
        spec = case[key][algorithm] if algorithm else case[key]
        raw = text.encode("ascii")
        (self.fixtures / spec["path"]).write_bytes(raw)
        spec["sha256"] = hashlib.sha256(raw).hexdigest()
        self.save()

    def load(self, name="example-directed"):
        return REF.load_case(self.manifest, name)

    def actual(self, document, algorithm):
        path = self.work / "actual.json"
        path.write_text(json.dumps(document))
        return REF.actual_rows(path, algorithm)

    def test_pinned_examples_and_reference_comparison(self):
        for name, vertices, edges in [("example-directed", 10, 17), ("example-undirected", 9, 12)]:
            with self.subTest(name=name):
                _, _, nodes, links, expected = self.load(name)
                self.assertEqual((len(nodes), len(links)), (vertices, edges))
                for algorithm, values in expected.items():
                    actual = self.actual(bindings(values, algorithm), algorithm)
                    REF.compare(values, actual, algorithm)
        self.assertEqual(self.load()[4]["bfs"][2], REF.INFINITY)
        self.assertEqual(self.load()[4]["bfs"][10], 2)

    def test_fixture_provenance_and_unmodified_bytes(self):
        provenance = json.loads((self.fixtures / "provenance.json").read_text())
        self.assertEqual(provenance["commit"], "7b8bde76cf7aab5e90b25ecd4b38829e2f98b292")
        self.assertEqual(provenance["license"], "Apache-2.0")
        for item in provenance["files"]:
            self.assertEqual(hashlib.sha256((self.fixtures / item["path"]).read_bytes()).hexdigest(), item["sha256"])
        self.assertIn("Apache License", (self.fixtures / "upstream/LICENSE").read_text())

    def test_prepare_keeps_all_vertex_identities_and_explicit_parameters(self):
        out = self.work / "prepared"
        plan = REF.prepare(self.manifest, "example-directed", out)
        rdf = (out / "graph.nt").read_text()
        self.assertEqual(len(rdf.splitlines()), 10 + 17)
        self.assertIn(f"<{REF.VERTEX}10>", rdf)
        self.assertEqual(rdf.count(f"<{REF.VOCAB}Vertex> ."), 10)
        self.assertFalse(plan["engine_executed"])
        self.assertTrue(plan["unweighted"])
        for name in ["bfs", "pr"]:
            text = (out / f"{name}.rq").read_text()
            self.assertIn("algo:graphalytics true", text)
            self.assertIn("algo:direction algo:forward", text)
            self.assertIn("algo:nodeClass ref:Vertex", text)
            self.assertNotIn("algo:weight", text)
        self.assertIn("algo:maxIterations 2", (out / "pr.rq").read_text())
        self.assertIn("algo:dampingFactor 0.85", (out / "pr.rq").read_text())
        self.assertNotIn("graphalytics", (out / "wcc.rq").read_text().split("SERVICE", 1)[1])
        undirected = REF.queries(self.load("example-undirected")[1])
        self.assertIn("algo:direction algo:undirected", undirected["pr"])
        for name, digest in plan["files"].items():
            self.assertEqual(hashlib.sha256((out / name).read_bytes()).hexdigest(), digest)

    def test_prepare_never_overwrites_existing_directory_or_input(self):
        original = self.manifest.read_bytes()
        with self.assertRaises(FileExistsError):
            REF.prepare(self.manifest, "example-directed", self.fixtures)
        self.assertEqual(self.manifest.read_bytes(), original)

    def test_unknown_or_boolean_manifest_version_refused(self):
        for version in [2, True, "1"]:
            with self.subTest(version=version):
                self.data["format"]["version"] = version
                self.save()
                with self.assertRaisesRegex(REF.Invalid, "format/version"):
                    self.load()

    def test_unknown_fields_and_algorithms_refused(self):
        self.data["cases"][0]["expected"]["sssp"] = self.data["cases"][0]["expected"]["bfs"]
        self.save()
        with self.assertRaisesRegex(REF.Invalid, "expected algorithms"):
            self.load()
        self.data["cases"][0]["expected"].pop("sssp")
        self.data["cases"][0]["weighted"] = True
        self.save()
        with self.assertRaisesRegex(REF.Invalid, "unsupported fields"):
            self.load()

    def test_path_traversal_and_symlink_escape_refused(self):
        case = self.data["cases"][0]
        outside_dir = self.work / "outside"
        outside_dir.mkdir()
        outside = outside_dir / "example-directed.v"
        outside.write_text("1\n")
        for value in ["../outside/example-directed.v", "escape/example-directed.v"]:
            with self.subTest(value=value):
                if value.startswith("escape/"):
                    (self.fixtures / "escape").symlink_to(outside_dir, target_is_directory=True)
                case["vertices"]["path"] = value
                self.save()
                with self.assertRaisesRegex(REF.Invalid, "escapes"):
                    self.load()

    def test_corrupt_hash_and_oversized_input_refused(self):
        self.data["cases"][0]["vertices"]["sha256"] = "0" * 64
        self.save()
        with self.assertRaisesRegex(REF.Invalid, "SHA256 mismatch"):
            self.load()
        with patch.object(REF, "MAX_BYTES", 1):
            with self.assertRaisesRegex(REF.Invalid, "over 16 MiB"):
                self.load()

    def test_manifest_parameters_must_match_upstream_configuration(self):
        for key, value in [("bfs_source", 2), ("directed", False), ("edge_columns", 2)]:
            with self.subTest(key=key):
                old = self.data["cases"][0][key]
                self.data["cases"][0][key] = value
                self.save()
                with self.assertRaises(REF.Invalid):
                    self.load()
                self.data["cases"][0][key] = old
        self.data["cases"][0]["pagerank"]["iterations"] = 20
        self.save()
        with self.assertRaisesRegex(REF.Invalid, "properties disagree"):
            self.load()

    def test_invalid_numeric_parameters_refused(self):
        for parameters in [{"iterations": True, "damping": 0.85}, {"iterations": 2, "damping": 1.0},
                           {"iterations": 0, "damping": 0.85}]:
            self.data["cases"][0]["pagerank"] = parameters
            self.save()
            with self.assertRaises(REF.Invalid):
                self.load()

    def test_duplicate_unsorted_and_property_vertices_refused(self):
        for text in ["1\n1\n", "2\n1\n", "1 label\n", "-1\n", "18446744073709551616\n"]:
            with self.subTest(text=text):
                self.change_file("vertices", text)
                with self.assertRaises(REF.Invalid):
                    self.load()

    def test_invalid_edges_refused(self):
        for text in ["1 11 0.5\n", "1 1 0.5\n", "1 3 0.5\n1 3 0.5\n", "3 1 0.5\n1 3 0.5\n",
                     "1 3 NaN\n", "1 3 -1\n", "1 3\n"]:
            with self.subTest(text=text):
                self.change_file("edges", text)
                with self.assertRaises(REF.Invalid):
                    self.load()

    def test_undirected_reverse_edge_and_duplicate_orientation_refused(self):
        for text in ["3 2 0.5\n", "2 3 0.5\n3 2 0.5\n"]:
            self.change_file("edges", text, case_index=1)
            with self.assertRaisesRegex(REF.Invalid, "lower endpoint"):
                self.load("example-undirected")

    def test_isolated_vertex_survives_conversion(self):
        # Add a local synthetic isolate to a temporary copy, not to upstream data.
        self.change_file("vertices", "".join(f"{i}\n" for i in range(1, 11)) + "99\n")
        config = self.fixtures / self.data["cases"][0]["properties"]["path"]
        self.change_file("properties", config.read_text().replace("meta.vertices = 10", "meta.vertices = 11"))
        for algorithm, value in [("bfs", REF.INFINITY), ("wcc", 99), ("pr", 0.1)]:
            path = self.fixtures / self.data["cases"][0]["expected"][algorithm]["path"]
            self.change_file("expected", path.read_text() + f"99 {value}\n", algorithm)
        out = self.work / "isolated"
        plan = REF.prepare(self.manifest, "example-directed", out)
        self.assertEqual(plan["vertices"], 11)
        rdf = (out / "graph.nt").read_text()
        self.assertEqual(rdf.count(f"<{REF.VERTEX}99>"), 1)
        self.assertIn(f"<{REF.VERTEX}99> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{REF.VOCAB}Vertex>", rdf)
        self.assertIn("algo:nodeClass ref:Vertex", (out / "bfs.rq").read_text())

    def test_reference_missing_extra_duplicate_and_wrong_values_refused(self):
        for text in ["1 0\n", "1 0\n1 1\n", "99 0\n", "1 NaN\n", "1 9223372036854775808\n"]:
            with self.subTest(text=text):
                self.change_file("expected", text, "bfs")
                with self.assertRaises(REF.Invalid):
                    self.load()

    def test_actual_vertices_preserve_uint64_above_double_precision(self):
        values = {9007199254740992: 0, 9007199254740993: 1, (1 << 64) - 1: 2}
        actual = self.actual(bindings(values, "bfs"), "bfs")
        self.assertEqual(actual, values)
        REF.compare(values, actual, "bfs")
        collapsed = {9007199254740992: 1, (1 << 64) - 1: 2}
        with self.assertRaisesRegex(REF.Invalid, "vertex set"):
            REF.compare(values, collapsed, "bfs")

    def test_bfs_wrong_depth_and_missing_unreachable_are_not_passes(self):
        expected = self.load()[4]["bfs"]
        for actual in [dict(expected), {k: v for k, v in expected.items() if v != REF.INFINITY}]:
            if len(actual) == len(expected):
                actual[10] = 3
            with self.assertRaises(REF.Invalid):
                REF.compare(expected, actual, "bfs")

    def test_wcc_relabeling_allowed_but_merge_and_split_fail(self):
        expected = {2: 2, 9: 2, 50: 50, 100: 100}
        REF.compare(expected, {2: 42, 9: 42, 50: 3, 100: 99}, "wcc")
        for actual in [{2: 42, 9: 43, 50: 3, 100: 99}, {2: 42, 9: 42, 50: 42, 100: 99}]:
            with self.assertRaises(REF.Invalid):
                REF.compare(expected, actual, "wcc")

    def test_pr_relative_tolerance_has_no_absolute_floor(self):
        REF.compare({1: 1e-20}, {1: 1.00009e-20}, "pr")
        for expected, actual in [({1: 1e-20}, {1: 1.0002e-20}), ({1: 0.0}, {1: 1e-300}),
                                 ({1: 0.2, 2: 0.8}, {1: 0.3, 2: 0.7})]:
            with self.assertRaisesRegex(REF.Invalid, "mismatch"):
                REF.compare(expected, actual, "pr")
        REF.compare({1: 0.0}, {1: 0.0}, "pr")

    def test_actual_missing_extra_duplicate_and_renumbered_vertices_fail(self):
        expected = {1: 0, 10: 1}
        good = bindings(expected, "bfs")
        for edit in [lambda b: b.pop(), lambda b: b.append(copy.deepcopy(b[0])),
                     lambda b: b[1]["node"].update(value=REF.VERTEX + "9")]:
            document = copy.deepcopy(good)
            edit(document["results"]["bindings"])
            with self.assertRaises(REF.Invalid):
                REF.compare(expected, self.actual(document, "bfs"), "bfs")

    def test_actual_rdf_types_and_unknown_iris_refused(self):
        good = bindings({1: 0}, "bfs")
        changes = [("node", {"type": "bnode", "value": "1"}),
                   ("node", {"type": "uri", "value": "https://other/1"}),
                   ("node", {"type": "uri", "value": REF.VERTEX + "01"}),
                   ("value", {"type": "uri", "value": "0"}),
                   ("value", {"type": "literal", "value": "0"}),
                   ("value", {"type": "literal", "value": "0", "datatype": REF.XSD + "double"}),
                   ("value", {"type": "literal", "value": "inf", "datatype": REF.XSD + "integer"})]
        for key, value in changes:
            document = copy.deepcopy(good)
            document["results"]["bindings"][0][key] = value
            with self.assertRaises(REF.Invalid):
                self.actual(document, "bfs")

    def test_pr_nan_infinity_negative_and_wrong_datatype_refused(self):
        for value in ["NaN", "INF", "Infinity", "-0.1", "1e999", "not-a-number", "1_0", " 0.1 "]:
            with self.assertRaises(REF.Invalid):
                self.actual(bindings({1: value}, "pr"), "pr")
        document = bindings({1: 0.1}, "pr")
        document["results"]["bindings"][0]["value"]["datatype"] = REF.XSD + "integer"
        with self.assertRaises(REF.Invalid):
            self.actual(document, "pr")

    def test_duplicate_json_keys_and_non_select_output_refused(self):
        path = self.work / "actual.json"
        path.write_text('{"head":{},"head":{}}')
        with self.assertRaisesRegex(REF.Invalid, "duplicate JSON"):
            REF.actual_rows(path, "bfs")
        for value in [{"boolean": True}, {"head": {"vars": ["node", "node", "value"]},
                                           "results": {"bindings": []}}, {"error": "query failed"}]:
            with self.assertRaises(REF.Invalid):
                self.actual(value, "bfs")

    def test_cli_success_and_nonzero_invalid_comparison(self):
        actual = self.work / "actual.json"
        actual.write_text(json.dumps(bindings(self.load()[4]["bfs"], "bfs")))
        command = [sys.executable, str(SCRIPT), "--manifest", str(self.manifest),
                   "validate", "example-directed", "bfs", str(actual)]
        result = subprocess.run(command, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(json.loads(result.stdout)["engine_execution_verified"])
        actual.write_text(json.dumps(bindings({1: 0}, "bfs")))
        result = subprocess.run(command, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("vertex set", result.stderr)


class GraphalyticsNativeRunnerTests(unittest.TestCase):
    def test_existing_output_directory_is_refused_without_changes(self):
        with tempfile.TemporaryDirectory(prefix="graphalytics native ") as tmp:
            work = Path(tmp)
            binary = work / "sparkles"
            binary.write_text("#!/bin/sh\nexit 1\n")
            binary.chmod(0o755)
            out = work / "out"
            out.mkdir()
            summary = out / "summary.txt"
            summary.write_text("binary earlier sha256 0\n")
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/graphalytics-native.sh"), str(binary), str(out)],
                capture_output=True, text=True, check=False)
            self.assertEqual(result.returncode, 2)
            self.assertIn("already exists", result.stderr)
            self.assertEqual(summary.read_text(), "binary earlier sha256 0\n")
            self.assertEqual(sorted(p.name for p in out.iterdir()), ["summary.txt"])


if __name__ == "__main__":
    unittest.main()
