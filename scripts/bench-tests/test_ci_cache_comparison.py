"""No-build checks for the isolated hosted cache comparison and restored helpers."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import contextlib
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("cache_comparison", ROOT / "scripts/ci-cache-comparison.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
IDENTITY = "a" * 64


class RestoredBuildScripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sparkles-cache-comparison-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = self.root / "target"
        self.binary = self.root / "installed-mbx"
        self.binary.write_bytes(b"verified installed mbx fixture")

    def launcher(self, package="example-123", content=None):
        path = self.target / "debug/build" / package / "build_script_build"
        path.parent.mkdir(parents=True, exist_ok=True)
        real = path.with_name(path.name + ".mbx-real")
        real.write_bytes(b"dependency build script ELF fixture")
        if content is None:
            content = (
                '#!/bin/sh\nMBX_BUILD_SCRIPT_SHIM_PATH="$0" exec '
                f'"$(dirname "$0")/../../.mbx-build-script-shims/{IDENTITY}/mbx" "$@"\n'
            )
        path.write_text(content)
        path.chmod(0o755)
        return path

    def test_fresh_runner_rehydrates_pruned_helpers_without_touching_launchers(self):
        first = self.launcher()
        second = self.launcher("other-456")
        before = [(path.read_bytes(), path.stat().st_mtime_ns) for path in (first, second)]
        receipt = MODULE.repair_launchers(self.target, self.binary)
        helper = self.target / "debug/.mbx-build-script-shims" / IDENTITY / "mbx"
        self.assertEqual(helper.read_bytes(), self.binary.read_bytes())
        self.assertTrue(os.access(helper, os.X_OK))
        self.assertEqual(receipt, {"launchers": 2, "helpers": 1})
        self.assertEqual(before, [(p.read_bytes(), p.stat().st_mtime_ns) for p in (first, second)])

    def test_unknown_cached_launcher_stops_before_repairing_anything(self):
        self.launcher()
        self.launcher("unknown-789", '#!/bin/sh\nexec /gone/mbx "$@"\n')
        with self.assertRaisesRegex(ValueError, "Unsupported"):
            MODULE.repair_launchers(self.target, self.binary)
        self.assertFalse((self.target / "debug/.mbx-build-script-shims").exists())

    def test_launcher_path_cannot_escape_through_a_restored_symlink(self):
        self.launcher()
        outside = self.root / "outside"
        outside.mkdir()
        (self.target / "debug/.mbx-build-script-shims").symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "escapes"):
            MODULE.repair_launchers(self.target, self.binary)
        self.assertEqual(list(outside.iterdir()), [])

    def test_managed_target_view_cannot_be_owned_by_swatinem(self):
        actual = self.root / "managed"
        actual.mkdir()
        self.target.symlink_to(actual, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "ordinary target"):
            MODULE.repair_launchers(self.target, self.binary)

    def test_empty_seed_needs_no_repair(self):
        self.assertEqual(MODULE.repair_launchers(self.target, self.binary), {"launchers": 0, "helpers": 0})


class Receipts(unittest.TestCase):
    def test_comparison_refuses_mixed_controls_or_missing_arms(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            for arm in ("remote", "target-local", "hybrid"):
                MODULE.write_json(temp / arm / "summary.json", {
                    "phase": "warm", "controls": {"arm": arm, "fixed_sha256": "same"},
                    "counts": {"binaries": 243, "non_test_binaries": 5, "test_binaries": 238, "tests": 100, "test_ids_sha256": "same"},
                    "phases": {"archive": {"exit_code": 0}},
                })
            self.assertEqual(set(MODULE.compare_reports(temp)), {"remote", "target-local", "hybrid"})
            changed = temp / "hybrid/summary.json"
            data = json.loads(changed.read_text())
            data["controls"]["fixed_sha256"] = "different compiler"
            MODULE.write_json(changed, data)
            with self.assertRaisesRegex(ValueError, "differ"):
                MODULE.compare_reports(temp)
            changed.unlink()
            with self.assertRaisesRegex(ValueError, "Exactly one"):
                MODULE.compare_reports(temp)

    def test_listed_test_identifiers_are_stable_and_counts_are_checked(self):
        first = {"test-count": 2, "rust-suites": {"binary": {"testcases": {"second": {}, "first": {}}}}}
        second = {"test-count": 2, "rust-suites": {"binary": {"testcases": {"first": {}, "second": {}}}}}
        self.assertEqual(MODULE.listing_counts(first), MODULE.listing_counts(second))
        first["test-count"] = 3
        with self.assertRaisesRegex(ValueError, "Invalid nextest"):
            MODULE.listing_counts(first)

    @unittest.skipUnless(Path("/usr/bin/time").exists(), "GNU time is required for resource receipts")
    def test_failed_command_preserves_metrics_and_fails_the_measurement(self):
        with tempfile.TemporaryDirectory() as temp, patch.dict(os.environ, CACHE_COMPARISON_RESULTS=temp):
            with self.assertRaises(subprocess.CalledProcessError) as failed:
                MODULE.run_phase("failed", [sys.executable, "-c", "print('before failure'); raise SystemExit(7)"])
            self.assertEqual(failed.exception.returncode, 7)
            receipt = json.loads((Path(temp) / "failed.phase.json").read_text())
            self.assertEqual(receipt["exit_code"], 7)
            self.assertGreaterEqual(receipt["elapsed_seconds"], 0)
            self.assertIn("before failure", (Path(temp) / "failed.log").read_text())

    def test_swatinem_metadata_uses_mbx_and_probes_keep_their_tool(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            for tool in ("mbx", "rustc"):
                path = temp / tool
                path.write_text(f'#!/bin/sh\nprintf "%s\\n" "{tool}" "$@"\n')
                path.chmod(0o755)
            env = dict(os.environ, PATH=str(temp) + os.pathsep + os.environ["PATH"])
            formatter = ROOT / "scripts/ci-cache-comparison-tool"
            cargo = subprocess.check_output([formatter, "cargo", "metadata", "--format-version", "1"], env=env, text=True)
            rustc = subprocess.check_output([formatter, "rustc", "-vV"], env=env, text=True)
            self.assertEqual(cargo.splitlines(), ["mbx", "metadata", "--format-version", "1"])
            self.assertEqual(rustc.splitlines(), ["rustc", "-vV"])
            unsupported = subprocess.run([formatter, "sh", "-c", "exit 0"], env=env, capture_output=True)
            self.assertEqual(unsupported.returncode, 64)


class FixedControls(unittest.TestCase):
    def test_controls_hash_excludes_only_declared_transport_difference(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "Cargo.lock").write_text("fixture lockfile")
            binary = root / "mbx"
            binary.write_text("verified mbx fixture")
            doctor = {"checks": [
                {"name": "remote", "severity": "pass", "detail": "compatible"},
                {"name": "policy", "severity": "pass", "detail": "configured read-write, effective read-only"},
            ]}
            answers = {
                ("git", "rev-parse", "HEAD"): MODULE.SOURCE,
                ("git", "status", "--porcelain", "--untracked-files=no"): "",
                ("mbx", "--version"): "mbx 1.28.0",
                ("rustc", "-vV"): "rustc 1.99.1 (fixture 2026-10-13)",
                ("cargo-nextest", "nextest", "--version"): "cargo-nextest 0.9.146 (fixture)",
                ("mbx", "doctor", "--json"): json.dumps(doctor),
                ("git", "rev-parse", "HEAD^{tree}"): "fixture-tree",
                ("cc", "--version"): "cc fixture",
                ("lscpu",): "fixture CPU",
            }
            env = dict(
                CACHE_COMPARISON_RESULTS=str(root / "receipts"), CACHE_COMPARISON_ARM="remote",
                CARGO_INCREMENTAL="0", CARGO_PROFILE_DEV_DEBUG="0", MBX_CACHE_LINKS="1",
                MBX_EAGER_INCREMENTAL="0", MBX_SHARE_OUT_DIR="1", MBX_BUILD_SCRIPT_EXECUTION="1",
                MBX_REMOTE_TRUST_WRITES="false", MBX_GC_MAX_SIZE="30GiB", MBX_GC_MIN_FREE_SIZE="10GiB",
                MBX_TARGET_VIEWS="1", MBX_REMOTE_URL="https://mbx.kclj.dev", MBX_REMOTE_NAMESPACE="kclejeune/sparkles",
            )
            with contextlib.chdir(root), patch.dict(os.environ, env, clear=True), patch.object(MODULE, "output", side_effect=lambda *args: answers[args]), patch.object(MODULE.shutil, "which", return_value=str(binary)):
                MODULE.assert_controls()
                first = json.loads((root / "receipts/controls.json").read_text())
                os.environ.update(CACHE_COMPARISON_ARM="hybrid", MBX_TARGET_VIEWS="0")
                MODULE.assert_controls()
                second = json.loads((root / "receipts/controls.json").read_text())
                self.assertEqual(first["fixed_sha256"], second["fixed_sha256"])
                os.environ["RUSTFLAGS"] = "-C target-cpu=native"
                with self.assertRaisesRegex(ValueError, "Unexpected compiler override"):
                    MODULE.assert_controls()


@unittest.skipUnless(shutil.which("yq"), "yq is required for workflow contracts")
class WorkflowControls(unittest.TestCase):
    def setUp(self):
        self.workflow = json.loads(subprocess.check_output([
            "yq", "-o=json", ".", str(ROOT / ".github/workflows/ci-cache-comparison.yml"),
        ], text=True))

    def test_runner_paths_are_set_only_after_a_runner_starts(self):
        for job in self.workflow["jobs"].values():
            self.assertFalse(any("runner." in str(value) for value in job["env"].values()))
            staging = next(step for step in job["steps"] if step["name"].startswith("Stage helpers"))
            self.assertIn('echo "CACHE_COMPARISON_RESULTS=$RUNNER_TEMP/ci-cache-results" >> "$GITHUB_ENV"', staging["run"])

    def test_exact_branch_one_seed_three_fresh_warm_arms(self):
        self.assertEqual(self.workflow["on"], {"push": {"branches": ["ci-cache-comparison-20261027"]}})
        jobs = self.workflow["jobs"]
        self.assertEqual(set(jobs), {"seed", "warm"})
        self.assertEqual(jobs["warm"]["needs"], "seed")
        self.assertEqual(jobs["warm"]["strategy"]["matrix"]["arm"], ["remote", "target-local", "hybrid"])
        self.assertEqual(jobs["warm"]["strategy"]["max-parallel"], 1)
        self.assertTrue(all(j["runs-on"] == "ubuntu-24.04" and j["timeout-minutes"] == 45 for j in jobs.values()))

    def test_both_target_arms_restore_exactly_the_seed_payload_and_only_seed_saves(self):
        cache_steps = []
        for job in self.workflow["jobs"].values():
            step = next(s for s in job["steps"] if s.get("id") == "target-cache")
            cache_steps.append(step)
            self.assertFalse(step["with"]["cache-bin"])
            self.assertFalse(step["with"]["cache-workspace-crates"])
            self.assertFalse(step["with"]["cache-all-crates"])
            self.assertEqual(step["with"]["workspaces"], ". -> target")
            self.assertNotIn("cache-directories", step["with"])
            self.assertEqual(step["with"]["cmd-format"], "ci-cache-comparison-tool {0}")
        self.assertEqual(cache_steps[0]["with"]["shared-key"], cache_steps[1]["with"]["shared-key"])
        self.assertIn("github.run_id", cache_steps[0]["with"]["shared-key"])
        self.assertFalse(cache_steps[1]["with"]["save-if"])
        self.assertEqual(cache_steps[1]["if"], "matrix.arm != 'remote'")
        warm_steps = self.workflow["jobs"]["warm"]["steps"]
        self.assertIn('test "$TARGET_CACHE_HIT" = true', [s.get("run") for s in warm_steps])

    def test_pinned_source_is_checked_out_after_helpers_are_staged(self):
        for job in self.workflow["jobs"].values():
            steps = job["steps"]
            self.assertIn("Stage helpers", steps[1]["name"])
            self.assertEqual(steps[2]["with"]["ref"], MODULE.SOURCE)
            mbx = next(s for s in steps if s.get("uses", "").startswith("jdx/mr-boxington-action@"))
            self.assertEqual(mbx["with"]["version"], MODULE.MBX)
            self.assertTrue(mbx["with"]["cache-links"])
            self.assertEqual(mbx["with"]["namespace"], "kclejeune/sparkles")
            self.assertFalse(any("cargo nextest" in s.get("run", "") for s in steps))
        env = self.workflow["env"]
        self.assertEqual(env["MBX_GC_MAX_SIZE"], "30GiB")
        self.assertEqual(env["MBX_REMOTE_TRUST_WRITES"], "false")
        self.assertNotIn("MBX_CACHE_DIR", env)
        self.assertNotIn("CARGO_TARGET_DIR", env)


class WorkflowSemanticValidation(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.actionlint = os.environ.get("ACTIONLINT") or shutil.which("actionlint")
        if not cls.actionlint:
            raise RuntimeError("actionlint is required; set ACTIONLINT to the verified executable")

    def test_complete_workflow_passes_actionlint_including_run_blocks(self):
        result = subprocess.run([
            self.actionlint, str(ROOT / ".github/workflows/ci-cache-comparison.yml"),
        ], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_actionlint_rejects_the_original_job_runner_context_error(self):
        text = (ROOT / ".github/workflows/ci-cache-comparison.yml").read_text()
        marker = "      CACHE_COMPARISON_PHASE: seed\n"
        self.assertIn(marker, text)
        bad = text.replace(marker, marker + "      CACHE_COMPARISON_RESULTS: ${{ runner.temp }}/ci-cache-results\n", 1)
        result = subprocess.run([self.actionlint, "-"], input=bad, cwd=ROOT, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('context "runner" is not allowed here', result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
