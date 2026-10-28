"""Exercise local CI profile selection without building workspace crates."""
import os
import re
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]

FULL_TASKS = {
    "fmt:check", "ui:fmt:check", "node:fmt:check", "lint:shell", "lint:doc-paths",
    "test:bench-scripts", "api:check", "client:types:check", "lint", "ui:check",
    "fmt:wasm", "py:lint", "jvm:lint", "node:lint", "test", "ui:test", "py:test",
    "jvm:rust-test", "jvm:test", "node:test", "node:client:check", "licenses:check",
}
FAST_TASKS = {
    "fmt:check", "lint:shell", "lint:doc-paths", "api:check", "client:types:check",
    "lint", "test",
}
FEATURE_TASKS = {"lint:features", "test:features"}


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
        self.assertEqual(set(tasks), FULL_TASKS)
        self.assertEqual(len(tasks), len(FULL_TASKS))

    def test_exhaustive_features_adds_both_checks_once(self):
        result, tasks, lines = self.run_profile("--exhaustive-features", "--force")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(tasks), FULL_TASKS | FEATURE_TASKS)
        self.assertEqual(len(tasks), len(FULL_TASKS | FEATURE_TASKS))
        self.assertEqual(tasks.count("lint:features"), 1)
        self.assertEqual(tasks.count("test:features"), 1)
        self.assertTrue(all("--force" in line for line in lines if "--skip-deps" in line))
        self.assertIn("py:test", tasks)

    def test_named_and_fast_profiles_can_opt_in_without_duplicates(self):
        for selection in (("lint:features",), ("--fast",)):
            with self.subTest(selection=selection):
                result, tasks, _ = self.run_profile(*selection, "--exhaustive-features")
                self.assertEqual(result.returncode, 0, result.stderr)
                expected = FEATURE_TASKS if selection == ("lint:features",) else FAST_TASKS | FEATURE_TASKS
                self.assertEqual(set(tasks), expected)
                self.assertEqual(len(tasks), len(expected))
                self.assertEqual(tasks.count("lint:features"), 1)
                self.assertEqual(tasks.count("test:features"), 1)
                self.assertNotIn("py:test", tasks)

    def test_default_fast_preserves_its_exact_suite(self):
        result, tasks, _ = self.run_profile("--fast")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(set(tasks), FAST_TASKS)
        self.assertEqual(len(tasks), len(FAST_TASKS))

    def test_api_and_client_check_failures_propagate(self):
        for failing in ("api:check", "client:types:check"):
            with self.subTest(failing=failing):
                result, tasks, _ = self.run_profile("--fast", failing=failing)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(set(tasks), FAST_TASKS)
                self.assertEqual(len(tasks), len(FAST_TASKS))
                self.assertIn("exit 29", result.stdout)

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


