"""Oracles and client wire behavior required by X09's native indexed gates."""

import http.server
import importlib.util
import json
import pathlib
import subprocess
import tempfile
import threading
import unittest

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
            self.assertEqual(samples, [])
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
                scenario="mixed", peak_rss_kib=123, readers={"p50_ms": 1, "p99_ms": 1}
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
        self.lock = threading.Lock()
        requests, lock = self.requests, self.lock

        class Writes(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                with lock:
                    requests.append(body.decode())
                    receipt = len(requests)
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


if __name__ == "__main__":
    unittest.main()
