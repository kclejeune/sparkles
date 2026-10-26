"""Exercise local CI profile selection without building workspace crates."""
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class CiProfiles(unittest.TestCase):
    def run_profile(self, *args, failing="", notify_completed=False):
        with tempfile.TemporaryDirectory(prefix="sparkles-ci-profile-") as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            script = root / "scripts/ci.sh"
            source = (ROOT / "scripts/ci.sh").read_text()
            if notify_completed:
                # Retire completed job notifications before wait -n. Explicit wait
                # must still collect each recorded PID and preserve its status.
                source = source.replace(
                    "  set -m\n",
                    "  set -m\n  set -T\n"
                    "  trap 'if [[ $BASH_COMMAND == \"wait -n -p done_pid\" ]]; then "
                    "for ci_pid in \"${!pids[@]}\"; do "
                    "while kill -0 \"$ci_pid\" 2>/dev/null; do sleep 0.001; done; done; "
                    "jobs > /dev/null; fi' DEBUG\n",
                )
            script.write_text(source)
            frontend = root / "bin/mise"
            frontend.write_text(
                '#!/bin/bash\n'
                'printf "%s\\n" "$*" >> "$CI_CALLS"\n'
                'if [[ -n "$CI_FAIL_TASK" && " ${*: -1} " == " $CI_FAIL_TASK " ]]; then exit 29; fi\n'
            )
            frontend.chmod(0o755)
            calls = root / "calls"
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                       CI_CALLS=str(calls), CI_FAIL_TASK=failing)
            result = subprocess.run(["bash", str(script), "--jobs", "2", *args],
                                    cwd=root, env=env, capture_output=True, text=True, timeout=20)
            lines = calls.read_text().splitlines()
            tasks = [line.split()[-1] for line in lines if "--skip-deps" in line]
            return result, tasks, lines

    def test_default_preserves_full_suite_without_exhaustive_feature_builds(self):
        result, tasks, _ = self.run_profile()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(tasks), {
            "fmt:check", "ui:fmt:check", "node:fmt:check", "lint:shell", "lint:doc-paths",
            "test:bench-scripts", "lint", "ui:check", "fmt:wasm", "py:lint", "jvm:lint",
            "node:lint", "test", "ui:test", "py:test", "jvm:rust-test", "jvm:test",
            "node:test", "node:client:check", "licenses:check",
        })

    def test_exhaustive_features_adds_both_checks_once(self):
        result, tasks, lines = self.run_profile("--exhaustive-features", "--force")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(tasks.count("lint:features"), 1)
        self.assertEqual(tasks.count("test:features"), 1)
        self.assertTrue(all("--force" in line for line in lines if "--skip-deps" in line))
        self.assertIn("py:test", tasks)

    def test_named_and_fast_profiles_can_opt_in_without_duplicates(self):
        for selection in (("lint:features",), ("--fast",)):
            with self.subTest(selection=selection):
                result, tasks, _ = self.run_profile(*selection, "--exhaustive-features")
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(tasks.count("lint:features"), 1)
                self.assertEqual(tasks.count("test:features"), 1)
                self.assertNotIn("py:test", tasks)

    def test_explicit_feature_task_still_runs_and_failure_propagates(self):
        result, tasks, _ = self.run_profile("test:features", failing="test:features")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(tasks, ["test:features"])
        self.assertIn("exit 29", result.stdout)

    def test_completed_job_notifications_preserve_success_and_failure(self):
        # Cover the final drain and the reap that makes room in a full queue.
        for selection in (("lint:doc-paths",), ("lint:doc-paths", "lint:shell", "fmt:check")):
            for failing, expected in (("", 0), ("lint:doc-paths", 1)):
                with self.subTest(selection=selection, failing=failing):
                    result, tasks, _ = self.run_profile(
                        *selection, failing=failing, notify_completed=True,
                    )
                    self.assertEqual(result.returncode, expected, result.stderr)
                    self.assertEqual(set(tasks), set(selection))
                    self.assertNotIn("unbound variable", result.stderr)
                    if failing:
                        self.assertIn("exit 29", result.stdout)

    def test_mise_flag_reaches_local_gate(self):
        task = tomllib.loads((ROOT / "mise.toml").read_text())["tasks"]["ci"]
        self.assertIn('flag "--exhaustive-features"', task["usage"])
        self.assertIn("${usage_exhaustive_features:+--exhaustive-features}", task["run"])


if __name__ == "__main__":
    unittest.main()