class ManualPlatformShells(unittest.TestCase):
    def commands(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text().split("  other-platforms:", 1)[1]
        runs = re.findall(r"        run: (?:\|\n((?:          [^\n]*\n?)+)|([^\n]+))", workflow)
        commands = ["\n".join(line[10:] for line in block.splitlines()) if block else scalar
                    for block, scalar in runs]
        return [command for command in commands if "mbx nextest" in command or "mbx test" in command]

    def test_macos_prepares_both_limits_and_linux_preserves_them(self):
        commands = self.commands()
        self.assertEqual(len(commands), 2)
        for platform in ("Linux", "macOS"):
            for command in commands:
                with self.subTest(platform=platform, command=command):
                    prefix = (
                        'before_soft=$(ulimit -S -c); before_hard=$(ulimit -H -c)\n'
                        'mbx() { if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then '
                        '[ "$(ulimit -S -c)" = "$before_soft" ] && [ "$(ulimit -H -c)" = "$before_hard" ]; '
                        'elif [ "$RUNNER_OS" = macOS ]; then '
                        '[ "$(ulimit -S -c)" = 0 ] && [ "$(ulimit -H -c)" = 0 ]; '
                        'else [ "$(ulimit -S -c)" = "$before_soft" ] && '
                        '[ "$(ulimit -H -c)" = "$before_hard" ]; fi; }\n'
                    )
                    result = subprocess.run(["bash", "-e", "-c", prefix + command],
                                            env=dict(os.environ, RUNNER_OS=platform),
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_failed_macos_preparation_never_starts_tests(self):
        for command in self.commands():
            result = subprocess.run(
                ["bash", "-e", "-c", 'ulimit() { return 17; }; mbx() { '
                 'if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then return 0; fi; '
                 'echo unexpected-test; };\n' + command],
                env=dict(os.environ, RUNNER_OS="macOS"), capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 17, result.stderr)
            self.assertNotIn("unexpected-test", result.stdout)

    def test_manual_policy_probe_runs_before_preparation_and_suite(self):
        command = next(command for command in self.commands() if "mbx nextest" in command)
        prefix = (
            'ulimit() { echo preparation; builtin ulimit "$@"; }; '
            'mbx() { if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then '
            'echo policy; else echo suite; fi; };\n'
        )
        result = subprocess.run(["bash", "-e", "-c", prefix + command],
                                env=dict(os.environ, RUNNER_OS="macOS"),
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["policy", "preparation", "suite"])

    def test_failed_manual_policy_probe_blocks_suite(self):
        command = next(command for command in self.commands() if "mbx nextest" in command)
        prefix = (
            'mbx() { if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then '
            'return 19; fi; echo unexpected-suite; };\n'
        )
        result = subprocess.run(["bash", "-e", "-c", prefix + command],
                                env=dict(os.environ, RUNNER_OS="macOS"),
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 19, result.stderr)
        self.assertNotIn("unexpected-suite", result.stdout)


class LocalMacTestShells(unittest.TestCase):
    def commands(self):
        tasks = tomllib.loads((ROOT / "mise.toml").read_text())["tasks"]
        return tasks["test"]["run"][1:] + [tasks["test:cargo"]["run"]]

    def test_test_hosts_prepare_only_darwin_and_policy_runs_before_preparation(self):
        for platform in ("Linux", "Darwin"):
            for command in self.commands():
                with self.subTest(platform=platform, command=command):
                    prefix = (
                        'before_soft=$(ulimit -S -c); before_hard=$(ulimit -H -c)\n'
                        'uname() { printf "%s\\n" "$CI_TEST_PLATFORM"; }\n'
                        'mbx() { if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then '
                        'echo policy; [ "$(ulimit -S -c)" = "$before_soft" ] && '
                        '[ "$(ulimit -H -c)" = "$before_hard" ]; '
                        'elif [ "$CI_TEST_PLATFORM" = Darwin ]; then '
                        '[ "$(ulimit -S -c)" = 0 ] && [ "$(ulimit -H -c)" = 0 ]; '
                        'else [ "$(ulimit -S -c)" = "$before_soft" ] && '
                        '[ "$(ulimit -H -c)" = "$before_hard" ]; fi; }\n'
                    )
                    result = subprocess.run(["bash", "-c", prefix + command],
                                            env=dict(os.environ, CI_TEST_PLATFORM=platform),
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual("policy" in result.stdout,
                                     platform == "Darwin" and "--doc" not in command)

    def test_failed_preparation_or_policy_blocks_local_workspace_tests(self):
        for command in self.commands():
            for fail_policy in (False, True):
                if fail_policy and "--doc" in command:
                    continue
                with self.subTest(command=command, fail_policy=fail_policy):
                    prefix = (
                        'uname() { echo Darwin; }; '
                        'ulimit() { return 17; }; '
                        'mbx() { if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then '
                        f'return {19 if fail_policy else 0}; fi; '
                        'echo unexpected-workspace-tests; };\n'
                    )
                    result = subprocess.run(["bash", "-c", prefix + command],
                                            capture_output=True, text=True, timeout=10)
                    self.assertEqual(result.returncode, 19 if fail_policy else 17, result.stderr)
                    self.assertNotIn("unexpected-workspace-tests", result.stdout)


class StandaloneMacExample(unittest.TestCase):
    def test_example_preserves_each_failure_and_parent_shell_limits(self):
        blocks = re.findall(r"```sh\n(.*?)```", (ROOT / "docs/DEVELOPMENT.md").read_text(), re.S)
        example = next(block for block in blocks if "keys_require_explicit_process_preparation" in block)
        for failure, expected, stages in (
            ("", 0, ["policy", "prepare", "nextest", "doctest"]),
            ("policy", 19, ["policy"]),
            ("prepare", 17, ["policy", "prepare"]),
            ("nextest", 23, ["policy", "prepare", "nextest"]),
            ("doctest", 29, ["policy", "prepare", "nextest", "doctest"]),
        ):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(prefix="sparkles-mac-example-") as directory:
                calls = Path(directory) / "calls"
                prefix = (
                    'before_soft=$(ulimit -S -c); before_hard=$(ulimit -H -c)\n'
                    'ulimit() { if [ "$*" = "-c 0" ]; then echo prepare >> "$CI_CALLS"; '
                    'if [ "$CI_FAIL_STAGE" = prepare ]; then return 17; fi; fi; builtin ulimit "$@"; }\n'
                    'mbx() { local stage status; '
                    'if [[ "$*" == *keys_require_explicit_process_preparation* ]]; then stage=policy; status=19; '
                    'elif [[ "$*" == *nextest* ]]; then stage=nextest; status=23; '
                    'else stage=doctest; status=29; fi; '
                    'echo "$stage" >> "$CI_CALLS"; '
                    'if [ "$CI_FAIL_STAGE" = "$stage" ]; then return "$status"; fi; }\n'
                )
                suffix = (
                    '\nresult=$?; '
                    '[ "$(builtin ulimit -S -c)" = "$before_soft" ] && '
                    '[ "$(builtin ulimit -H -c)" = "$before_hard" ] || exit 98; '
                    'exit "$result"\n'
                )
                result = subprocess.run(["bash", "-c", prefix + example + suffix],
                                        env=dict(os.environ, CI_CALLS=str(calls), CI_FAIL_STAGE=failure),
                                        capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, expected, result.stderr)
                self.assertEqual(calls.read_text().splitlines(), stages)


if __name__ == "__main__":
    unittest.main()
