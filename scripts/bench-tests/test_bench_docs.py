"""Evidence and publication safeguards for the benchmark documentation renderer."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("bench_docs", SCRIPTS / "bench-docs.py")
DOCS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(DOCS)


class BenchmarkDocumentationTests(unittest.TestCase):
    def setUp(self):
        directory = DOCS.ROOT / DOCS.DOCS / "synthetic"
        self.selection = DOCS.read_json(directory / "report.json")["records"][0]
        _, self.record = DOCS.source(DOCS.ROOT, directory, self.selection)

    def render_fixture(self, records, selections=None):
        """Render complete selections without changing the retained repository evidence."""
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        directory = root / DOCS.DOCS / "synthetic"
        directory.mkdir(parents=True)
        selections = copy.deepcopy(selections or [self.selection] * len(records))
        for index, (record, selection) in enumerate(zip(records, selections)):
            raw = json.dumps(record).encode()
            name = f"record-{index}.json"
            (directory / name).write_bytes(raw)
            digest = hashlib.sha256(raw).hexdigest()
            selection.update(path=name, sha256=digest, run_id="sha256-" + digest)
        (directory / "report.json").write_text(json.dumps({"report_version": 1, "records": selections}))
        return root, DOCS.render(root)

    def test_duplicate_chart_names_are_refused_before_rendering(self):
        record = copy.deepcopy(self.record)
        record["scale"] = "different-scale"
        selections = [self.selection, dict(self.selection, scale=record["scale"])]
        with mock.patch.object(DOCS, "chart", wraps=DOCS.chart) as renderer:
            with self.assertRaisesRegex(ValueError, "duplicate chart output"):
                self.render_fixture([self.record, record], selections)
            renderer.assert_not_called()

    def test_untimed_answer_failure_is_preserved_beside_warm_timing(self):
        record = copy.deepcopy(self.record)
        warm = next(row for row in record["results"] if row["mode"] == "warm")
        failed = dict(warm, engine="sparkles" if warm["engine"] != "sparkles" else "jena",
                      mode="answer", status="error", answer=None, samples_ms=[], median_ms=None,
                      reused=False)
        record["results"] = [warm, failed]
        rows = DOCS.prepare_record(record, self.selection)
        self.assertIsNotNone(rows[warm["query"]][warm["engine"]][1][0])
        self.assertEqual(rows[warm["query"]][failed["engine"]][1],
                         (None, "error (answer check; untimed)"))

    def test_all_answer_only_query_remains_visible_without_numeric_latencies(self):
        record = copy.deepcopy(self.record)
        warm = next(row for row in record["results"] if row["mode"] == "warm")
        record["results"] = [dict(warm, mode="answer", status="error", answer=None,
                                   samples_ms=[], median_ms=None, reused=False),
                             dict(warm, mode="answer", engine="sparkles", reused=False)]
        # A successful answer-only row must not publish even a stray numeric field as warm timing.
        rows = DOCS.prepare_record(record, self.selection)
        self.assertEqual(rows[warm["query"]][warm["engine"]][1],
                         (None, "error (answer check; untimed)"))
        self.assertEqual(rows[warm["query"]]["sparkles"][1], (None, "answer checked; not timed"))

    def test_answer_only_row_does_not_replace_a_warm_measurement(self):
        record = copy.deepcopy(self.record)
        warm = next(row for row in record["results"] if row["mode"] == "warm")
        answer = dict(warm, mode="answer", status="error", answer=None, samples_ms=[], median_ms=None)
        for results in ([warm, answer], [answer, warm]):
            record["results"] = results
            rows = DOCS.prepare_record(record, self.selection)
            self.assertIsNotNone(rows[warm["query"]][warm["engine"]][1][0])

    def test_all_excluded_latencies_render_failure_table_and_empty_chart(self):
        for mode, status, answer, expected in [("warm", "timeout", None, "timeout"),
                                                ("warm", "ok", None, "answer unqualified"),
                                                ("answer", "error", None, "error (answer check; untimed)")]:
            with self.subTest(mode=mode, status=status):
                record = copy.deepcopy(self.record)
                warm = next(row for row in record["results"] if row["mode"] == "warm")
                record["results"] = [dict(warm, mode=mode, status=status, answer=answer,
                                           samples_ms=[], median_ms=None)]
                root, outputs = self.render_fixture([record, self.record],
                    [self.selection, dict(self.selection, chart="valid-selection.svg")])
                report = outputs[root / DOCS.DOCS / "synthetic/RECORDS.md"]
                self.assertIn(f'| {warm["query"]} |', report)
                self.assertIn(expected, report)
                self.assertEqual(report.count("Raw record:"), 2)
                svg = outputs[root / DOCS.DOCS / "synthetic/figures" / self.selection["chart"]]
                self.assertIn("No qualified warm latencies", svg)
                valid_svg = outputs[root / DOCS.DOCS / "synthetic/figures/valid-selection.svg"]
                self.assertIn("Measured medians in ms", valid_svg)

    def test_duplicate_rows_in_either_mode_are_refused(self):
        for mode in ("warm", "answer"):
            with self.subTest(mode=mode):
                record = copy.deepcopy(self.record)
                row = dict(next(row for row in record["results"] if row["mode"] == "warm"), mode=mode)
                record["results"] = [row, row]
                with self.assertRaisesRegex(ValueError, "duplicate query/engine"):
                    DOCS.prepare_record(record, self.selection)

    def test_checked_in_outputs_and_evidence_are_current(self):
        result = subprocess.run([sys.executable, str(SCRIPTS / "bench-docs.py"), "--check"],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_legacy_bookmarks_reach_preserved_anchors(self):
        navigation = DOCS.read_json(DOCS.ROOT / DOCS.DOCS / "anchors.json")
        legacy = (DOCS.ROOT / "docs/BENCHMARKS.md").read_text()
        for anchor, target in navigation["anchors"].items():
            with self.subTest(anchor=anchor):
                self.assertIn(f'<a id="{anchor}"></a>', legacy)
                body = (DOCS.ROOT / target["path"]).read_text()
                ids = set(re.findall(r'<a id="([^"]+)"', body))
                counts = {}
                for title in re.findall(r'^#+ (.*)$', body, re.MULTILINE):
                    slug = re.sub(r'[^\w\- ]', '', title.replace('`', '').lower()).replace(' ', '-')
                    count = counts.get(slug, 0)
                    counts[slug] = count + 1
                    ids.add(slug + (f'-{count}' if count else ''))
                self.assertIn(target["anchor"], ids)

    def test_source_content_mutation_is_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / DOCS.DOCS / "synthetic"
            directory.mkdir(parents=True)
            path = directory / "record.json"
            path.write_text('{"changed":true}')
            selection = dict(self.selection, path="record.json")
            with self.assertRaisesRegex(ValueError, "hash mismatch"):
                DOCS.source(root, directory, selection)

    def test_source_outside_benchmark_tree_is_refused(self):
        directory = DOCS.ROOT / DOCS.DOCS / "synthetic"
        with self.assertRaisesRegex(ValueError, "escapes benchmark tree"):
            DOCS.source(DOCS.ROOT, directory, dict(self.selection, path="../../../BENCHMARKS.md"))

    def test_catalog_cannot_publish_metadata_absent_from_source(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / DOCS.DOCS / "synthetic"
            directory.mkdir(parents=True)
            original = DOCS.ROOT / DOCS.DOCS / "synthetic" / self.selection["path"]
            (directory / "record.json").write_bytes(original.read_bytes())
            context = DOCS.context_of(self.record)
            context["suite"] = "fabricated-suite"
            entry = dict(self.selection, path="record.json", context=context,
                         format="bench-record-" + str(self.record["record"]))
            (directory / "RUNS.json").write_text(json.dumps({"catalog_version": 1, "sources": [entry]}))
            with self.assertRaisesRegex(ValueError, "context does not match source"):
                DOCS.verify_catalogs(root)

    def test_write_contract_cannot_be_mislabeled_as_warm_query(self):
        with self.assertRaisesRegex(ValueError, "only single-client warm"):
            DOCS.prepare_record(self.record, dict(self.selection, mode="update"))

    def test_different_source_contracts_are_refused(self):
        for key, value in [("host", "other-host"), ("binary_commit", "another-build"),
                           ("suite", "other-suite"), ("scale", "other-scale")]:
            with self.subTest(key=key), self.assertRaisesRegex(ValueError, "does not match"):
                DOCS.prepare_record(self.record, dict(self.selection, **{key: value}))
        selected = copy.deepcopy(self.selection)
        selected["settings"]["timer"] = "complete-http"
        with self.assertRaisesRegex(ValueError, "setting does not match"):
            DOCS.prepare_record(self.record, selected)

    def test_unknown_record_version_is_refused(self):
        with self.assertRaisesRegex(ValueError, "unknown common record"):
            DOCS.prepare_record(dict(self.record, record=99), self.selection)

    def test_failed_or_unqualified_answer_is_excluded(self):
        for status, answer, expected in [("timeout", None, "timeout"),
                                         ("answer-disagreement", None, "answer-disagreement"),
                                         ("ok", None, "answer unqualified"),
                                         ("ok", {"agrees": False}, "answer unqualified"),
                                         ("ok", {"agrees": True}, "answer evidence missing")]:
            with self.subTest(status=status, answer=answer):
                record = copy.deepcopy(self.record)
                row = next(r for r in record["results"] if r["mode"] == "warm")
                row.update(status=status, answer=answer)
                rows = DOCS.prepare_record(record, self.selection)
                self.assertEqual(rows[row["query"]][row["engine"]][1], (None, expected))

    def test_conflicting_agreement_claims_are_refused(self):
        record = copy.deepcopy(self.record)
        row = next(r for r in record["results"] if r["mode"] == "warm")
        row["answer"]["hash"] = "different-fingerprint"
        with self.assertRaisesRegex(ValueError, "conflicting qualified answers"):
            DOCS.prepare_record(record, self.selection)

    def test_recorded_median_must_match_samples(self):
        record = copy.deepcopy(self.record)
        row = next(r for r in record["results"] if r["mode"] == "warm")
        row["median_ms"] *= 2
        with self.assertRaisesRegex(ValueError, "median disagrees"):
            DOCS.prepare_record(record, self.selection)

    def test_reused_binary_identity_must_match(self):
        record = copy.deepcopy(self.record)
        record["manifest"]["reused"]["jena"]["engine"]["sha256"] = "different-binary"
        with self.assertRaisesRegex(ValueError, "carried-over binary mismatch"):
            DOCS.prepare_record(record, self.selection)


if __name__ == "__main__":
    unittest.main()
