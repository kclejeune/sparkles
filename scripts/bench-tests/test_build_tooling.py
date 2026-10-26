"""Real Boxington contracts for mise task caching and managed-target consumers."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
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


class ArtifactRetentionContracts(unittest.TestCase):
    def test_ci_evidence_survives_compiler_target_collection(self):
        with tempfile.TemporaryDirectory(prefix="sparkles-ci-artifacts-") as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            (root / "target").mkdir()
            script = root / "scripts/ci.sh"
            script.write_bytes((ROOT / "scripts/ci.sh").read_bytes())
            frontend = root / "bin/mise"
            frontend.write_text('#!/bin/sh\nprintf "retained-ci-output\\n"\n')
            frontend.chmod(0o755)
            env = dict(os.environ, PATH=str(root / "bin") + os.pathsep + os.environ["PATH"])
            result = subprocess.run(["bash", str(script), "lint:doc-paths"], cwd=root,
                                    env=env, capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stderr)
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
