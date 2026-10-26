"""Oracles and client wire behavior required by X09's native indexed gates."""

import csv
import http.server
import importlib.util
import json
import mmap
import os
import pathlib
import subprocess
import tempfile
import threading
import time
import unittest
from types import SimpleNamespace
from unittest import mock

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "indexed_commit", SCRIPTS / "bench-indexed-commit.py"
)
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


def answer(value="17", datatype=BENCH.XSD + "integer"):
    return json.dumps(
        {
            "head": {"vars": ["n"]},
            "results": {
                "bindings": [
                    {"n": {"type": "literal", "datatype": datatype, "value": value}}
                ]
            },
        }
    ).encode()


def index_plan(operator="SpatialScan"):
    return {
        "executed": True,
        "nodes": [
            {
                "id": "0",
                "operator": operator,
                "actualRows": 17,
                "complete": True,
                "partial": False,
            }
        ],
        "plan": {
            "id": "0",
            "operator": operator,
            "children": [],
            "counters": {"index": "ready", "fallback": False},
        },
    }


def rss_bounds(upper, lower=None):
    return {
        "peak_rss_kib": upper,
        "reset_hwm_outer_window_kib": upper,
        "sampled_writer_interval_peak_kib": upper if lower is None else lower,
        "writer_interval_sample_count": 10,
    }


