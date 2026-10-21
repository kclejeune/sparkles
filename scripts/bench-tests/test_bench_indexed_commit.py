"""Oracles and client wire behavior required by X09's native indexed gates."""

import http.server
import importlib.util
import json
import mmap
import os
import pathlib
import subprocess
import tempfile
import threading
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
        parent = SCRIPTS.parent / "target"
        parent.mkdir(exist_ok=True)
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
                BENCH, "raw_request", return_value=b'{"plan":"SpatialScan"}'
            ),
            mock.patch.object(BENCH.time, "sleep"),
        ):
            BENCH.indexed_ready(SimpleNamespace(port=3986), "geo", pathlib.Path(temp))
            self.assertEqual(status.call_count, 2)

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

    def test_gate_failures_are_not_masked_by_passing_memory(self):
        rows = [
            {
                "people": 100000,
                "index": "text",
                "scenario": "writers",
                "grouped": grouped,
                "peak_rss_kib": 1024,
                "tps": tps,
            }
            for grouped, tps in ((False, 100), (True, 140))
        ]
        result = BENCH.summarize(rows)
        self.assertFalse(result["passed"])
        self.assertFalse(result["gates"][0]["checks"]["throughput"]["passed"])
        rows[1]["tps"] = 151
        self.assertTrue(BENCH.summarize(rows)["passed"])

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
                readers={"p50_ms": 1, "p99_ms": 1},
                write_rate=100,
                tps=100,
                max_schedule_lag_ms=0,
            )
        rows[1]["readers"]["p99_ms"] = 1.11
        self.assertFalse(BENCH.summarize(rows)["passed"])


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
                self.send_response(200)
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
