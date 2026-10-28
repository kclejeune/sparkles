"""No-build contracts for the hosted hybrid dependency cache and mbx launcher repair."""
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("cache_rehydrate", ROOT / "scripts/ci-cache-rehydrate.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)
IDENTITY = "a" * 64
HYBRID = "github.event_name != 'pull_request' || github.event.pull_request.head.repo.full_name == github.repository"
SAVE = "${{ github.ref == 'refs/heads/main' && (github.event_name == 'push' || github.event_name == 'workflow_dispatch') }}"


class RestoredBuildScripts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sparkles-ci-hybrid-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = self.root / "target"
        self.binary = self.root / "installed-mbx"
        self.binary.write_bytes(b"verified installed mbx fixture")
        self.helper = self.target / "debug/.mbx-build-script-shims" / IDENTITY / "mbx"

    def launcher(self, package="example-123", content=None):
        path = self.target / "debug/build" / package / "build_script_build"
        path.parent.mkdir(parents=True, exist_ok=True)
        real = path.with_name(path.name + ".mbx-real")
        real.write_bytes(b"dependency build script ELF fixture")
        real.chmod(0o755)
        if content is None:
            content = (
                '#!/bin/sh\nMBX_BUILD_SCRIPT_SHIM_PATH="$0" exec '
                f'"$(dirname "$0")/../../.mbx-build-script-shims/{IDENTITY}/mbx" "$@"\n'
            ).encode()
        path.write_bytes(content)
        path.chmod(0o755)
        return path

    def repair(self):
        return MODULE.repair_launchers(self.target, self.binary)

    def test_pruned_shared_helper_is_executable_and_launcher_and_real_mtimes_survive(self):
        first = self.launcher()
        second = self.launcher("other-456")
        paths = [first, second, first.with_name(first.name + ".mbx-real")]
        for path in paths:
            os.utime(path, ns=(1_600_000_000_000_000_000, 1_600_000_000_000_000_000))
        before = [(path.read_bytes(), path.stat().st_mtime_ns) for path in paths]
        self.assertEqual(self.repair(), {"launchers": 2, "helpers": 1})
        self.assertEqual(self.helper.read_bytes(), self.binary.read_bytes())
        self.assertTrue(os.access(self.helper, os.X_OK))
        self.assertEqual(self.repair(), {"launchers": 2, "helpers": 1})
        self.assertEqual(before, [(p.read_bytes(), p.stat().st_mtime_ns) for p in paths])

    def test_existing_helper_hardlink_is_replaced_without_modifying_its_other_name(self):
        self.launcher()
        self.helper.parent.mkdir(parents=True)
        other = self.root / "other-store-entry"
        other.write_bytes(b"old helper")
        os.link(other, self.helper)
        self.repair()
        self.assertEqual(other.read_bytes(), b"old helper")
        self.assertEqual(self.helper.read_bytes(), self.binary.read_bytes())

    def test_unknown_forms_fail_before_any_copy_and_never_execute_cached_text(self):
        marker = self.root / "must-not-exist"
        valid = self.launcher()
        supported = valid.read_bytes()
        bad_forms = [
            b'#!/bin/sh\nexec /gone/mbx "$@"\n',
            supported + f"touch {marker}\n".encode(),
            supported.replace(b"../../", b"../../../"),
            supported.replace(IDENTITY.encode(), b"not-a-hash"),
            b"\xff", b"x" * 4097,
            supported.replace(b"\n", b"\r\n"),
        ]
        bad = self.launcher("unsupported-789")
        for content in bad_forms:
            with self.subTest(content=content[:80]):
                bad.write_bytes(content)
                with self.assertRaisesRegex(ValueError, "Unsupported"):
                    self.repair()
                self.assertFalse(self.helper.exists())
                self.assertFalse(marker.exists())

    def test_missing_executable_pair_is_refused(self):
        path = self.launcher()
        path.unlink()
        with self.assertRaisesRegex(ValueError, "Missing"):
            self.repair()
        self.assertFalse(self.helper.exists())

    def test_supported_orphan_launcher_is_refused_for_all_cargo_names(self):
        for name in ("build_script_build", "build-script-build", "build_script_build-0123456789abcdef",
                     "build-script-build-0123456789abcdef", "build_script_custom_build"):
            with self.subTest(name=name):
                path = self.launcher()
                path.with_name(path.name + ".mbx-real").unlink()
                orphan = path.with_name(name)
                path.rename(orphan)
                with self.assertRaisesRegex(ValueError, "Missing"):
                    self.repair()
                self.assertFalse(self.helper.exists())
                orphan.unlink()

    def test_malformed_orphan_refuses_mixed_target_before_publishing_any_helper(self):
        # The valid pair sorts first, so publication inside validation would fail.
        self.launcher("aaa-valid-123")
        orphan = self.launcher("zzz-orphan-456")
        orphan.with_name(orphan.name + ".mbx-real").unlink()
        marker = self.root / "must-not-exist"
        for content in (f'#!/bin/sh\ntouch {marker}\n'.encode(), b"\xff", b"x" * 4097):
            with self.subTest(content=content[:80]):
                orphan.write_bytes(content)
                with self.assertRaises(ValueError):
                    self.repair()
                self.assertFalse(self.helper.exists())
                self.assertFalse(marker.exists())

    def test_orphan_launcher_symlinks_are_refused_without_following_them(self):
        path = self.launcher()
        path.with_name(path.name + ".mbx-real").unlink()
        path.unlink()
        for destination in (self.binary, self.root / "absent", self.root):
            with self.subTest(destination=destination):
                path.symlink_to(destination)
                try:
                    with self.assertRaisesRegex(ValueError, "symlink"):
                        self.repair()
                    self.assertFalse(self.helper.exists())
                finally:
                    path.unlink()

    def test_directory_at_cargo_executable_path_is_refused(self):
        path = self.launcher()
        path.with_name(path.name + ".mbx-real").unlink()
        path.unlink()
        path.mkdir()
        with self.assertRaisesRegex(ValueError, "executable"):
            self.repair()
        self.assertFalse(self.helper.exists())

    def test_native_elf_without_private_pair_and_nonexecutable_outputs_are_valid(self):
        path = self.launcher()
        path.with_name(path.name + ".mbx-real").unlink()
        path.write_bytes(b"\x7fELF" + b"native executable fixture" * 4096)
        for name in ("build_script_build-0123456789abcdef.d", "build_script_build.mbx-action", "output", "stderr"):
            path.with_name(name).write_bytes(b"not an mbx launcher")
        output = path.parent / "out/build_script_build"
        output.parent.mkdir()
        output.write_bytes(b"generated output, not a Cargo launcher")
        before = (path.read_bytes(), path.stat().st_mtime_ns)
        self.assertEqual(self.repair(), {"launchers": 0, "helpers": 0})
        self.assertEqual(before, (path.read_bytes(), path.stat().st_mtime_ns))
        self.assertFalse(self.helper.exists())

    def test_candidate_reads_are_bounded_for_native_and_mbx_executables(self):
        class BoundedReader(io.BytesIO):
            def read(self, size=-1):
                if not 0 <= size <= 4097:
                    raise AssertionError(f"Unbounded cached executable read: {size}")
                self.requests.append(size)
                return super().read(size)

        for native in (False, True):
            with self.subTest(native=native):
                path = self.launcher()
                if native:
                    path.with_name(path.name + ".mbx-real").unlink()
                    path.write_bytes(b"\x7fELF" + b"x" * 8192)
                stream = BoundedReader(path.read_bytes())
                stream.requests = []
                with patch.object(Path, "open", return_value=stream):
                    self.repair()
                self.assertEqual(stream.requests, [4097])

    def test_executable_symlinks_are_refused(self):
        for which in ("launcher", "real"):
            with self.subTest(which=which):
                path = self.launcher()
                path = path if which == "launcher" else path.with_name(path.name + ".mbx-real")
                content = path.read_bytes()
                path.unlink()
                path.symlink_to(self.binary)
                with self.assertRaisesRegex(ValueError, "symlink"):
                    self.repair()
                path.unlink()
                path.write_bytes(content)
        self.assertFalse(self.helper.exists())

    def test_helper_symlink_escape_is_refused_without_writing_outside_target(self):
        self.launcher()
        outside = self.root / "outside"
        outside.mkdir()
        self.helper.parent.parent.mkdir()
        self.helper.parent.symlink_to(outside, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "escapes"):
            self.repair()
        self.assertEqual(list(outside.iterdir()), [])

    def test_internal_helper_symlink_is_also_refused(self):
        self.launcher()
        elsewhere = self.target / "internal"
        elsewhere.mkdir()
        self.helper.parent.parent.mkdir()
        self.helper.parent.symlink_to(elsewhere, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.repair()
        self.assertEqual(list(elsewhere.iterdir()), [])

    def test_symlinked_build_directory_is_refused(self):
        build = self.target / "debug/build"
        build.parent.mkdir(parents=True)
        build.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "symlink"):
            self.repair()

    def test_managed_view_and_nondirectory_target_are_refused(self):
        actual = self.root / "managed"
        actual.mkdir()
        self.target.symlink_to(actual, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "ordinary target"):
            self.repair()
        self.target.unlink()
        self.target.touch()
        with self.assertRaisesRegex(ValueError, "ordinary target"):
            self.repair()

    def test_cold_miss_and_empty_target_are_valid(self):
        self.assertEqual(self.repair(), {"launchers": 0, "helpers": 0})
        self.assertFalse(self.target.exists())
        self.target.mkdir()
        self.assertEqual(self.repair(), {"launchers": 0, "helpers": 0})

    def test_cli_gates_installed_version_and_propagates_probe_failures(self):
        self.launcher()
        binary = self.root / "mbx"
        env = dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ["PATH"])
        for body in ("echo 'mbx 1.29.0'", "exit 17"):
            binary.write_text("#!/bin/sh\n" + body + "\n")
            binary.chmod(0o755)
            failed = subprocess.run([sys.executable, str(ROOT / "scripts/ci-cache-rehydrate.py")],
                                    cwd=self.root, env=env, capture_output=True, text=True)
            self.assertNotEqual(failed.returncode, 0)
            self.assertFalse(self.helper.exists())
        binary.write_text("#!/bin/sh\necho 'mbx 1.28.0'\n")
        subprocess.run([sys.executable, str(ROOT / "scripts/ci-cache-rehydrate.py")],
                       cwd=self.root, env=env, capture_output=True, text=True, check=True)
        self.assertEqual(self.helper.read_bytes(), binary.read_bytes())


