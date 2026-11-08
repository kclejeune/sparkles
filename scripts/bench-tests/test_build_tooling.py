"""Real Boxington contracts for mise task caching and managed-target consumers."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
MBX = shutil.which("mbx")


@unittest.skipUnless(MBX and sys.platform.startswith("linux"), "mbx and Linux XDG isolation are required")
class BoxingtonContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sparkles-mbx-contract-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.workspace = self.root / "project"
        (self.workspace / "src").mkdir(parents=True)
        (self.workspace / "Cargo.toml").write_text(
            '[package]\nname="sparkles-mbx-contract"\nversion="0.0.0"\n'
            'edition="2024"\n[workspace]\n'
        )
        (self.workspace / "src/lib.rs").write_text("pub fn value() -> u8 { 1 }\n")
        channel = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("MBX_")}
        for key in ("CARGO_TARGET_DIR", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
            self.env.pop(key, None)
        self.env.update(
            RUSTUP_TOOLCHAIN=channel,
            XDG_CONFIG_HOME=str(self.root / "config"),
            MBX_CACHE_DIR=str(self.root / "cache"),
            MBX_TARGET_VIEWS="1",
            MBX_GC_AUTO="0",
            MBX_REMOTE_URL="",
            PATH=str(ROOT / "scripts") + os.pathsep + self.env["PATH"],
        )

    def mbx(self, *args):
        return subprocess.run(
            [MBX, *args], cwd=self.workspace, env=self.env,
            capture_output=True, text=True, timeout=45, check=True,
        )

    def waiting_child(self, protected):
        command = ["python3", "-u", "-c", "import sys; print('ready', flush=True); sys.stdin.read()"]
        if protected:
            command = [MBX, "sparkles-task", "--", *command]
        process = subprocess.Popen(
            command, cwd=self.workspace, env=self.env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(lambda: self.stop(process))
        # A finite timeout also covers a wrapper regression before child startup.
        import selectors
        with selectors.DefaultSelector() as ready:
            ready.register(process.stdout, selectors.EVENT_READ)
            self.assertTrue(ready.select(15), "task did not start")
        self.assertEqual(process.stdout.readline().strip(), "ready")
        return process

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.communicate("stop", timeout=15)

    def test_downstream_task_holds_managed_target_lease(self):
        self.mbx("build", "--offline")
        target = self.workspace / "target"
        self.assertTrue(target.is_symlink())
        unprotected = self.waiting_child(False)
        self.mbx("clean", str(self.workspace))
        self.assertFalse(target.exists(), "negative control unexpectedly retained target")
        self.stop(unprotected)
        self.mbx("build", "--offline")
        protected = self.waiting_child(True)
        self.mbx("clean", str(self.workspace))
        self.assertTrue(target.exists(), "collector removed a leased target")
        self.stop(protected)
        self.assertEqual(protected.returncode, 0)
        self.mbx("clean", str(self.workspace))
        self.assertFalse(target.exists(), "lease was not released")
        failed = subprocess.run(
            [MBX, "sparkles-task", "--", "bash", "-c", "exit 37"],
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(failed.returncode, 37, failed.stderr)
        streamed = subprocess.run(
            [MBX, "sparkles-task", "--", "bash", "-e"], input="echo before\nexit 19\necho after\n",
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(streamed.returncode, 19, streamed.stderr)
        self.assertIn("before", streamed.stdout)
        self.assertNotIn("after", streamed.stdout)

    def ci_setup(self, check):
        """Put scripts/ci.sh in the contract workspace with a mise stand-in whose prepare
        step builds and whose `jvm:test` check runs `check`, and return its environment."""
        (self.workspace / "scripts").mkdir(exist_ok=True)
        for name in ("ci.sh", "cargo-sparkles-task"):
            shutil.copy2(ROOT / "scripts" / name, self.workspace / "scripts" / name)
        bin_dir = self.root / "ci-bin"
        bin_dir.mkdir(exist_ok=True)
        frontend = bin_dir / "mise"
        frontend.write_text(
            "#!/bin/sh\n"
            "for task; do :; done\n"
            'case "$task" in\n'
            "  jvm:native) exec mbx build --offline ;;\n"
            # The collection that removed a gate's target between two of its tasks, forced.
            f"  jvm:test) {check} ;;\n"
            "  *) exit 8 ;;\n"
            "esac\n"
        )
        frontend.chmod(0o755)
        env = dict(self.env, PATH=str(bin_dir) + os.pathsep + self.env["PATH"])
        env.pop("SPARKLES_CI_LEASED", None)
        return env

    def ci_run(self, leased):
        """Run scripts/ci.sh on `jvm:test`, whose check collects the target before it reads
        what prepare built."""
        env = self.ci_setup(
            'mbx clean "$PWD" || exit 9; test -e target/debug/libsparkles_mbx_contract.rlib'
        )
        if not leased:
            # A run that takes an outer command's lease for granted, as every run of
            # ci.sh did before it took one itself.
            env["SPARKLES_CI_LEASED"] = "1"
        result = subprocess.run(
            ["bash", "scripts/ci.sh", "jvm:test"], cwd=self.workspace, env=env,
            capture_output=True, text=True, timeout=120,
        )
        summary = (self.workspace / ".artifacts/ci/summary.txt").read_text()
        return result, summary

    def test_ci_run_holds_managed_target_lease_between_tasks(self):
        result, summary = self.ci_run(leased=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(summary, r"prepare\s+pass")
        self.assertRegex(summary, r"jvm:test\s+pass")
        self.assertTrue((self.workspace / "target").exists(), "collector removed a leased target")
        self.mbx("clean", str(self.workspace))
        self.assertFalse((self.workspace / "target").exists(), "ci.sh did not release its lease")

    def test_ci_run_without_the_lease_loses_prepared_outputs(self):
        # The negative control: the same run without the lease loses what prepare built.
        result, summary = self.ci_run(leased=False)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertRegex(summary, r"prepare\s+pass")
        self.assertRegex(summary, r"jvm:test\s+FAIL")

    def test_terminated_ci_run_stops_its_tasks_and_releases_the_lease(self):
        started = self.root / "check-started"
        env = self.ci_setup(f'touch "{started}"; exec sleep 60')
        run = subprocess.Popen(
            ["bash", "scripts/ci.sh", "jvm:test"], cwd=self.workspace, env=env,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(lambda: run.poll() is None and run.kill())
        deadline = time.monotonic() + 60
        while not started.exists():
            self.assertIsNone(run.poll(), run.communicate()[1] if run.poll() is not None else "")
            self.assertLess(time.monotonic(), deadline, "the check did not start")
            time.sleep(0.05)
        self.mbx("clean", str(self.workspace))
        self.assertTrue((self.workspace / "target").exists(), "collector removed a leased target")
        run.terminate()
        _, stderr = run.communicate(timeout=30)
        self.assertEqual(run.returncode, 130, stderr)
        summary = (self.workspace / ".artifacts/ci/summary.txt").read_text()
        self.assertRegex(summary, r"jvm:test\s+stopped")
        # The holder ends with the script, so the next collection may take the target.
        deadline = time.monotonic() + 15
        while (self.workspace / "target").exists():
            self.assertLess(time.monotonic(), deadline, "the lease outlived the CI run")
            self.mbx("clean", str(self.workspace))
            time.sleep(0.2)

    def test_explicit_target_dir_needs_the_task_lease_after_collection(self):
        # A collection that has moved the view away and dropped its record, but not yet the
        # link, leaves `target` dangling. mbx does not place an explicitly named target, so
        # a bare build fails, and the same build under sparkles-task gets a fresh view.
        self.mbx("build", "--offline")
        view = Path(os.readlink(self.workspace / "target"))
        shutil.rmtree(view)
        view.with_suffix(".json").unlink()
        bare = subprocess.run(
            [MBX, "build", "--offline", "--target-dir", "target"], cwd=self.workspace,
            env=self.env, capture_output=True, text=True, timeout=45,
        )
        self.assertNotEqual(bare.returncode, 0, "the negative control built through the link")
        self.assertIn("Not a directory", bare.stderr)
        self.mbx("sparkles-task", "--", MBX, "build", "--offline", "--target-dir", "target")
        self.assertTrue((self.workspace / "target/debug").is_dir())

    def dangle_target(self):
        """Leave `target` as a link to a collected view, as a collection that removed the
        view and its record before the link does."""
        self.mbx("build", "--offline")
        link = self.workspace / "target"
        view = Path(os.readlink(link))
        shutil.rmtree(view)
        view.with_suffix(".json").unlink()
        self.assertTrue(link.is_symlink() and not link.exists(), "target is not dangling")

    def crash_harness_step(self):
        """The crash-harness step of mise.toml's test:bench-scripts, as a script run from
        the contract workspace."""
        command = tomllib.loads((ROOT / "mise.toml").read_text())["tasks"]["test:bench-scripts"]["run"][1]
        self.assertIn("crash-harness.rs", command)
        (self.workspace / "scripts").mkdir(exist_ok=True)
        shutil.copy2(ROOT / "scripts/crash-harness.rs", self.workspace / "scripts/crash-harness.rs")
        step = self.workspace / "crash-step.sh"
        # A checkout path with a space, which the command must quote.
        self.config_root = self.root / "spaced checkout"
        step.write_text("set -e\n" + command.replace("{{config_root}}", str(self.config_root)) + "\n")
        return step

    def test_crash_harness_step_survives_a_dangling_target(self):
        # The step's test binary is scratch output under .artifacts, so a collected target
        # cannot fail its mkdir, which once failed with "File exists".
        step = self.crash_harness_step()
        self.dangle_target()
        result = subprocess.run(["bash", str(step)], cwd=self.workspace, env=self.env,
                                capture_output=True, text=True, timeout=300)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertTrue((self.config_root / ".artifacts/crash-harness/crash-harness-test").is_file())

    def test_premerge_repairs_a_dangling_target(self):
        step = self.crash_harness_step()
        for name in ("premerge.sh", "cargo-sparkles-task"):
            shutil.copy2(ROOT / "scripts" / name, self.workspace / "scripts" / name)
        bin_dir = self.root / "premerge-bin"
        bin_dir.mkdir()
        # A mise stand-in that runs only the crash-harness step of test:bench-scripts.
        frontend = bin_dir / "mise"
        frontend.write_text(
            "#!/bin/sh\n"
            f'[ "$*" = "run test:bench-scripts" ] && exec bash "{step}"\n'
            "exit 8\n"
        )
        frontend.chmod(0o755)
        env = dict(self.env, PATH=str(bin_dir) + os.pathsep + self.env["PATH"])
        self.dangle_target()
        result = subprocess.run(["bash", "scripts/premerge.sh", "bench-scripts"], cwd=self.workspace,
                                env=env, capture_output=True, text=True, timeout=300)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("premerge: target links to a collected directory", result.stdout)
        self.assertRegex(result.stdout, r"PASS bench-scripts")
        self.assertTrue((self.workspace / "target").is_dir(), "premerge did not place a fresh target")
        # A target that is in place is left alone, without the message.
        again = subprocess.run(["bash", "scripts/premerge.sh", "bench-scripts"], cwd=self.workspace,
                               env=env, capture_output=True, text=True, timeout=300)
        self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
        self.assertNotIn("collected directory", again.stdout)

    def test_container_checksum_failure_stops_even_in_conditional_source(self):
        fake = self.root / "fake-bin"
        fake.mkdir()
        marker = self.root / "installed-unverified"
        tools = {
            "curl": '#!/bin/sh\nwhile [ "$#" -gt 0 ]; do if [ "$1" = --output ]; then shift; printf corrupt > "$1"; break; fi; shift; done\n',
            "install": '#!/bin/sh\ntouch "$INSTALL_MARKER"\n',
            "mbx": '#!/bin/sh\nexit 0\n',
            "cargo": '#!/bin/sh\nexit 0\n',
        }
        for name, content in tools.items():
            path = fake / name
            path.write_text(content)
            path.chmod(0o755)
        env = dict(self.env, PATH=str(fake) + os.pathsep + self.env["PATH"],
                   INSTALL_MARKER=str(marker), MBX_INSTALL_DIR=str(self.root / "installation"))
        result = subprocess.run(
            ["bash", "-c", 'if source "$1"; then exit 0; else exit 71; fi',
             "bootstrap-control", str(ROOT / "scripts/install-mbx-container.sh")],
            env=env, cwd=ROOT, capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(result.returncode, 71, result.stderr)
        self.assertFalse(marker.exists(), "unverified binary reached installation")

    def test_global_policy_changes_key_without_exposing_settings(self):
        script = ROOT / "scripts/mbx-policy-key.py"
        def key():
            result = subprocess.run(
                ["python3", str(script)], env=self.env, cwd=self.workspace,
                capture_output=True, text=True, timeout=15, check=True,
            )
            value = result.stdout.strip()
            self.assertRegex(value, r"^[0-9a-f]{64}$")
            return value
        config = Path(self.env["XDG_CONFIG_HOME"]) / "mbx/config.toml"
        config.parent.mkdir(parents=True)
        config.write_text('[linker]\ndefault="system"\n')
        first = key()
        config.write_text('[linker]\ndefault="rust-lld"\n')
        self.assertNotEqual(first, key())
        config.write_text('eager_incremental=true\n[linker]\ndefault="system"\n')
        self.assertNotEqual(first, key())
        # Token changes cannot reveal values or invalidate a build-policy key.
        config.write_text('[remote]\ntoken="fixture-only-private-token"\n[linker]\ndefault="system"\n')
        self.assertEqual(first, key())


class TaskFailureContracts(unittest.TestCase):
    def test_shex_wrapper_preserves_build_failure(self):
        program = tomllib.loads((ROOT / "mise.toml").read_text())["tasks"]["bench:shex"]["run"]
        with tempfile.TemporaryDirectory(prefix="sparkles-mbx-shell-") as directory:
            root = Path(directory)
            (root / "bin").mkdir()
            (root / ".artifacts/bench-data").mkdir(parents=True)
            (root / ".artifacts/bench-data/shacl-1.nt").touch()
            frontend = root / "bin/mbx"
            frontend.write_text(
                '#!/bin/sh\nif [ "$1" = sparkles-task ]; then shift; shift; exec "$@"; fi\nexit 23\n'
            )
            frontend.chmod(0o755)
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                       usage_people="1", usage_compare_rudof="")
            result = subprocess.run(["bash", "-c", program], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 23, result.stderr)


class JvmBindingContracts(unittest.TestCase):
    def run_two_jvm_tasks(self, locked):
        """Start two JVM tasks a second apart and return their exit codes and errors."""
        with tempfile.TemporaryDirectory(prefix="sparkles-jvm-lock-") as directory:
            root = Path(directory)
            for name in ("jvm-native.sh", "jvm-gradle.sh", "with-lock.py"):
                (root / "scripts").mkdir(exist_ok=True)
                shutil.copy2(ROOT / "scripts" / name, root / "scripts" / name)
            tools = {
                "bin/mbx": "#!/bin/sh\nexit 0\n",
                # The generator writes the bindings a moment after the old ones are gone.
                # The generator marks that it started, then writes the bindings a moment
                # after the old ones are gone.
                "target/debug/uniffi-bindgen": (
                    '#!/bin/sh\ntouch "started.$$"\nwhile [ "$1" != --out-dir ]; do shift; done\n'
                    'sleep 0.3; mkdir -p "$2"; touch "$2/bindings.kt"\n'
                ),
                # Gradle gives the other task up to two seconds to start generating, then
                # reads the bindings it was given.
                "jvm/gradlew": (
                    "#!/bin/sh\nfor i in $(seq 20); do\n"
                    "  [ \"$(ls ../started.* 2> /dev/null | wc -l)\" -ge 2 ] && break\n  sleep 0.1\ndone\n"
                    "test -e ../target/jvm/uniffi/bindings.kt || exit 41\n"
                ),
            }
            for name, content in tools.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(content)
                path.chmod(0o755)
            (root / "target/release").mkdir(parents=True)
            (root / "target/release/libsparkles_ffi.so").touch()
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
            env.pop("SPARKLES_JVM_LOCKED", None)
            if not locked:
                # Each task believes a caller already holds the lock.
                env["SPARKLES_JVM_LOCKED"] = "1"
            runs = []
            for n in range(2):
                runs.append(subprocess.Popen(
                    ["bash", "scripts/jvm-gradle.sh", f":task{n}"], cwd=root, env=env,
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                ))
                # The first task's Gradle is running when the second task starts.
                time.sleep(1)
            results = []
            for run in runs:
                _, stderr = run.communicate(timeout=30)
                results.append((run.returncode, stderr))
            return results

    def test_concurrent_jvm_tasks_do_not_regenerate_bindings_under_each_other(self):
        for code, stderr in self.run_two_jvm_tasks(locked=True):
            self.assertEqual(code, 0, stderr)

    def test_unlocked_jvm_tasks_lose_bindings_to_each_other(self):
        # The negative control: without the lock, the second task deletes the bindings
        # the first task's Gradle reads.
        self.assertEqual(self.run_two_jvm_tasks(locked=False)[0][0], 41)


class ExplicitTargetTasks(unittest.TestCase):
    def test_every_explicit_root_target_cargo_call_holds_the_task_lease(self):
        tasks = tomllib.loads((ROOT / "mise.toml").read_text())["tasks"]
        calls = []
        for name, task in tasks.items():
            run = task.get("run", [])
            for line in "\n".join(run if isinstance(run, list) else [run]).splitlines():
                if "--target-dir target" in line and line.lstrip().startswith("mbx "):
                    calls.append((name, line.strip()))
        self.assertTrue(calls)
        for name, line in calls:
            with self.subTest(task=name):
                self.assertTrue(line.startswith("mbx sparkles-task -- "), line)


class ArtifactRetentionContracts(unittest.TestCase):
    def test_ci_evidence_survives_compiler_target_collection(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-ci-artifacts-") as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            (root / "target").mkdir()
            script = root / "scripts/ci.sh"
            (root / "scripts/ci-retain-junit.py").write_bytes((ROOT / "scripts/ci-retain-junit.py").read_bytes())
            script.write_bytes((ROOT / "scripts/ci.sh").read_bytes())
            frontend = root / "bin/mise"
            frontend.write_text('#!/bin/sh\nprintf "retained-ci-output\\n"\n')
            frontend.chmod(0o755)
            leaser = root / "bin/mbx"
            leaser.write_text(
                '#!/bin/sh\n[ "$1" = sparkles-task ] || exit 23\nshift\n[ "$1" != -- ] || shift\n'
                'touch "$LEASE_MARKER"\nexec "$@"\n'
            )
            leaser.chmod(0o755)
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                       LEASE_MARKER=str(root / "leased"))
            env.pop("SPARKLES_CI_LEASED", None)
            result = subprocess.run(["bash", str(script), "lint:doc-paths"], cwd=root,
                                    env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((root / "leased").exists(), "ci.sh ran without the target lease")
            summaries = list(root.glob("**/ci/summary.txt"))
            self.assertEqual(len(summaries), 1, result.stdout)
            summary = summaries[0]
            self.assertRegex(summary.read_text(), r"lint:doc-paths\s+pass \d+s")
            logs = list(summary.parent.glob("logs/*.log"))
            self.assertEqual(len(logs), 1)
            self.assertIn("retained-ci-output", logs[0].read_text())
            shutil.rmtree(root / "target")
            self.assertTrue(summary.exists(), "compiler collection deleted validation evidence")
            self.assertTrue(logs[0].exists(), "compiler collection deleted validation log")


if __name__ == "__main__":
    unittest.main()
