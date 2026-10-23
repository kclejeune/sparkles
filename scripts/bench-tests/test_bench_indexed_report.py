import contextlib
import importlib.util
import io
import json
import pathlib
import statistics
import tempfile
import unittest

SCRIPTS = pathlib.Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "indexed_report", SCRIPTS / "bench-indexed-report.py"
)
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


def scrape(seconds, count, path="direct"):
    return (
        f'sparkles_commit_sync_seconds_sum{{dataset="bench",path="{path}"}} {seconds}\n'
        f'sparkles_commit_sync_seconds_count{{dataset="bench",path="{path}"}} {count}\n'
        'sparkles_commit_sync_seconds_bucket{dataset="bench",path="direct",le="1"} 9\n'
    )


def serial(
    block,
    grouped,
    p50,
    mean,
    sync_ms,
    count=2200,
    index="text",
    people=1000,
    scenario="serial-literal",
    syncs=None,
    warmup=(0, 0.0),
):
    """A serial process whose counters hold `syncs` commits, by default one per request.

    The client sent `count` timed requests of mean latency `mean` after `warmup` untimed
    requests of the given count and latency.
    """
    count_all = count + warmup[0]
    syncs = count_all if syncs is None else syncs
    return {
        "block": block,
        "scenario": scenario,
        "grouped": grouped,
        "people": people,
        "index": index,
        "calibration": False,
        "writers": {"p50_ms": p50, "p90_ms": p50 * 1.1, "mean_ms": mean},
        "sync": (sync_ms * syncs / 1000, syncs),
        "attempts": (count, mean, warmup),
    }


def writers(block, grouped, tps, sync_ms):
    """A four-writer process that committed 50,000 changes."""
    return {
        "block": block,
        "scenario": "writers",
        "grouped": grouped,
        "people": 1000,
        "index": "text",
        "calibration": False,
        "tps": tps,
        "peak_rss_kib": 200 * 1024,
        "writers": {"p50_ms": 1.0, "p90_ms": 1.0, "mean_ms": 1.0},
        "sync": (sync_ms * 50 / 1000, 50),
        "attempts": (0, 0.0, (0, 0.0)),
    }


class Campaign:
    """Lays out a campaign directory as bench-indexed-commit.py writes it."""

    def __init__(self, root, rows):
        self.root = pathlib.Path(root)
        seen = {}
        results = []
        for row in rows:
            result = {k: v for k, v in row.items() if k not in ("sync", "attempts")}
            results.append(result)
            if row.get("calibration"):
                continue
            key = (row["index"], row["people"], row["block"], row["scenario"])
            position = seen.get(key, 0)
            seen[key] = position + 1
            run = (
                self.root
                / f"{row['index']}-{row['people']}"
                / f"b{row['block']}-{row['scenario']}-{position}"
            )
            (run / "before").mkdir(parents=True)
            (run / "before" / "metrics.txt").write_text(scrape(0.5, 10))
            seconds, count = row["sync"]
            (run / "metrics.txt").write_text(scrape(0.5 + seconds, 10 + count))
            attempts, mean, (warmups, warmup_ms) = row["attempts"]
            lines = ["client\tindex\ttimed\tstart_unix\tend_unix\tlatency_ms\tstatus"]
            lines += [f"0\t{i}\tfalse\t0\t0\t{warmup_ms}\t200" for i in range(warmups)]
            lines += [f"0\t{i}\ttrue\t0\t0\t{mean}\t200" for i in range(attempts)]
            (run / "request-timings.tsv").write_text("\n".join(lines) + "\n")
        (self.root / "results.json").write_text(json.dumps(results))


def abba(block, off, on, **kwargs):
    return [
        serial(block, False, *off, **kwargs),
        serial(block, True, *on, **kwargs),
        serial(block, True, *on, **kwargs),
        serial(block, False, *off, **kwargs),
    ]