class CacheToolFormatter(unittest.TestCase):
    def test_metadata_uses_mbx_probes_keep_their_tools_and_failures_propagate(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            for tool in ("mbx", "rustc", "rustup"):
                path = temp / tool
                path.write_text(f'#!/bin/sh\nprintf "%s\\n" "{tool}" "$@"\nexit "${{FAKE_EXIT:-0}}"\n')
                path.chmod(0o755)
            env = dict(os.environ, PATH=str(temp) + os.pathsep + os.environ["PATH"])
            formatter = ROOT / "scripts/ci-cache-tool.sh"
            for tool, args in (("cargo", ["metadata", "--format-version", "1"]),
                               ("rustc", ["-vV"]), ("rustup", ["run", "stable", "rustc", "-vV"])):
                result = subprocess.check_output([formatter, tool, *args], env=env, text=True)
                self.assertEqual(result.splitlines(), ["mbx", *args, "--locked"] if tool == "cargo" else [tool, *args])
                failure = subprocess.run([formatter, tool, *args], env=dict(env, FAKE_EXIT="23"), capture_output=True)
                self.assertEqual(failure.returncode, 23)
            marker = temp / "not-executed"
            unsupported = subprocess.run([formatter, "sh", "-c", f"touch {marker}"], env=env, capture_output=True)
            self.assertEqual(unsupported.returncode, 64)
            self.assertFalse(marker.exists())


@unittest.skipUnless(shutil.which("yq"), "yq is required for workflow contracts")
class WorkflowContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = json.loads(subprocess.check_output([
            "yq", "-o=json", ".", str(ROOT / ".github/workflows/ci.yml"),
        ], text=True))
        cls.build = cls.workflow["jobs"]["build"]

    def test_hybrid_only_build_job_retains_fork_remote_and_credential_policy(self):
        steps = self.build["steps"]
        hybrid = [s for s in steps if s.get("name") in (
            "Configure hybrid target ownership", "Restore dependency target cache", "Rehydrate cached build-script helpers")]
        self.assertEqual(len(hybrid), 3)
        self.assertTrue(all(s["if"] == HYBRID for s in hybrid))
        self.assertNotIn("MBX_TARGET_VIEWS", self.workflow["env"])
        self.assertNotIn("MBX_TARGET_VIEWS", self.build["env"])
        self.assertEqual(hybrid[0]["run"], 'echo "MBX_TARGET_VIEWS=0" >> "$GITHUB_ENV"')
        for name, job in self.workflow["jobs"].items():
            if name != "build":
                self.assertFalse(any("Swatinem/" in s.get("uses", "") for s in job.get("steps", [])))
        mbx = next(s["with"] for s in steps if s.get("uses") == "jdx/mr-boxington-action@v1.7.3")
        self.assertIn("&& 'remote' || 'github'", mbx["backend"])
        self.assertEqual(mbx["namespace"], "kclejeune/sparkles")
        self.assertEqual(mbx["remote-url"], "https://mbx.kclj.dev")
        self.assertEqual(mbx["oidc-audience"], "https://mbx.kclj.dev")
        self.assertEqual(mbx["version"], MODULE.MBX_VERSION)
        self.assertEqual(self.build["permissions"], {"contents": "read", "id-token": "write"})
        self.assertEqual(self.workflow["env"]["MBX_REMOTE_TRUST_WRITES"],
                         "${{ github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && 'true' || 'false' }}")

    def test_stable_target_key_excludes_object_store_and_only_main_saves(self):
        cache = next(s for s in self.build["steps"] if s.get("name") == "Restore dependency target cache")
        self.assertEqual(cache["uses"], "Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0")
        config = cache["with"]
        self.assertEqual(config["prefix-key"], "sparkles-ci-tests-v1")
        self.assertEqual(config["shared-key"], "mbx-" + MODULE.MBX_VERSION)
        self.assertNotIn("github.run", json.dumps(config))
        self.assertNotIn("add-rust-environment-hash-key", config)
        self.assertEqual(config["workspaces"], ". -> target")
        self.assertTrue(config["cache-targets"])
        for key in ("cache-workspace-crates", "cache-all-crates", "cache-bin"):
            self.assertFalse(config[key])
        self.assertNotIn("cache-directories", config)
        self.assertEqual(config["save-if"], SAVE)
        self.assertEqual(config["cmd-format"], "bash scripts/ci-cache-tool.sh {0}")
        self.assertNotIn("MBX_CACHE_DIR", self.workflow["env"])
        self.assertNotIn("MBX_CACHE_DIR", self.build["env"])
        self.assertNotIn("CARGO_TARGET_DIR", self.build["env"])

    def test_hydration_and_fresh_metadata_precede_unchanged_archive_and_doctests(self):
        steps = self.build["steps"]
        by_name = {s.get("name"): s for s in steps}
        hydrate = by_name["Rehydrate cached build-script helpers"]
        self.assertEqual(hydrate["run"].splitlines(), [
            "python3 scripts/ci-cache-rehydrate.py", "mbx metadata --locked --format-version 1 > /dev/null"])
        self.assertLess(steps.index(by_name["Restore dependency target cache"]), steps.index(hydrate))
        self.assertLess(steps.index(hydrate), steps.index(by_name["Build the test archive"]))
        self.assertEqual(by_name["Build the test archive"]["run"].splitlines(), [
            'mkdir -p "$RUNNER_TEMP/archive"',
            'mbx nextest archive --locked --workspace --zstd-level -7 --archive-file "$RUNNER_TEMP/raw.tar.zst"',
            'zstd -q -d -c "$RUNNER_TEMP/raw.tar.zst" | zstd -q -6 -T0 --long=27 -o "$RUNNER_TEMP/archive/tests.tar.zst"',
            'rm "$RUNNER_TEMP/raw.tar.zst"',
            'ls -l "$RUNNER_TEMP/archive/tests.tar.zst"',
            'tar -cf "$RUNNER_TEMP/archive/ui-build.tar" ui/build',
        ])
        self.assertEqual(by_name["Doctests"]["run"], "mbx test --locked --workspace --doc")
        upload = next(s["with"] for s in steps if s.get("uses") == "actions/upload-artifact@v4")
        self.assertEqual(upload, {"name": "nextest-archive", "path": "${{ runner.temp }}/archive",
                                  "retention-days": 1, "compression-level": 0})
        self.assertEqual(self.workflow["jobs"]["test"]["needs"], "build")
        self.assertEqual(self.workflow["jobs"]["test"]["strategy"]["matrix"]["shard"], ["1/4", "2/4", "3/4", "4/4", "timing"])
        self.assertEqual(self.workflow["jobs"]["jena-clients"]["needs"], "build")
        self.assertEqual(self.build["env"]["MBX_GC_MAX_SIZE"], "30GiB")
        self.assertEqual(self.build["env"]["MBX_GC_MIN_FREE_SIZE"], "10GiB")


@unittest.skipUnless(os.environ.get("ACTIONLINT") or shutil.which("actionlint"), "actionlint is required for semantic workflow checks")
class WorkflowSemantics(unittest.TestCase):
    def test_official_actionlint_accepts_workflow_and_rejects_invalid_context(self):
        actionlint = os.environ.get("ACTIONLINT") or shutil.which("actionlint")
        path = ROOT / ".github/workflows/ci.yml"
        good = subprocess.run([actionlint, str(path)], cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(good.returncode, 0, good.stdout + good.stderr)
        bad = path.read_text().replace("      MBX_GC_MAX_SIZE: 30GiB\n", "      MBX_GC_MAX_SIZE: ${{ runner.temp }}\n", 1)
        self.assertNotEqual(bad, path.read_text())
        result = subprocess.run([actionlint, "-"], input=bad, cwd=ROOT, capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('context "runner" is not allowed here', result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