class Oracles(unittest.TestCase):
    def test_owned_flush_counts_files_and_rejects_external_links_and_fifo(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-flush-") as temp:
            parent = pathlib.Path(temp)
            root = parent / "owned"
            (root / "child").mkdir(parents=True)
            (root / "one").write_bytes(b"a")
            (root / "child" / "two").write_bytes(b"bc")
            result = BENCH.flush_owned_tree(root)
            self.assertEqual(
                (result["files"], result["directories"], result["bytes"]), (2, 2, 3)
            )
            external = parent / "external"
            external.mkdir()
            (external / "marker").write_text("preserve")
            unsafe = parent / "unsafe"
            unsafe.mkdir()
            for target in [external, external / "marker"]:
                link = unsafe / "link"
                link.symlink_to(target)
                with mock.patch.object(BENCH.os, "fsync") as sync:
                    with self.assertRaises((OSError, ValueError)):
                        BENCH.flush_owned_tree(unsafe)
                    sync.assert_not_called()
                link.unlink()
            fifo = unsafe / "fifo"
            os.mkfifo(fifo)
            with self.assertRaisesRegex(ValueError, "special file"):
                BENCH.flush_owned_tree(unsafe)
            self.assertEqual((external / "marker").read_text(), "preserve")

    def test_evidence_flush_failure_records_itself_and_preserves_original_failure(self):
        for original in (None, RuntimeError("original measurement failure")):
            with (
                self.subTest(original=original),
                tempfile.TemporaryDirectory(
                    prefix="sparkles-evidence-failure-"
                ) as temp,
            ):
                root = pathlib.Path(temp)

                def measured(*args, original_error=original, **kwargs):
                    directory = args[1] / args[6]
                    (directory / "store").mkdir()
                    if original_error:
                        raise original_error
                    BENCH.save(directory / "result.json", {"reopen": "passed"})
                    return {"reopen": "passed"}

                with (
                    mock.patch.object(BENCH, "_measure", side_effect=measured),
                    mock.patch.object(
                        BENCH,
                        "flush_owned_tree",
                        side_effect=OSError("evidence flush failed"),
                    ),
                    self.assertRaisesRegex(OSError, "evidence flush failed"),
                ):
                    BENCH.measure(None, root, "text", "writers", False, 0, "run", 0)
                directory = root / "run"
                self.assertFalse((directory / "store").exists())
                self.assertTrue((directory / "failure.json").exists())
                failure = json.loads((directory / "failure.json").read_text())
                self.assertEqual(
                    failure["error"],
                    str(original) if original else "evidence flush failed",
                )
                secondary = json.loads(
                    (directory / "evidence-sync-failure.json").read_text()
                )
                self.assertEqual(secondary["error"], "evidence flush failed")
                self.assertEqual(secondary["phase"], "evidence flush")

    def test_clone_flush_failure_prevents_server_start_and_preserves_failure(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-failed-flush-") as temp:
            root = pathlib.Path(temp)
            (root / "seed").mkdir()
            (root / "seed" / "marker").write_text("seed")
            with (
                mock.patch.object(
                    BENCH, "flush_owned_tree", side_effect=[OSError("flush failed"), {}]
                ),
                mock.patch.object(BENCH, "Server") as server,
                self.assertRaisesRegex(OSError, "flush failed"),
            ):
                BENCH.measure(None, root, "text", "writers", True, 0, "run", 0)
            server.assert_not_called()
            self.assertEqual((root / "seed" / "marker").read_text(), "seed")
            self.assertFalse((root / "run" / "store").exists())
            self.assertIn("flush failed", (root / "run" / "failure.json").read_text())

    @unittest.skipUnless(
        pathlib.Path("/proc/self/smaps").exists(), "Linux dirty-page probe"
    )
    def test_fresh_ext4_clone_is_clean_before_server_start(self):
        parent = SCRIPTS.parent / ".artifacts" / "bench-tests"
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="sparkles-clone-fence-", dir=parent
        ) as temp:
            root = pathlib.Path(temp)
            filesystem = subprocess.check_output(
                ["findmnt", "-n", "-o", "FSTYPE", "-T", str(root)], text=True
            ).strip()
            if filesystem != "ext4":
                self.skipTest("regression requires ext4 copy writeback")
            (root / "seed").mkdir()
            source = root / "seed" / "base"
            with source.open("wb") as out:
                out.write(b"x" * (1024 * 1024))
                out.flush()
                os.fsync(out.fileno())

            def server_start(_args, store, _directory, _grouped):
                clone = store / "base"
                with (
                    clone.open("rb") as inp,
                    mmap.mmap(inp.fileno(), 0, access=mmap.ACCESS_READ) as mapped,
                ):
                    sum(mapped[i] for i in range(0, len(mapped), 4096))
                    active, dirty = False, 0
                    for line in (
                        pathlib.Path("/proc/self/smaps").read_text().splitlines()
                    ):
                        if line.endswith(str(clone)):
                            active = True
                        elif active:
                            if "-" in line.split()[0]:
                                break
                            if line.startswith(("Private_Dirty:", "Shared_Dirty:")):
                                dirty += int(line.split()[1])
                    self.assertEqual(
                        dirty, 0, "clone writeback overlaps server startup"
                    )
                raise RuntimeError("clean clone reached server start")

            with (
                mock.patch.object(BENCH, "Server", side_effect=server_start),
                self.assertRaisesRegex(
                    RuntimeError, "clean clone reached server start"
                ),
            ):
                BENCH.measure(None, root, "text", "writers", True, 0, "run", 0)
            self.assertFalse((root / "run" / "store").exists())

    def test_missing_or_regenerated_embed_history_is_not_durable_recovery(self):
        live = [
            {"seq": 2, "kind": "update", "timestamp": "T0"},
            {
                "seq": 3,
                "kind": "embed",
                "timestamp": "T1",
                "message": "embeddings of vector index docs",
            },
        ]
        # Receipt equality alone cannot detect the lost trailing internal commit.
        self.assertEqual(
            BENCH.verify_receipts({"workload": [2], "seeds": []}, live[:1], True), 0
        )
        BENCH.verify_recovered_history(live, live)
        with self.assertRaises(AssertionError):
            BENCH.verify_recovered_history(live, live[:1])
        regenerated = [dict(c) for c in live]
        regenerated[1]["timestamp"] = "T2"
        with self.assertRaises(AssertionError):
            BENCH.verify_recovered_history(live, regenerated)

    def test_existing_run_store_survives_refusal(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-existing-run-") as temp:
            root = pathlib.Path(temp)
            (root / "run" / "store").mkdir(parents=True)
            marker = root / "run" / "store" / "preserve"
            marker.write_text("existing store")
            with self.assertRaises(FileExistsError):
                BENCH.measure(None, root, "text", "writers", True, 0, "run", 0)
            self.assertEqual(marker.read_text(), "existing store")

    def test_acknowledgments_match_exact_recovered_user_history(self):
        receipt = {"workload": [10, 12], "seeds": []}
        history = [
            {"seq": 10, "kind": "update"},
            {"seq": 11, "kind": "embed", "message": "embeddings of vector index docs"},
            {"seq": 12, "kind": "update"},
        ]
        self.assertEqual(BENCH.verify_receipts(receipt, history, True), 1)
        with self.assertRaises(AssertionError):
            BENCH.verify_receipts(receipt, history, False)
        for bad in (
            {"workload": [10, 13], "seeds": []},
            {"workload": [10, 10], "seeds": []},
            {"workload": [12], "seeds": []},
        ):
            with self.assertRaises(AssertionError):
                BENCH.verify_receipts(bad, history, True)
        history[1]["kind"] = "transaction"
        with self.assertRaises(AssertionError):
            BENCH.verify_receipts(receipt, history, True)

    def test_slow_inflight_reader_is_not_censored_at_writer_stop(self):
        samples = [
            {"start": -1, "end": 1},
            {"start": 1, "end": 100},
            {"start": 3, "end": 4},
        ]
        self.assertEqual(BENCH.window_samples(samples, 0, 2), [samples[1]])

    def test_spatial_plan_note_cannot_substitute_for_executed_index(self):
        plan = {
            "executed": True,
            "notes": [{"text": "SpatialScan could have been used"}],
            "nodes": [
                {
                    "operator": "IndexScan",
                    "actualRows": 17,
                    "complete": True,
                    "partial": False,
                }
            ],
        }
        with (
            tempfile.TemporaryDirectory(prefix="sparkles-plan-") as temp,
            mock.patch.object(BENCH, "get_json", return_value={"state": "ready"}),
            mock.patch.object(BENCH, "query", return_value=17),
            mock.patch.object(
                BENCH, "raw_request", return_value=json.dumps(plan).encode()
            ),
            self.assertRaisesRegex(AssertionError, "SpatialScan"),
        ):
            BENCH.indexed_ready(SimpleNamespace(port=3986), "geo", pathlib.Path(temp))

    def test_provider_idle_does_not_make_an_unfinished_embedding_seed_ready(self):
        status = {
            "state": "ready",
            "rows": 10,
            "hnsw": {"nodes": 11},
            "overlay": {
                "inserts": 4,
                "deletes": 0,
                "indexed": 1,
                "pending": 3,
                "overBudget": False,
            },
            "skipped": {"malformed": 0, "wrongDimension": 0, "zeroNorm": 0},
            "embedding": {"state": "idle", "appliedSeq": 7, "headSeq": 7},
        }
        with (
            tempfile.TemporaryDirectory(prefix="sparkles-seed-ready-") as temp,
            mock.patch.object(BENCH, "get_json", return_value=status),
            mock.patch.object(BENCH, "embedding_count", return_value=14),
            self.assertRaisesRegex(AssertionError, "seed overlay"),
        ):
            BENCH.indexed_ready(SimpleNamespace(port=3986), "embed", pathlib.Path(temp))

    def test_canonical_embedding_seed_requires_complete_persisted_graph(self):
        clean = {
            "state": "ready",
            "files": {"opened": True, "bytes": 4096},
            "memory": {"residency": "mmap"},
            "rows": 10,
            "hnsw": {"nodes": 10},
            "overlay": {
                "inserts": 0,
                "deletes": 0,
                "indexed": 0,
                "pending": 0,
                "overBudget": False,
            },
            "skipped": {"malformed": 0, "wrongDimension": 0, "zeroNorm": 0},
            "embedding": {"state": "idle", "appliedSeq": 7, "headSeq": 7, "backlog": 0},
        }
        bad = []
        for section, key, value in (
            (None, "state", "failed"),
            (None, "files", None),
            ("files", "opened", False),
            ("files", "bytes", 0),
            ("memory", "residency", "heap"),
            (None, "rows", 9),
            ("hnsw", "nodes", 9),
            ("overlay", "inserts", 1),
            ("overlay", "deletes", 1),
            ("overlay", "indexed", 1),
            ("overlay", "pending", 1),
            ("overlay", "overBudget", True),
            ("skipped", "malformed", 1),
            ("skipped", "wrongDimension", 1),
            ("skipped", "zeroNorm", 1),
            ("embedding", "state", "embedding"),
            ("embedding", "appliedSeq", 6),
            ("embedding", "backlog", 1),
        ):
            status = json.loads(json.dumps(clean))
            (status if section is None else status[section])[key] = value
            bad.append(status)
        for status in bad:
            with (
                self.subTest(status=status),
                tempfile.TemporaryDirectory(prefix="sparkles-canonical-") as temp,
                mock.patch.object(BENCH, "get_json", return_value=status),
                self.assertRaises(AssertionError),
            ):
                BENCH.canonical_embedding_ready(
                    SimpleNamespace(port=3986), pathlib.Path(temp), 10
                )
        building = {**clean, "state": "building", "rows": 0}
        with (
            tempfile.TemporaryDirectory(prefix="sparkles-canonical-") as temp,
            mock.patch.object(BENCH, "get_json", side_effect=[building, clean]),
            mock.patch.object(BENCH.time, "sleep"),
        ):
            result = BENCH.canonical_embedding_ready(
                SimpleNamespace(port=3986), pathlib.Path(temp), 10
            )
            self.assertEqual(result, clean)
            observations = json.loads(
                (pathlib.Path(temp) / "canonical-embedding-readiness.json").read_text()
            )
            self.assertEqual(len(observations), 2)
        cold = {**clean, "embedding": {**clean["embedding"], "state": "disabled"}}
        with (
            tempfile.TemporaryDirectory(prefix="sparkles-canonical-") as temp,
            mock.patch.object(BENCH, "get_json", return_value=cold),
        ):
            self.assertEqual(
                BENCH.canonical_embedding_ready(
                    SimpleNamespace(port=3986), pathlib.Path(temp), 10, embedding=False
                ),
                cold,
            )

    def test_embedding_seed_compacts_and_rebuilds_after_stop_then_cold_validates(self):
        events = []
        args = SimpleNamespace(binary="/owned/sparkles", provider_port=3987, port=3986)

        def start(_args, _seed, directory, _grouped, **kwargs):
            events.append(("start", directory.name, kwargs.get("embedding", True)))
            return SimpleNamespace(
                close=lambda: events.append(("stop", directory.name))
            )

        def offline(_args, command, directory):
            events.append(("offline", command[1], directory.name))
            if command[1] == "compact":
                self.assertEqual(command[-2:], ["--partial", "off"])
            else:
                self.assertEqual(command[1:3], ["vector", "rebuild"])

        with (
            tempfile.TemporaryDirectory(prefix="sparkles-prepare-") as temp,
            mock.patch.object(BENCH, "generate", return_value=20),
            mock.patch.object(BENCH, "unique_triples", return_value=17),
            mock.patch.object(BENCH, "run_command"),
            mock.patch.object(BENCH, "digest", return_value="fixture-sha"),
            mock.patch.object(BENCH, "Server", side_effect=start),
            mock.patch.object(
                BENCH,
                "caught_up",
                side_effect=lambda _a: events.append(("provider-only",)) or {},
            ),
            mock.patch.object(
                BENCH,
                "indexed_ready",
                side_effect=AssertionError(
                    "bootstrap must not require canonical graph yet"
                ),
            ),
            mock.patch.object(BENCH, "source_count", return_value=17),
            mock.patch.object(BENCH, "embedding_count", return_value=10),
            mock.patch.object(BENCH, "query", return_value=10),
            mock.patch.object(BENCH, "run_preparation_command", side_effect=offline),
            mock.patch.object(
                BENCH,
                "canonical_embedding_ready",
                side_effect=lambda a, d, n, **kw: events.append(
                    ("canonical", n, kw["embedding"])
                ),
            ),
            mock.patch.object(BENCH, "flush_owned_tree", return_value={}),
        ):
            self.assertEqual(BENCH.prepare(args, pathlib.Path(temp), "embed", 1000), 17)
        self.assertEqual(
            events,
            [
                ("start", "bootstrap", True),
                ("provider-only",),
                ("stop", "bootstrap"),
                ("offline", "compact", "compact-seed"),
                ("offline", "vector", "rebuild-seed"),
                ("start", "canonical-seed", False),
                ("canonical", 10, False),
                ("stop", "canonical-seed"),
            ],
        )

    def test_offline_seed_command_has_bounded_owned_service_and_cleanup_receipts(self):
        args = SimpleNamespace(server_cpus="0")
        process = mock.Mock(returncode=0)
        process.poll.side_effect = [None, 0]
        properties = (
            "MemoryPeak=1234\nMemoryMax=12884901888\nMemorySwapMax=0\nMainPID=42\n"
        )

        def control(command, **kwargs):
            self.assertIn("timeout", kwargs)
            if "show" in command:
                return SimpleNamespace(returncode=0, stdout=properties)
            return SimpleNamespace(returncode=3, stdout="inactive")

        with (
            tempfile.TemporaryDirectory(prefix="sparkles-offline-") as temp,
            mock.patch.object(
                BENCH.subprocess, "Popen", return_value=process
            ) as launched,
            mock.patch.object(BENCH.subprocess, "run", side_effect=control),
            mock.patch.object(BENCH.os, "sched_getaffinity", return_value={0}),
            mock.patch.object(BENCH.os, "readlink", return_value="/owned/sparkles"),
            mock.patch.object(BENCH.time, "sleep"),
            mock.patch.object(BENCH, "stop_owned") as stop,
        ):
            directory = pathlib.Path(temp) / "compact"
            BENCH.run_preparation_command(
                args,
                ["/owned/sparkles", "compact", "--loc", str(directory / "seed")],
                directory,
            )
            command = launched.call_args.args[0]
            self.assertIn("MemoryMax=12G", command)
            self.assertIn("MemorySwapMax=0", command)
            self.assertIn("RuntimeMaxSec=600s", command)
            self.assertEqual(
                json.loads((directory / "resources.json").read_text())[
                    "observed_peak_bytes"
                ],
                1234,
            )
            self.assertFalse(
                json.loads((directory / "cleanup.json").read_text())["forced"]
            )
            stop.assert_called_once_with(process)

    def test_offline_seed_failure_cleanup_is_finite_and_preserves_receipts(self):
        for forced, stop_error in (
            (True, None),
            (False, OSError("client stop failed")),
        ):
            with self.subTest(forced=forced):
                args = SimpleNamespace(server_cpus="0")
                process = mock.Mock(returncode=0)
                process.poll.side_effect = [None, 0]
                controls = []

                def control(command, controls=controls, forced=forced, **kwargs):
                    controls.append(command)
                    self.assertIn("timeout", kwargs)
                    if "show" in command:
                        return SimpleNamespace(returncode=0, stdout="")
                    if forced and "stop" in command:
                        raise subprocess.TimeoutExpired(command, 40)
                    return SimpleNamespace(returncode=3, stdout="inactive")

                with (
                    tempfile.TemporaryDirectory(
                        prefix="sparkles-offline-failed-"
                    ) as temp,
                    mock.patch.object(BENCH.subprocess, "Popen", return_value=process),
                    mock.patch.object(BENCH.subprocess, "run", side_effect=control),
                    mock.patch.object(BENCH.time, "sleep"),
                    mock.patch.object(BENCH, "stop_owned", side_effect=stop_error),
                    self.assertRaises(AssertionError if forced else OSError),
                ):
                    directory = pathlib.Path(temp) / "compact"
                    try:
                        BENCH.run_preparation_command(
                            args, ["/owned/sparkles", "compact"], directory
                        )
                    finally:
                        receipt = json.loads((directory / "cleanup.json").read_text())
                        self.assertEqual(receipt["forced"], forced)
                        self.assertTrue(receipt["inactive"])
                        self.assertTrue((directory / "resources.json").exists())
                        if forced:
                            self.assertTrue(
                                any(
                                    "--kill-whom=all" in command for command in controls
                                )
                            )

    def test_reopened_spatial_index_is_ready_before_its_plan_is_used(self):
        with (
            tempfile.TemporaryDirectory(prefix="sparkles-ready-") as temp,
            mock.patch.object(
                BENCH,
                "get_json",
                side_effect=[{"state": "building"}, {"state": "ready"}],
            ) as status,
            mock.patch.object(BENCH, "query", return_value=17),
            mock.patch.object(
                BENCH, "raw_request", return_value=json.dumps(index_plan()).encode()
            ),
            mock.patch.object(BENCH.time, "sleep"),
        ):
            BENCH.indexed_ready(SimpleNamespace(port=3986), "geo", pathlib.Path(temp))
            self.assertEqual(status.call_count, 2)

    def test_executed_index_plan_requires_exact_complete_operator_and_no_fallback(self):
        valid = index_plan()
        invalid = []
        for key, value in (("executed", False), ("executed", None)):
            invalid.append({**valid, key: value})
        for key, value in (
            ("complete", False),
            ("partial", True),
            ("actualRows", None),
            ("actualRows", True),
            ("operator", "IndexScan"),
        ):
            invalid.append({**valid, "nodes": [{**valid["nodes"][0], key: value}]})
        for counters in (
            {},
            {"index": "ready", "fallback": True},
            {"index": "building", "fallback": False},
        ):
            invalid.append({**valid, "plan": {**valid["plan"], "counters": counters}})
        invalid.append({**valid, "plan": {**valid["plan"], "id": "different"}})
        for plan in [valid, index_plan("TextSearch"), *invalid]:
            with (
                self.subTest(plan=plan),
                tempfile.TemporaryDirectory(prefix="sparkles-plan-") as temp,
                mock.patch.object(
                    BENCH, "raw_request", return_value=json.dumps(plan).encode()
                ),
            ):
                path = pathlib.Path(temp) / "raw.json"
                operator = plan["nodes"][0]["operator"]
                operator = "TextSearch" if operator == "TextSearch" else "SpatialScan"
                if plan in invalid:
                    with self.assertRaises(AssertionError):
                        BENCH.executed_index_plan(3986, "query", operator, path)
                else:
                    BENCH.executed_index_plan(3986, "query", operator, path)
                self.assertEqual(json.loads(path.read_text()), plan)

    def test_selective_publication_counts_and_cold_plan_are_checked(self):
        for index, op, expected, count in (
            ("geo", "geo", 50000, 334),
            ("geo", "geo", 2200, 15),
            ("text", "literal", 2200, 2200),
        ):
            probe = BENCH.publication_index_probe(index, op, "unique", expected)
            self.assertEqual(probe["expected"], count)
            if index == "geo":
                self.assertIn("^^geo:wktLiteral", probe["query"])
                self.assertIn("99.5 39.5", probe["query"])
                self.assertIn("urn:wb:unique:", probe["query"])
            with (
                tempfile.TemporaryDirectory(prefix="sparkles-publication-") as temp,
                mock.patch.object(BENCH, "query", return_value=count),
                mock.patch.object(BENCH, "executed_index_plan") as plan,
            ):
                directory = pathlib.Path(temp)
                BENCH.verify_index_publication(3986, probe, directory)
                plan.assert_called_once_with(
                    3986,
                    probe["query"],
                    probe["operator"],
                    directory / "index-publication-plan.json",
                )
            with (
                tempfile.TemporaryDirectory(prefix="sparkles-publication-") as temp,
                mock.patch.object(BENCH, "query", return_value=count + 1),
                self.assertRaises(AssertionError),
            ):
                BENCH.verify_index_publication(3986, probe, pathlib.Path(temp))
        self.assertIsNone(BENCH.publication_index_probe("geo", "delete", "n", 0))
        self.assertIsNone(BENCH.publication_index_probe("embed", "literal", "n", 8))

    def test_rss_peak_combines_kernel_bound_with_actual_interval_samples(self):
        samples = [
            {"start_unix": lo, "end_unix": hi, "rss_kib": rss}
            for lo, hi, rss in (
                (0, 0.1, 9999),
                (1.1, 1.2, 2048),
                (1.9, 2.1, 8888),
                (3, 3.1, 7777),
            )
        ]
        peak = BENCH.writer_peak_rss("VmHWM: 1024 kB", samples, 1, 2)
        self.assertEqual(peak["peak_rss_kib"], 2048)
        self.assertEqual(peak["reset_hwm_outer_window_kib"], 1024)
        self.assertEqual(peak["sampled_writer_interval_peak_kib"], 2048)
        self.assertEqual(peak["writer_interval_sample_count"], 1)
        peak = BENCH.writer_peak_rss("VmHWM: 4096 kB", samples, 1, 2)
        self.assertEqual(peak["peak_rss_kib"], 4096)
        peak = BENCH.writer_peak_rss("VmHWM: 1024 kB", [], 1, 2)
        self.assertEqual(peak["peak_rss_kib"], 1024)
        self.assertIsNone(peak["sampled_writer_interval_peak_kib"])
        for start, end in ((2, 1), (float("nan"), 2), (1, float("inf"))):
            with self.assertRaises(AssertionError):
                BENCH.writer_peak_rss("VmHWM: 1024 kB", samples, start, end)

    def test_memory_samples_preserve_wall_read_interval(self):
        samples = []
        stop = mock.Mock()
        stop.is_set.side_effect = [False, True]
        with (
            mock.patch.object(
                BENCH.pathlib.Path, "read_text", return_value="VmRSS: 2048 kB"
            ),
            mock.patch.object(BENCH.time, "time", side_effect=[10, 10.1]),
        ):
            BENCH.memory_samples(123, stop, samples)
        self.assertEqual(samples[0]["start_unix"], 10)
        self.assertEqual(samples[0]["end_unix"], 10.1)
        self.assertEqual(samples[0]["rss_kib"], 2048)

    def test_cpu_list_expands_ranges(self):
        self.assertEqual(BENCH.cpu_set("0-3,8,10-11"), {0, 1, 2, 3, 8, 10, 11})

    def test_duplicate_input_lines_do_not_inflate_the_rdf_count_oracle(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-set-count-") as temp:
            root = pathlib.Path(temp)
            (root / "data.nt").write_text(
                '<urn:s> <urn:p> "one" .\n' * 2 + '<urn:t> <urn:p> "two" .\n'
            )
            self.assertEqual(BENCH.unique_triples(root), 2)
            self.assertFalse((root / "unique.nt").exists())

    def test_failed_server_startup_removes_only_the_owned_clone(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-server-failure-") as temp:
            root = pathlib.Path(temp)
            (root / "seed").mkdir()
            (root / "seed" / "preserve").write_text("seed")
            with (
                mock.patch.object(
                    BENCH, "Server", side_effect=RuntimeError("startup failed")
                ),
                self.assertRaisesRegex(RuntimeError, "startup failed"),
            ):
                BENCH.measure(None, root, "text", "writers", True, 0, "run", 0)
            self.assertFalse((root / "run" / "store").exists())
            self.assertEqual((root / "seed" / "preserve").read_text(), "seed")
            self.assertTrue((root / "run" / "failure.json").exists())

    def test_owned_cleanup_kills_a_process_that_ignores_termination(self):
        process = mock.Mock()
        process.poll.return_value = None
        process.wait.side_effect = [subprocess.TimeoutExpired("owned", 10), 0]
        BENCH.stop_owned(process)
        process.terminate.assert_called_once()
        process.kill.assert_called_once()
        self.assertEqual(
            process.wait.call_args_list, [mock.call(timeout=10), mock.call(timeout=5)]
        )

    def test_failed_client_output_is_preserved(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-client-failure-") as temp:
            root = pathlib.Path(temp)
            with self.assertRaises(subprocess.CalledProcessError):
                BENCH.run_client(
                    ["sh", "-c", 'printf "failure receipt\\n"; exit 2'], root
                )
            self.assertEqual((root / "client.stdout").read_text(), "failure receipt\n")
            self.assertEqual(
                json.loads((root / "client-exit.json").read_text())["returncode"], 2
            )

    def test_stuck_client_is_killed_and_partial_output_is_preserved(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-client-timeout-") as temp:
            root = pathlib.Path(temp)
            with self.assertRaises(subprocess.TimeoutExpired):
                BENCH.run_client(
                    [
                        "python3",
                        "-c",
                        'import time; print("partial",flush=True); time.sleep(60)',
                    ],
                    root,
                    timeout=0.2,
                )
            self.assertEqual((root / "client.stdout").read_text(), "partial\n")
            self.assertTrue(
                json.loads((root / "client-exit.json").read_text())["forced"]
            )

    def test_incomplete_scenario_set_and_warmup_are_not_full_acceptance(self):
        args = SimpleNamespace(
            blocks=5,
            people=[100000, 1000000],
            indexes=["text", "geo", "embed"],
            scenarios=[
                "serial-literal",
                "serial-iri",
                "serial-delete",
                "writers",
                "mixed",
            ],
            serial_changes=2000,
            writer_changes=50000,
            warmup=200,
        )
        self.assertTrue(BENCH.full_protocol(args))
        args.warmup = 0
        self.assertFalse(BENCH.full_protocol(args))
        args.warmup = 200
        args.scenarios = ["writers"] * 5
        self.assertFalse(BENCH.full_protocol(args))

    @unittest.skipUnless(
        pathlib.Path("/proc/self/clear_refs").exists(), "Linux peak-RSS reset"
    )
    def test_owned_child_high_water_reset(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-hwm-probe-") as temp:
            BENCH.probe_hwm(pathlib.Path(temp))
            result = json.loads(
                (pathlib.Path(temp) / "hwm-reset-probe.json").read_text()
            )
            self.assertTrue(result["passed"])
            self.assertGreater(result["before_kib"] - result["after_kib"], 16384)

    def test_geometry_fixture_replaces_real_ages_and_has_exact_sentinels(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-indexed-data-") as temp:
            root = pathlib.Path(temp)
            triples = BENCH.generate(root, 100, "geo")
            lines = (root / "data.nt").read_text().splitlines()
            base = (root / "base.nt").read_text().splitlines()
            self.assertEqual(triples, len(base) + BENCH.SENTINELS)
            self.assertEqual(len(lines), triples)
            self.assertFalse(
                any(
                    f"<{BENCH.FOAF_NAME.removesuffix('name')}age>" in line
                    for line in lines
                )
            )
            self.assertEqual(
                sum("wktLiteral" in line for line in lines), 100 + BENCH.SENTINELS
            )
            self.assertEqual(
                sum('"POINT(0 0)"' in line for line in lines), BENCH.SENTINELS
            )

    def test_integer_count_is_required(self):
        self.assertEqual(BENCH.count_result(answer()), 17)
        for data in (
            b"{}",
            b'{"head":{},"results":{}}',
            answer("-1"),
            answer("bad"),
            answer(datatype=BENCH.XSD + "string"),
        ):
            with self.assertRaises((AssertionError, KeyError, ValueError)):
                BENCH.count_result(data)

    def test_geometry_is_typed_and_outside_the_reader_window(self):
        for person in (0, 1, 99, 999999):
            line = BENCH.geometry_line(person)
            self.assertIn(f"<{BENCH.GEO}asWKT>", line)
            self.assertIn(f"^^<{BENCH.GEO}wktLiteral>", line)
            point = line.split("POINT(")[1].split(")")[0]
            x, y = map(float, point.split())
            self.assertGreater(x, 1)
            self.assertGreater(y, 1)

    def test_provider_vectors_are_deterministic_normalized_and_input_sensitive(self):
        one = BENCH.vector("one")
        self.assertEqual(one, BENCH.vector("one"))
        self.assertNotEqual(one, BENCH.vector("two"))
        self.assertEqual(len(one), 8)
        self.assertAlmostEqual(sum(v * v for v in one), 1)

    def test_wrong_reader_answer_fails_the_run(self):
        class Wrong(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_POST(self):
                self.rfile.read(int(self.headers["Content-Length"]))
                data = answer("16")
                self.send_response(200)
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Wrong)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        stop, samples, errors = threading.Event(), [], []
        try:
            BENCH.read_samples(
                server.server_port, BENCH.TEXT_QUERY, 17, stop, samples, errors
            )
            self.assertTrue(stop.is_set())
            self.assertTrue(errors)
            self.assertEqual(len(samples), 1)
            self.assertIn("error", samples[0])
        finally:
            server.shutdown()
            server.server_close()
            thread.join()

    def test_ordinary_post_write_peak_cannot_mask_grouped_interval_rss(self):
        rows = [
            {
                "people": 100000,
                "index": "embed",
                "scenario": "writers",
                "grouped": grouped,
                "tps": tps,
                "peak_rss_kib": upper,
                "rss_peak_components": rss_bounds(upper, lower),
            }
            for grouped, upper, lower, tps in (
                (False, 300 * 1024, 100 * 1024, 100),
                (True, 250 * 1024, 250 * 1024, 150),
            )
        ]
        summary = BENCH.summarize(rows)
        self.assertFalse(summary["passed"])
        check = summary["gates"][0]["checks"]["peak_rss"]
        self.assertEqual(check["delta_kib"], -50 * 1024)
        self.assertEqual(check["certified_delta_upper_kib"], 150 * 1024)
        for extra, accepted in ((65536, True), (65537, False)):
            rows[0].update(
                peak_rss_kib=100 * 1024, rss_peak_components=rss_bounds(100 * 1024)
            )
            rows[1].update(
                peak_rss_kib=100 * 1024 + extra,
                rss_peak_components=rss_bounds(100 * 1024 + extra),
            )
            self.assertEqual(BENCH.summarize(rows)["passed"], accepted)
        for missing in (
            None,
            {
                **rss_bounds(100 * 1024),
                "sampled_writer_interval_peak_kib": None,
                "writer_interval_sample_count": 0,
            },
        ):
            rows[0]["rss_peak_components"] = missing
            check = BENCH.summarize(rows)["gates"][0]["checks"]["peak_rss"]
            self.assertFalse(check["passed"])
            self.assertIsNone(check["certified_delta_upper_kib"])

    def test_gate_failures_are_not_masked_by_passing_memory(self):
        rows = [
            {
                "people": 100000,
                "index": "text",
                "scenario": "writers",
                "grouped": grouped,
                "peak_rss_kib": 1024,
                "rss_peak_components": rss_bounds(1024),
                "tps": tps,
            }
            for grouped, tps in ((False, 100), (True, 140))
        ]
        result = BENCH.summarize(rows)
        self.assertFalse(result["passed"])
        self.assertFalse(result["gates"][0]["checks"]["throughput"]["passed"])
        rows[1]["tps"] = 151
        self.assertTrue(BENCH.summarize(rows)["passed"])

    def test_active_reader_calibration_uses_slowest_abba_and_validates_common_rate(
        self,
    ):
        calls = []
        discovery = iter((1036.3, 1457.1, 1376.8, 668.4))

        def process(
            args, cell, index, scenario, grouped, triples, run_id, rate, *, calibration
        ):
            self.assertTrue(calibration)
            self.assertEqual((index, scenario), ("geo", "mixed"))
            calls.append((grouped, rate, run_id))
            row = {
                "calibration": True,
                "scenario": scenario,
                "grouped": grouped,
                "write_rate": rate,
                "tps": next(discovery) if not rate else rate * (1 if grouped else 0.99),
                "max_schedule_lag_ms": 200 if rate else 0,
                "reopen": "passed",
            }
            row["mixed_load_checks"] = BENCH.mixed_load_checks(row) if rate else None
            return row

        with tempfile.TemporaryDirectory(prefix="sparkles-active-rate-") as temp:
            root = pathlib.Path(temp)
            args = SimpleNamespace(rate_fraction=0.8)
            with mock.patch.object(BENCH, "measure", side_effect=process):
                rate = BENCH.calibrate_mixed(args, root, "geo", 10, 0)
            self.assertAlmostEqual(rate, 668.4 * 0.8)
            self.assertEqual(
                [grouped for grouped, _, _ in calls],
                [False, True, True, False, False, True],
            )
            self.assertEqual([r for _, r, _ in calls[:4]], [0] * 4)
            self.assertEqual([r for _, r, _ in calls[4:]], [rate] * 2)
            receipt = json.loads((root / "rate-calibration.json").read_text())
            self.assertTrue(receipt["achieved_rate_validated"])
            self.assertEqual(
                (len(receipt["unpaced"]), len(receipt["validation"])), (4, 2)
            )
            self.assertTrue(
                all(
                    not r["mixed_load_checks"]["schedule_lag"]["passed"]
                    for r in receipt["validation"]
                )
            )
            with self.assertRaisesRegex(AssertionError, "calibration"):
                BENCH.summarize(receipt["validation"])

    def test_calibration_leaves_the_requested_headroom(self):
        discovery = iter((900.0, 1200.0, 1100.0, 700.0))

        def process(
            args, cell, index, scenario, grouped, triples, run_id, rate, *, calibration
        ):
            row = {
                "calibration": True,
                "scenario": scenario,
                "grouped": grouped,
                "write_rate": rate,
                "tps": next(discovery) if not rate else rate,
                "max_schedule_lag_ms": 10 if rate else 0,
                "reopen": "passed",
            }
            row["mixed_load_checks"] = BENCH.mixed_load_checks(row) if rate else None
            return row

        with tempfile.TemporaryDirectory(prefix="sparkles-headroom-") as temp:
            root = pathlib.Path(temp)
            args = SimpleNamespace(rate_fraction=0.6)
            with mock.patch.object(BENCH, "measure", side_effect=process):
                rate = BENCH.calibrate_mixed(args, root, "text", 10, 0)
            self.assertAlmostEqual(rate, 700.0 * 0.6)
            receipt = json.loads((root / "rate-calibration.json").read_text())
            self.assertEqual(receipt["rate_fraction"], 0.6)
            self.assertAlmostEqual(receipt["common_rate"], 420.0)

    def test_failed_common_rate_preserves_both_alternatives_without_retry(self):
        rates = []

        def process(
            args, cell, index, scenario, grouped, triples, run_id, rate, *, calibration
        ):
            self.assertTrue(calibration)
            rates.append(rate)
            row = {
                "calibration": True,
                "scenario": scenario,
                "grouped": grouped,
                "write_rate": rate,
                "tps": rate if grouped else rate * 0.8,
                "max_schedule_lag_ms": 0,
                "reopen": "passed",
            }
            row["mixed_load_checks"] = BENCH.mixed_load_checks(row)
            return row

        with tempfile.TemporaryDirectory(prefix="sparkles-invalid-rate-") as temp:
            root = pathlib.Path(temp)
            with (
                mock.patch.object(BENCH, "measure", side_effect=process),
                self.assertRaisesRegex(AssertionError, "rate validation failed"),
            ):
                BENCH.calibrate_mixed(
                    SimpleNamespace(rate_fraction=0.8), root, "text", 10, 500
                )
            self.assertEqual(rates, [500, 500])
            receipt = json.loads((root / "rate-calibration.json").read_text())
            self.assertFalse(receipt["achieved_rate_validated"])
            self.assertEqual(receipt["unpaced"], [])
            self.assertEqual(len(receipt["validation"]), 2)
            self.assertTrue(all(r["reopen"] == "passed" for r in receipt["validation"]))

    def test_unpaced_mixed_acceptance_is_refused_before_creating_run(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-unpaced-refusal-") as temp:
            root = pathlib.Path(temp)
            with self.assertRaisesRegex(AssertionError, "only calibration"):
                BENCH.measure(None, root, "geo", "mixed", False, 10, "run", 0)
            self.assertFalse((root / "run").exists())

    def test_calibration_process_cannot_enter_acceptance_summary(self):
        rows = [
            {
                "people": 1000000,
                "index": "geo",
                "scenario": "mixed",
                "grouped": grouped,
                "calibration": True,
                "peak_rss_kib": 1024,
                "rss_peak_components": rss_bounds(1024),
                "readers": {"p50_ms": 1, "p99_ms": 1},
                "write_rate": 500,
                "tps": 500,
                "max_schedule_lag_ms": 1,
            }
            for grouped in (False, True)
        ]
        with self.assertRaisesRegex(AssertionError, "calibration"):
            BENCH.summarize(rows)

    def test_pacing_boundaries_remain_unchanged(self):
        row = {"write_rate": 100, "tps": 95, "max_schedule_lag_ms": 80}
        checks = BENCH.mixed_load_checks(row)
        self.assertTrue(all(v["passed"] for v in checks.values()))
        row["tps"] = 105
        self.assertTrue(BENCH.mixed_load_checks(row)["achieved_rate"]["passed"])
        row["max_schedule_lag_ms"] = 80.001
        self.assertFalse(BENCH.mixed_load_checks(row)["schedule_lag"]["passed"])
        row.update(write_rate=540.64, tps=538.2, max_schedule_lag_ms=50)
        self.assertTrue(BENCH.mixed_load_checks(row)["schedule_lag"]["passed"])
        row["max_schedule_lag_ms"] = 50.001
        self.assertFalse(BENCH.mixed_load_checks(row)["schedule_lag"]["passed"])
        row["write_rate"] = 0
        with self.assertRaises(AssertionError):
            BENCH.mixed_load_checks(row)

    def test_one_paced_stall_cannot_be_averaged_into_acceptance(self):
        rows = [
            {
                "people": 1000000,
                "index": "text",
                "scenario": "mixed",
                "grouped": grouped,
                "peak_rss_kib": 1024,
                "rss_peak_components": rss_bounds(1024),
                "readers": {"p50_ms": 1, "p99_ms": 1},
                "write_rate": 540.64,
                "tps": 538.2,
                "max_schedule_lag_ms": lag,
            }
            for grouped, lag in ((False, 854.644), (True, 1), (True, 1), (False, 1))
        ]
        result = BENCH.summarize(rows)
        self.assertFalse(result["passed"])
        checks = result["gates"][0]["checks"]
        self.assertTrue(checks["achieved_rate"]["passed"])
        self.assertFalse(checks["schedule_lag"]["passed"])
        self.assertTrue(checks["p99_ms"]["passed"])
        rows[0]["max_schedule_lag_ms"] = 1
        self.assertTrue(BENCH.summarize(rows)["passed"])
        rows[3]["tps"] = 400
        self.assertFalse(BENCH.summarize(rows)["passed"])

    def test_serial_and_read_tail_thresholds(self):
        rows = [
            {
                "people": 1000000,
                "index": "geo",
                "scenario": "serial-literal",
                "grouped": grouped,
                "writers": {"p50_ms": n, "p90_ms": n},
            }
            for grouped, n in ((False, 1), (True, 1.06))
        ]
        self.assertFalse(BENCH.summarize(rows)["passed"])
        for r in rows:
            r.update(
                scenario="mixed",
                peak_rss_kib=123,
                rss_peak_components=rss_bounds(123),
                readers={"p50_ms": 1, "p99_ms": 1},
                write_rate=100,
                tps=100,
                max_schedule_lag_ms=0,
            )
        rows[1]["readers"]["p99_ms"] = 1.11
        self.assertFalse(BENCH.summarize(rows)["passed"])


class ClientStatistics(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="sparkles-client-statistics-")
        cls.binary = pathlib.Path(cls.temp.name) / "write-bench-unit-tests"
        subprocess.run(
            [
                "rustc",
                "--edition",
                "2024",
                "--test",
                "-o",
                str(cls.binary),
                str(SCRIPTS / "write-bench.rs"),
            ],
            check=True,
            timeout=30,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def test_serial_gate_rejections_survive_json_serialization(self):
        self.run_statistic_test("statistics_preserve_serial_gate_rejections")

    def test_serial_gate_acceptance_survives_json_serialization(self):
        self.run_statistic_test("statistics_preserve_serial_gate_acceptance")

    def test_load_gate_rejections_survive_json_serialization(self):
        self.run_statistic_test("throughput_preserves_load_gate_rejections")

    def test_load_gate_acceptance_survives_json_serialization(self):
        self.run_statistic_test("throughput_preserves_load_gate_acceptance")

    def test_zero_duration_serialization(self):
        self.run_statistic_test("throughput_zero_duration_is_zero")

    def run_statistic_test(self, name):
        result = subprocess.run(
            [str(self.binary), "--exact", f"tests::{name}"],
            capture_output=True,
            check=False,
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


class Client(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="sparkles-indexed-client-")
        cls.binary = pathlib.Path(cls.temp.name) / "write-bench"
        subprocess.run(
            [
                "rustc",
                "--edition",
                "2024",
                "-O",
                "-o",
                str(cls.binary),
                str(SCRIPTS / "write-bench.rs"),
            ],
            check=True,
        )

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.requests = []
        self.receipt_step = 1
        self.response_delay = 0
        self.response_status = 200
        self.drop_response = False
        self.stop_listener_on_request = None
        self.hold_responses = None
        self.lock = threading.Lock()
        requests, lock = self.requests, self.lock
        owner = self

        class Writes(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                with lock:
                    requests.append(body.decode())
                    receipt = len(requests) * owner.receipt_step
                if owner.hold_responses is not None and not owner.hold_responses.wait(
                    timeout=10
                ):
                    self.close_connection = True
                    return
                time.sleep(owner.response_delay)
                if owner.stop_listener_on_request == len(requests):
                    owner.server.shutdown()
                    owner.server.socket.close()
                    self.close_connection = True
                    return
                if owner.drop_response:
                    self.close_connection = True
                    return
                self.send_response(owner.response_status)
                self.send_header("Content-Length", "0")
                self.send_header("Sparkles-Commit", str(receipt))
                self.end_headers()

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Writes)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join()

    def run_client(self, *extra):
        return subprocess.run(
            [
                str(self.binary),
                "--addr",
                f"127.0.0.1:{self.server.server_port}",
                "--changes",
                "8",
                "--warmup",
                "0",
                *extra,
            ],
            capture_output=True,
            check=False,
            timeout=15,
        )

    @unittest.skipUnless(
        pathlib.Path("/proc/self/task").is_dir(), "requires Linux procfs thread names"
    )
    def test_writer_threads_map_client_indices_to_native_tids(self):
        self.hold_responses = threading.Event()
        process = subprocess.Popen(
            [
                str(self.binary),
                "--addr",
                f"127.0.0.1:{self.server.server_port}",
                "--changes",
                "8",
                "--warmup",
                "0",
                "writers",
                "--clients",
                "4",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            received = 0
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                with self.lock:
                    received = len(self.requests)
                if received >= 4 or process.poll() is not None:
                    break
                time.sleep(0.005)
            self.assertGreaterEqual(received, 4, "all four real writers must be active")
            names = {}
            for task in pathlib.Path(f"/proc/{process.pid}/task").iterdir():
                try:
                    name = (task / "comm").read_text().strip()
                except FileNotFoundError:
                    continue
                if name.startswith("wb-writer-"):
                    names[name] = int(task.name)
            self.assertEqual(set(names), {f"wb-writer-{k}" for k in range(4)})
            self.assertEqual(len(set(names.values())), 4)
        finally:
            self.hold_responses.set()
            try:
                stdout, stderr = process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate(timeout=5)
                raise
        self.assertEqual(process.returncode, 0, stderr)
        result = json.loads(stdout)
        self.assertEqual(result["changes"], 8)
        self.assertEqual(result["errors"], 0)

    def test_final_response_cannot_create_lag_for_an_unsent_request(self):
        self.response_delay = 0.15
        result = self.run_client(
            "writers", "--clients", "1", "--changes", "1", "--write-rate", "100"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        row = json.loads(result.stdout)
        self.assertEqual(len(self.requests), 1)
        self.assertGreater(row["writers"]["max_ms"], 100)
        self.assertLess(row["max_schedule_lag_ms"], 50)

    def test_request_timings_match_real_backlog_and_preserve_warmup_and_errors(self):
        path = pathlib.Path(self.temp.name) / "request-timings.tsv"
        path.unlink(missing_ok=True)
        self.response_delay = 0.15
        result = self.run_client(
            "writers",
            "--clients",
            "1",
            "--changes",
            "2",
            "--write-rate",
            "100",
            "--request-timings",
            str(path),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(len(rows), 2)
        lag = max(
            max(0, float(r["start_offset_ms"]) - float(r["due_offset_ms"]))
            for r in rows
        )
        self.assertGreater(lag, 100)
        self.assertAlmostEqual(
            lag, json.loads(result.stdout)["max_schedule_lag_ms"], places=5
        )
        self.assertTrue(
            all(r["status"] == "200" and r["timed"] == "true" for r in rows)
        )
        self.assertEqual([int(r["index"]) for r in rows], [0, 1])
        self.assertTrue(
            all(float(r["end_unix"]) >= float(r["start_unix"]) > 0 for r in rows)
        )

        path.unlink()
        self.response_delay = 0
        result = self.run_client(
            "serial", "--changes", "3", "--warmup", "2", "--request-timings", str(path)
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(
            [r["timed"] for r in rows], ["false", "false", "true", "true", "true"]
        )
        self.assertTrue(all(r["due_offset_ms"] == "" for r in rows))
        self.assertEqual(json.loads(result.stdout)["changes"], 3)

        path.unlink()
        self.response_status = 500
        result = self.run_client(
            "writers", "--changes", "1", "--request-timings", str(path)
        )
        self.assertNotEqual(result.returncode, 0)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["status"], "500")

        path.unlink()
        self.drop_response = True
        result = self.run_client(
            "writers", "--changes", "1", "--request-timings", str(path)
        )
        self.assertNotEqual(result.returncode, 0)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["status"], "0")

    def test_refused_reconnect_preserves_earlier_and_failed_attempt_timings(self):
        self.stop_listener_on_request = 2
        path = pathlib.Path(self.temp.name) / "refused-reconnect.tsv"
        path.unlink(missing_ok=True)
        result = self.run_client(
            "writers",
            "--clients",
            "1",
            "--changes",
            "3",
            "--request-timings",
            str(path),
        )
        self.assertNotEqual(result.returncode, 0)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(len(self.requests), 2)
        self.assertEqual([r["status"] for r in rows], ["200", "0", "0"])
        self.assertEqual([int(r["index"]) for r in rows], [0, 1, 2])
        self.assertNotIn(b"panicked", result.stderr)

    def test_uncertain_socket_cannot_accept_a_delayed_old_receipt(self):
        binary = pathlib.Path(self.temp.name) / "write-bench-unit-tests"
        subprocess.run(
            [
                "rustc",
                "--edition",
                "2024",
                "--test",
                "-o",
                str(binary),
                str(SCRIPTS / "write-bench.rs"),
            ],
            check=True,
            timeout=30,
        )
        result = subprocess.run(
            [
                str(binary),
                "--exact",
                "tests::refused_redial_never_reuses_an_uncertain_connection",
            ],
            capture_output=True,
            check=False,
            timeout=5,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_initial_refusal_finishes_without_stranding_writer_barrier(self):
        self.server.shutdown()
        self.server.socket.close()
        path = pathlib.Path(self.temp.name) / "initial-refusal.tsv"
        path.unlink(missing_ok=True)
        result = self.run_client(
            "writers",
            "--clients",
            "2",
            "--changes",
            "3",
            "--request-timings",
            str(path),
        )
        self.assertNotEqual(result.returncode, 0)
        with path.open() as file:
            rows = list(csv.DictReader(file, delimiter="\t"))
        self.assertEqual(len(rows), 3)
        self.assertTrue(all(r["status"] == "0" for r in rows))
        self.assertEqual(sorted(int(r["index"]) for r in rows), [0, 1, 2])
        self.assertNotIn(b"panicked", result.stderr)

    def test_existing_request_timing_file_is_not_overwritten_or_written_to(self):
        path = pathlib.Path(self.temp.name) / "existing-timings.tsv"
        path.write_text("preserve\n")
        result = self.run_client("writers", "--request-timings", str(path))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.requests, [])
        self.assertEqual(path.read_text(), "preserve\n")

    def test_concurrent_geometry_writes_and_samples(self):
        samples = pathlib.Path(self.temp.name) / "geometry.tsv"
        samples.unlink(missing_ok=True)
        result = self.run_client("writers", "--op", "geo", "--samples", str(samples))
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(summary["op"], "geo")
        self.assertTrue(summary["receipts_contiguous"])
        self.assertEqual(summary["changes"], 8)
        self.assertEqual(len(samples.read_text().splitlines()), 9)
        self.assertEqual(len(self.requests), 8)
        self.assertTrue(all("asWKT" in q and "wktLiteral" in q for q in self.requests))

    def test_spatial_publication_cycle_matches_actual_client_coordinates(self):
        result = self.run_client("writers", "--op", "geo", "--changes", "301")
        self.assertEqual(result.returncode, 0, result.stderr)
        count = sum('"POINT(100 40)"' in request for request in self.requests)
        self.assertEqual(count, 3)
        self.assertEqual(
            count, BENCH.publication_index_probe("geo", "geo", "n", 301)["expected"]
        )

    def test_configured_literal_predicate_is_sent(self):
        result = self.run_client("serial", "--literal-predicate", BENCH.FOAF_NAME)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(all(f"<{BENCH.FOAF_NAME}>" in q for q in self.requests))

    def test_invalid_predicate_fails_before_any_write(self):
        result = self.run_client(
            "serial", "--literal-predicate", "urn:bad> <urn:injected"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.requests, [])

    def test_existing_sample_file_fails_before_any_write(self):
        samples = pathlib.Path(self.temp.name) / "existing.tsv"
        samples.write_text("preserve\n")
        result = self.run_client("serial", "--samples", str(samples))
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(samples.read_text(), "preserve\n")
        self.assertEqual(self.requests, [])

    def test_gaps_require_external_verification_and_preserve_ids(self):
        self.receipt_step = 2
        strict = self.run_client("serial")
        self.assertNotEqual(strict.returncode, 0)
        self.requests.clear()
        receipts = pathlib.Path(self.temp.name) / "gaps.json"
        receipts.unlink(missing_ok=True)
        verified = self.run_client(
            "serial", "--allow-receipt-gaps", "--receipts", str(receipts)
        )
        self.assertEqual(verified.returncode, 0, verified.stderr)
        self.assertFalse(json.loads(verified.stdout)["receipts_contiguous"])
        self.assertEqual(
            json.loads(receipts.read_text())["workload"], list(range(2, 18, 2))
        )


if __name__ == "__main__":
    unittest.main()
