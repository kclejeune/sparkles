import importlib.util
import json
import pathlib
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


def serial(block, grouped, p50, mean, sync_ms, count=2200):
    return {
        "block": block,
        "scenario": "serial-literal",
        "grouped": grouped,
        "people": 1000,
        "index": "text",
        "calibration": False,
        "writers": {"p50_ms": p50, "p90_ms": p50 * 1.1, "mean_ms": mean},
        "sync": (sync_ms * count / 1000, count),
    }


class Campaign:
    def __init__(self, root, rows):
        self.root = pathlib.Path(root)
        cell = self.root / "text-1000"
        cell.mkdir(parents=True)
        seen = {}
        results = []
        for row in rows:
            if row.get("calibration"):
                results.append({k: v for k, v in row.items() if k != "sync"})
                continue
            key = (row["block"], row["scenario"])
            position = seen.get(key, 0)
            seen[key] = position + 1
            run = cell / f"b{row['block']}-{row['scenario']}-{position}"
            (run / "before").mkdir(parents=True)
            (run / "before" / "metrics.txt").write_text(scrape(0.5, 10))
            seconds, count = row["sync"]
            (run / "metrics.txt").write_text(scrape(0.5 + seconds, 10 + count))
            results.append({k: v for k, v in row.items() if k != "sync"})
        (self.root / "results.json").write_text(json.dumps(results))


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
            rows += [
                serial(block, False, 1.8, 1.9, 1.6),
                serial(block, True, 0.45, 0.55, 0.35),
                serial(block, True, 0.45, 0.55, 0.35),
                serial(block, False, 1.8, 1.9, 1.6),
            ]
        with tempfile.TemporaryDirectory() as tmp:
            Campaign(tmp, rows)
            entry = REPORT.summarize(REPORT.load(tmp))[0]
        metrics = entry["metrics"]
        self.assertEqual(entry["processes"], {"off": 6, "on": 6})
        self.assertAlmostEqual(metrics["p50_ms"]["ratio"], 0.25)
        self.assertAlmostEqual(metrics["sync_ms"]["ratio"], 0.35 / 1.6)
        self.assertAlmostEqual(metrics["overhead_ms"]["off"]["median"], 0.3)
        self.assertAlmostEqual(metrics["overhead_ms"]["on"]["median"], 0.2)

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
            (
                pathlib.Path(tmp) / "text-1000" / "b0-serial-literal-1" / "metrics.txt"
            ).unlink()
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

    def test_marks_and_render_carry_the_harness_verdicts(self):
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