class IndexedReport(unittest.TestCase):
    def test_sync_mean_subtracts_the_baseline_scrape(self):
        with tempfile.TemporaryDirectory() as tmp:
            run = pathlib.Path(tmp)
            (run / "before").mkdir()
            (run / "before" / "metrics.txt").write_text(scrape(1.0, 100))
            (run / "metrics.txt").write_text(
                scrape(1.5, 200) + scrape(0.25, 50, "group")
            )
            self.assertAlmostEqual(REPORT.sync_mean_ms(run), 750 / 150)

    def test_missing_scrape_has_no_mean(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertIsNone(REPORT.sync_mean_ms(pathlib.Path(tmp)))

    def test_overhead_matches_when_only_the_drive_moves(self):
        # The ordinary writer meets a slow flush and grouping a fast one, so the
        # request latencies differ by 4x while the engine overhead is equal.
        rows = []
        for block in range(3):
            rows += abba(block, (1.8, 1.9, 1.6), (0.45, 0.55, 0.35))
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        metrics = entry["metrics"]
        self.assertEqual(entry["processes"], {"off": 6, "on": 6})
        self.assertAlmostEqual(metrics["p50_ms"]["ratio"], 0.25)
        self.assertAlmostEqual(metrics["sync_ms"]["ratio"], 0.35 / 1.6)
        self.assertAlmostEqual(metrics["overhead_ms"]["off"]["median"], 0.3)
        self.assertAlmostEqual(metrics["overhead_ms"]["on"]["median"], 0.2)

    def test_overhead_uses_the_commits_the_counters_hold(self):
        # 200 warmup requests take 1.0 ms and sync in 1.6 ms, and 2,000 timed requests
        # take 0.43 ms and sync in 0.23 ms. The counters average 0.3 ms over the 2,200
        # commits and the requests average 0.482 ms, so the overhead is 0.182 ms. The
        # timed requests alone would give 0.43 - 0.3 = 0.13 ms.
        sync_ms = (200 * 1.6 + 2000 * 0.23) / 2200
        mean = (200 * 1.0 + 2000 * 0.43) / 2200
        row = (0.43, 0.43, sync_ms)
        rows = abba(0, row, row, count=2000, warmup=(200, 1.0))
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        overhead = entry["metrics"]["overhead_ms"]["off"]["median"]
        self.assertAlmostEqual(overhead, mean - sync_ms)
        self.assertGreater(overhead - (0.43 - sync_ms), 0.05)

    def test_no_overhead_when_the_counters_hold_other_commits(self):
        # The delete scenario seeds its rows first, so the counters hold twice the
        # commits of the timed requests, and the embedding worker adds its own.
        rows = []
        for block in range(2):
            rows += abba(
                block,
                (0.4, 0.4, 0.23),
                (0.4, 0.4, 0.23),
                scenario="serial-delete",
                syncs=4400,
            )
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        self.assertIn("p50_ms", entry["metrics"])
        self.assertIn("sync_ms", entry["metrics"])
        self.assertNotIn("overhead_ms", entry["metrics"])

    def test_paired_ratio_compares_neighbours_inside_each_block(self):
        # The drive slows down for the second half of every block. A pooled ratio sees
        # the ordinary writer slow in position 3 only, but each pair stays inside one
        # state of the drive.
        rows = []
        for block in range(2):
            rows += [
                serial(block, False, 1.0, 1.0, 0.5),
                serial(block, True, 2.0, 2.0, 0.5),
                serial(block, True, 4.0, 4.0, 0.5),
                serial(block, False, 2.0, 2.0, 0.5),
            ]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        p50 = entry["metrics"]["p50_ms"]
        self.assertAlmostEqual(p50["ratio"], 3.0 / 1.5)
        self.assertEqual(p50["paired"]["range"], [2.0, 2.0])
        self.assertAlmostEqual(p50["paired"]["median"], 2.0)

    def test_metrics_are_omitted_when_a_process_lacks_a_scrape(self):
        rows = [
            serial(0, False, 1.0, 1.0, 0.5),
            serial(0, True, 1.0, 1.0, 0.5),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            run = pathlib.Path(tmp) / "text-1000" / "b0-serial-literal-1"
            (run / "metrics.txt").unlink()
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        self.assertIn("p50_ms", entry["metrics"])
        self.assertNotIn("sync_ms", entry["metrics"])
        self.assertNotIn("overhead_ms", entry["metrics"])

    def test_calibration_processes_are_not_reported(self):
        calibration = serial(0, False, 9.0, 9.0, 1.0)
        calibration["calibration"] = True
        rows = [
            calibration,
            serial(0, False, 1.0, 1.0, 0.5),
            serial(0, True, 1.0, 1.0, 0.5),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        self.assertEqual(entry["processes"], {"off": 1, "on": 1})

    def test_a_campaign_with_several_cells_resolves_each_directory(self):
        # The default campaign holds one directory per index and size, and every cell
        # numbers its positions from zero. The cells hold different latencies.
        rows = []
        for index, people, p50 in (
            ("text", 1000, 1.0),
            ("geo", 1000, 2.0),
            ("text", 2000, 3.0),
        ):
            for block in range(2):
                rows += abba(
                    block,
                    (p50, p50, 0.1),
                    (p50, p50, 0.1),
                    index=index,
                    people=people,
                )
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entries = REPORT.summarize(REPORT.load(tmp))
        medians = {
            (e["index"], e["people"]): e["metrics"]["p50_ms"]["off"]["median"]
            for e in entries
        }
        self.assertEqual(
            medians, {("geo", 1000): 2.0, ("text", 1000): 1.0, ("text", 2000): 3.0}
        )
        for e in entries:
            self.assertEqual(e["processes"], {"off": 4, "on": 4})
            self.assertAlmostEqual(
                e["metrics"]["sync_ms"]["off"]["median"], 0.1, places=6
            )

    def test_marks_stay_with_their_campaign(self):
        # Two campaigns of the same cell with opposite verdicts print their own marks.
        def campaign(root, passed):
            Campaign(root, abba(0, (1.0, 1.0, 0.5), (1.0, 1.0, 0.5)))
            gate = {
                "people": 1000,
                "index": "text",
                "scenario": "serial-literal",
                "checks": {"p50_ms": {"passed": passed}},
            }
            (pathlib.Path(root) / "gate-summary.json").write_text(
                json.dumps({"gates": [gate]})
            )

        with (
            tempfile.TemporaryDirectory() as first,
            tempfile.TemporaryDirectory() as second,
        ):
            campaign(first, False)
            campaign(second, True)
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                REPORT.main([first, second])
        before, after = out.getvalue().split(f"campaign {second}")
        self.assertIn("p50_ms=FAIL", before)
        self.assertNotIn("p50_ms=ok", before)
        self.assertIn("p50_ms=ok", after)
        self.assertNotIn("FAIL", after)

    def amended(self, rows):
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            return REPORT.summarize(REPORT.load(tmp))[0]["amended"]

    def test_amended_serial_gate_accepts_a_faster_grouped_writer(self):
        # The drive is slow (1.0 ms) or fast (0.25 ms) while a process runs. Six ordinary
        # processes meet the slow state and four the fast one, and grouping meets the slow
        # state in four processes and the fast one in six. The pooled ratio is 0.25, which
        # the original two-sided gate rejects. Eight of the ten neighbour pairs share a
        # state, so the median pair ratio is 1.0.
        slow, fast = (1.0, 1.0, 0.5), (0.25, 0.25, 0.1)
        layout = [
            ((slow, slow), (slow, slow)),
            ((slow, slow), (slow, slow)),
            ((slow, fast), (slow, fast)),
            ((fast, fast), (fast, fast)),
            ((fast, fast), (fast, fast)),
        ]
        rows = []
        for block, (first, second) in enumerate(layout):
            rows += [
                serial(block, False, *first[0]),
                serial(block, True, *first[1]),
                serial(block, True, *second[1]),
                serial(block, False, *second[0]),
            ]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        self.assertAlmostEqual(entry["metrics"]["p50_ms"]["ratio"], 0.25)
        verdict = entry["amended"]["p50_ms"]
        self.assertTrue(verdict["passed"])
        self.assertAlmostEqual(verdict["paired"], 1.0)

    def test_amended_serial_gate_rejects_a_slower_grouped_writer(self):
        rows = []
        for block in range(3):
            rows += abba(block, (1.0, 1.0, 0.5), (1.06, 1.06, 0.5))
        amended = self.amended(rows)
        self.assertFalse(amended["p50_ms"]["passed"])
        self.assertFalse(amended["p90_ms"]["passed"])

    def test_required_gain_follows_the_ordinary_synchronization(self):
        self.assertAlmostEqual(REPORT.required_gain(1.2), 1.5)
        self.assertAlmostEqual(REPORT.required_gain(3.0), 1.5)
        self.assertAlmostEqual(REPORT.required_gain(0.6), 1.25)
        self.assertAlmostEqual(REPORT.required_gain(0.0), 1.0)

    def test_amended_throughput_gate_scales_with_the_synchronization(self):
        # A gain of 1.4 passes where the ordinary synchronization takes 0.9 ms, which
        # requires 1.375, and fails where it takes 1.5 ms and 1.5 is required.
        def rows(sync_ms):
            out = []
            for block in range(3):
                out += [
                    writers(block, False, 1000, sync_ms),
                    writers(block, True, 1400, 1.5),
                    writers(block, True, 1400, 1.5),
                    writers(block, False, 1000, sync_ms),
                ]
            return out

        short = self.amended(rows(0.9))["throughput"]
        self.assertAlmostEqual(short["required"], 1.375)
        self.assertAlmostEqual(short["paired"], 1.4)
        self.assertTrue(short["passed"])
        long = self.amended(rows(1.5))["throughput"]
        self.assertAlmostEqual(long["required"], 1.5)
        self.assertFalse(long["passed"])

    def test_amended_throughput_judges_neighbour_pairs_not_pooled_medians(self):
        # The ordinary writer is slow after two grouped runs. The pooled ratio is 1.2
        # and the pairs give 1.6 and 1.1, so the median pair is 1.35.
        rows = []
        for block in range(3):
            rows += [
                writers(block, False, 1000, 1.2),
                writers(block, True, 1600, 1.5),
                writers(block, True, 1400, 1.5),
                writers(block, False, 1273, 1.2),
            ]
        amended = self.amended(rows)["throughput"]
        self.assertAlmostEqual(amended["paired"], statistics.median([1.6, 1400 / 1273]))
        self.assertFalse(amended["passed"])

    def test_render_shows_the_amended_verdicts(self):
        rows = abba(0, (1.0, 1.0, 0.5), (1.0, 1.0, 0.5))
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            text = REPORT.render(REPORT.summarize(REPORT.load(tmp)), {})
        self.assertIn("amended gate p50_ms: ok paired 1.000, limit 1.05", text)

    def test_render_carries_the_harness_verdicts(self):
        rows = [serial(0, False, 1.0, 1.0, 0.5), serial(0, True, 1.0, 1.0, 0.5)]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            (pathlib.Path(tmp) / "gate-summary.json").write_text(
                json.dumps(
                    {
                        "gates": [
                            {
                                "people": 1000,
                                "index": "text",
                                "scenario": "serial-literal",
                                "checks": {
                                    "p50_ms": {"passed": True},
                                    "p90_ms": {"passed": False},
                                },
                            }
                        ]
                    }
                )
            )
            text = REPORT.render(REPORT.summarize(REPORT.load(tmp)), REPORT.marks(tmp))
        self.assertIn("p50_ms=ok p90_ms=FAIL", text)
        self.assertIn("overhead_ms: off 0.500", text)


if __name__ == "__main__":
    unittest.main()
