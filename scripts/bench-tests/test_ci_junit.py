#!/usr/bin/env python3
"""Inert retention checks; fake tools never invoke Cargo or MBX builds."""

import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from hashlib import sha256
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


class CiJunitIntegration(unittest.TestCase):
    def run_ci(self, **options):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.prepare_ci_fixture(root)
            return self.run_ci_fixture(root, **options)

    def prepare_ci_fixture(self, root, *, real_mise=False):
        (root / "scripts").mkdir()
        (root / "bin").mkdir()
        (root / "global.toml").touch()
        for name in ("ci.sh", "ci-retain-junit.py", "nextest-slow.py"):
            shutil.copyfile(ROOT / "scripts" / name, root / "scripts" / name)
        config = (ROOT / "mise.toml").read_text()
        test_task = config.split("\n[tasks.test]\n", 1)[1].split("\n[tasks.", 1)[0]
        (root / "mise.toml").write_text(
            '[settings]\nexperimental = true\n'
            '[task_config]\nglobal_env = ["SPARKLES_CHANGED_PACKAGES"]\n'
            '[task_config.input_groups]\n'
            'rust = ["scripts/*.py"]\nui = []\ndocs = []\n'
            '[tasks."ui:build"]\nrun = "true"\n'
            '[tasks.test]\n' + test_task
        )
        (root / "scripts/changed-crates.py").write_text(
            """import os, sys
selection = os.environ["FIXTURE_SELECTION"]
if sys.argv[1:] == ["fixture-base"]:
    print(selection)
elif sys.argv[1:] == ["--cargo-args", "--doc"] and selection == "sparkles-server":
    print("none")
elif sys.argv[1:2] == ["--cargo-args"]:
    print({"all": "--workspace", "none": "none"}.get(selection, f"-p {selection}"))
else:
    sys.exit("unexpected changed-crates arguments")
"""
        )
        tools = {
            "git": '#!/bin/sh\ncase "$1" in rev-parse) echo "$FIXTURE_COMMIT";; status) :;; *) exit 1;; esac\n',
            "rustc": "#!/bin/sh\n[ \"$*\" = -V ] && echo 'fixture rustc'\n",
            "mbx-policy-key.py": "#!/bin/sh\necho 'fixture policy'\n",
            "mbx": """#!/usr/bin/env python3
import os, pathlib, shutil, subprocess, sys
root = pathlib.Path.cwd()
args = sys.argv[1:]
if args == ["--version"]:
    print("fixture mbx")
elif args[:2] == ["sparkles-task", "--"]:
    lease = root / "lease-live"
    lease.touch()
    try:
        sys.exit(subprocess.call(args[2:]))
    finally:
        shutil.rmtree(root / "cache")
        (root / "target").unlink()
        lease.unlink()
elif args[:2] == ["nextest", "run"]:
    assert (root / "lease-live").exists()
    if os.environ["MISSING"] == "0":
        for profile in os.environ["PROFILES"].split(","):
            p = root / "target/nextest" / profile / "junit.xml"
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(
                f'<testsuites fixture="{profile}"><testsuite failures="{os.environ["FAIL"]}"/></testsuites>\\n'
            )
    if os.environ["COPY_ERROR"] == "1":
        (root / ".artifacts/ci/junit/.staging").mkdir()
    sys.exit(29 if os.environ["FAIL"] == "1" else 0)
elif args[0] == "test":
    assert (root / "lease-live").exists()
    if "--doc" in args:
        assert os.environ["FIXTURE_SELECTION"] not in ("sparkles-server", "none")
    elif args[:4] != ["test", "--locked", "-p", "sparkles-backup"]:
        assert args == ["test", "--workspace", "--no-run", "--bin", "sparkles"]
else:
    sys.exit("unexpected mbx arguments")
""",
        }
        if not real_mise:
            tools["mise"] = """#!/usr/bin/env python3
import os, pathlib, subprocess, sys, tomllib
root = pathlib.Path.cwd()
task = sys.argv[-1]
if task == "test":
    assert (root / "lease-live").exists()
    if "REPLAYED_LOG" in os.environ:
        print(os.environ["REPLAYED_LOG"], end="")
        sys.exit(0)
    config = tomllib.loads((root / "mise.toml").read_text())
    for script in config["tasks"]["test"]["run"]:
        result = subprocess.run(["bash", "-c", script], check=False)
        if result.returncode:
            sys.exit(result.returncode)
elif task == "node:test":
    (root / "target/nextest/default/junit.xml").write_text("overwritten by later task")
"""
        for name, script in tools.items():
            path = root / "bin" / name
            path.write_text(script)
            path.chmod(0o755)

    def run_ci_fixture(
        self,
        root,
        *,
        failing=False,
        missing=False,
        profiles=("default",),
        later=False,
        copy_error=False,
        gate_run="",
        replayed_log=None,
        preexisting=False,
        selection="all",
    ):
        target = root / "cache/targets/v1/key"
        target.mkdir(parents=True)
        (root / "target").symlink_to(target)
        target.with_suffix(".json").write_text('{"workspace_root":"fixture"}\n')
        if preexisting:
            report = target / "nextest/default/junit.xml"
            report.parent.mkdir(parents=True)
            report.write_bytes(b"<testsuites/>\n")
        env = dict(
            os.environ,
            PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
            FAIL="1" if failing else "0",
            MISSING="1" if missing else "0",
            PROFILES=",".join(profiles),
            COPY_ERROR="1" if copy_error else "0",
            FIXTURE_COMMIT=("2" if replayed_log is not None else "1") * 40,
            FIXTURE_SELECTION=selection,
            SPARKLES_CI_GATE_RUN=gate_run,
            MISE_CACHE_DIR=str(root / "mise-cache"),
            MISE_GLOBAL_CONFIG_FILE=str(root / "global.toml"),
            MISE_TRUSTED_CONFIG_PATHS=str(root),
            MISE_TASK_CACHE="read-write",
        )
        if replayed_log is not None:
            env["REPLAYED_LOG"] = replayed_log
        env.pop("SPARKLES_CI_LEASED", None)
        tasks = ["test", "node:test"] if later else ["test"]
        result = subprocess.run(
            ["bash", "scripts/ci.sh", "--jobs", "1", "--changed", "fixture-base", *tasks],
            check=False,
            cwd=root,
            env=env,
            capture_output=True,
            text=True,
            timeout=20,
        )
        out = root / ".artifacts/ci"
        receipt = json.loads((out / "junit/manifest.json").read_text())
        contents = {
            str(p.relative_to(out)): p.read_bytes()
            for p in out.rglob("*")
            if p.is_file()
        }
        self.assertFalse(
            target.exists(), "fixture lease did not collect its original target"
        )
        return result, receipt, contents

    def test_success_survives_managed_target_collection(self):
        result, receipt, files = self.run_ci()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["task_exit"], 0)
        self.assertEqual(receipt["source_commit"], "1" * 40)
        self.assertEqual(receipt["status"], "retained")
        self.assertEqual(
            files["junit/test/default/junit.xml"],
            b'<testsuites fixture="default"><testsuite failures="0"/></testsuites>\n',
        )
        self.assertEqual(
            files["junit/target-metadata.json"], b'{"workspace_root":"fixture"}\n'
        )

    def test_failed_command_retains_actual_failure_and_original_exit(self):
        result, receipt, files = self.run_ci(failing=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(receipt["task_exit"], 29)
        self.assertIn(b'failures="1"', files["junit/test/default/junit.xml"])
        self.assertIn(b"exit 29", files["state/test"])

    def test_missing_report_is_not_fabricated_or_task_failure(self):
        result, receipt, files = self.run_ci(missing=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "missing")
        self.assertEqual(receipt["reports"], [])
        self.assertFalse(any(p.endswith(".xml") for p in files))

    def test_multiple_profiles_are_separate(self):
        result, receipt, files = self.run_ci(profiles=("default", "ci"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual({p["profile"] for p in receipt["reports"]}, {"default", "ci"})
        self.assertIn(b'fixture="default"', files["junit/test/default/junit.xml"])
        self.assertIn(b'fixture="ci"', files["junit/test/ci/junit.xml"])

    def test_later_serialized_task_cannot_replace_primary_copy(self):
        result, receipt, files = self.run_ci(later=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "retained")
        self.assertIn(b"<testsuites fixture=", files["junit/test/default/junit.xml"])

    def test_copy_failure_preserves_zero_and_nonzero_test_status(self):
        for failing in (False, True):
            with self.subTest(failing=failing):
                result, receipt, files = self.run_ci(failing=failing, copy_error=True)
                self.assertEqual(result.returncode, 1 if failing else 0)
                self.assertEqual(receipt["task_exit"], 29 if failing else 0)
                self.assertEqual(receipt["status"], "error")
                self.assertFalse(any(p.endswith(".xml") for p in files))


class RetentionDataControls(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location(
            "ci_retention", ROOT / "scripts/ci-retain-junit.py"
        )
        self.helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.helper)
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.out = self.root / "out"
        args = type("Args", (), {"base": "base", "packages": "all", "selected": True})()
        with mock.patch.object(
            self.helper.subprocess, "check_output", side_effect=["a" * 40, ""]
        ):
            self.helper.init(self.root, self.out, args)

    def report(self, profile="default", data=b"<testsuites/>\n"):
        p = self.root / "target/nextest" / profile / "junit.xml"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)
        return p

    def manifest(self):
        return json.loads((self.out / "manifest.json").read_text())

    def rewrite(self, value):
        (self.out / "manifest.json").write_text(json.dumps(value))

    def capture(self):
        self.helper.before(self.root, self.out)
        self.report()
        self.helper.after(self.root, self.out, 0)
        self.helper.verify(self.out)

    def test_unchanged_report_has_unproven_generation(self):
        self.report()
        self.helper.before(self.root, self.out)
        self.helper.after(self.root, self.out, 29)
        value = self.manifest()
        self.assertEqual(
            value["reports"][0]["freshness"], "unchanged-generation-unproven"
        )
        self.assertEqual(value["task_exit"], 29)
        self.helper.verify(self.out)

    def test_new_report_is_distinct_from_unchanged(self):
        self.capture()
        self.assertEqual(
            self.manifest()["reports"][0]["freshness"],
            "new-or-changed-since-task-start",
        )

    def test_missing_report_refuses_verification(self):
        self.helper.before(self.root, self.out)
        self.helper.after(self.root, self.out, 0)
        self.assertEqual(self.manifest()["status"], "missing")
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)
        self.assertEqual(list(self.out.rglob("*.xml")), [])

    def test_not_selected_unstarted_and_no_packages_are_distinct(self):
        self.helper.verify(self.out)
        self.assertEqual(self.manifest()["status"], "not-run")
        value = self.manifest()
        value.update(status="not-selected", selected=False)
        self.rewrite(value)
        self.helper.verify(self.out)
        value.update(status="running", selected=True, selection_packages="none")
        self.rewrite(value)
        self.helper.before(self.root, self.out)
        self.helper.after(self.root, self.out, 0)
        self.assertEqual(self.manifest()["status"], "not-applicable")
        self.helper.verify(self.out)

    def test_unknown_version_and_inconsistent_state_refuse(self):
        value = self.manifest()
        value["version"] = 3
        self.rewrite(value)
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)
        value.update(version=1, status="not-selected", selected=True)
        self.rewrite(value)
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)

    def test_altered_or_missing_retained_report_refuses(self):
        self.capture()
        report = self.out / "test/default/junit.xml"
        report.write_bytes(b"wrong")
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)
        report.unlink()
        with self.assertRaises(OSError):
            self.helper.verify(self.out)

    def test_metadata_tampering_and_duplicate_profile_refuse(self):
        metadata = self.root / "target.json"
        metadata.write_bytes(b'{"workspace_root":"actual fixture"}\n')
        self.capture()
        (self.out / "target-metadata.json").write_bytes(b"changed")
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)
        value = self.manifest()
        value["reports"].append(value["reports"][0])
        self.rewrite(value)
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)

    def test_source_report_and_profile_symlinks_refuse(self):
        report = self.report()
        data = self.root / "source.xml"
        report.rename(data)
        report.symlink_to(data)
        with self.assertRaises(OSError):
            self.helper.before(self.root, self.out)
        report.unlink()
        report.parent.rmdir()
        report.parent.symlink_to(self.root)
        with self.assertRaises(ValueError):
            self.helper.before(self.root, self.out)

    def test_unmanaged_target_missing_metadata_is_explicit(self):
        self.capture()
        value = self.manifest()
        self.assertEqual(value["target"]["target_kind"], "unmanaged")
        self.assertEqual(value["metadata_status"], "absent")
        self.assertIsNone(value["metadata"])

    def test_unsafe_profile_and_retained_path_refuse(self):
        self.report("unsafe.profile")
        with self.assertRaises(ValueError):
            self.helper.before(self.root, self.out)
        shutil.rmtree(self.root / "target")
        self.capture()
        value = self.manifest()
        value["reports"][0]["retained_path"] = "../outside"
        self.rewrite(value)
        with self.assertRaises(ValueError):
            self.helper.verify(self.out)

    def test_changed_during_read_refuses(self):
        report = self.report()
        actual = self.helper.os.fstat
        count = 0

        def changing(fd):
            nonlocal count
            value = actual(fd)
            count += 1
            if count == 2:
                return type(
                    "Changed",
                    (),
                    {
                        "st_dev": value.st_dev,
                        "st_ino": value.st_ino,
                        "st_size": value.st_size + 1,
                        "st_mtime_ns": value.st_mtime_ns,
                        "st_ctime_ns": value.st_ctime_ns,
                    },
                )()
            return value

        with (
            mock.patch.object(self.helper.os, "fstat", side_effect=changing),
            self.assertRaises(ValueError),
        ):
            self.helper.read_regular(report)


class ReaderContractControls(unittest.TestCase):
    setUp = RetentionDataControls.setUp
    report = RetentionDataControls.report
    manifest = RetentionDataControls.manifest
    rewrite = RetentionDataControls.rewrite
    capture = RetentionDataControls.capture

    def test_boolean_and_unknown_versions_refuse(self):
        self.capture()
        original = self.manifest()
        for version in (True, 3):
            with self.subTest(version=version):
                value = dict(original, version=version)
                self.rewrite(value)
                with self.assertRaises(ValueError):
                    self.helper.verify(self.out)

    def test_metadata_path_cannot_escape_retention_root(self):
        self.capture()
        outside = self.root / "outside.json"
        outside.write_bytes(b"actual outside bytes")
        _, identity = self.helper.read_regular(outside)
        value = self.manifest()
        value["metadata"] = dict(
            identity, source_path=str(outside), retained_path="test/../../outside.json"
        )
        self.rewrite(value)
        read_regular = self.helper.read_regular

        def manifest_only(path):
            self.assertEqual(path, self.out / "manifest.json")
            return read_regular(path)

        with (
            mock.patch.object(
                self.helper,
                "read_regular",
                side_effect=manifest_only,
            ),
            self.assertRaises(ValueError),
        ):
            self.helper.verify(self.out)

    def test_every_report_provenance_field_is_required(self):
        self.capture()
        original = self.manifest()
        for field in (
            "source_path",
            "device",
            "inode",
            "size",
            "mtime_ns",
            "ctime_ns",
            "sha256",
            "profile",
            "retained_path",
            "freshness",
        ):
            with self.subTest(field=field):
                value = json.loads(json.dumps(original))
                del value["reports"][0][field]
                self.rewrite(value)
                with self.assertRaises((KeyError, ValueError, TypeError)):
                    self.helper.verify(self.out)

    def test_report_provenance_types_and_freshness_refuse(self):
        self.capture()
        original = self.manifest()
        for field, invalid in (
            ("source_path", None),
            ("device", True),
            ("inode", "2"),
            ("size", -1),
            ("mtime_ns", False),
            ("ctime_ns", []),
            ("sha256", "not-a-hash"),
            ("profile", 4),
            ("retained_path", []),
            ("freshness", "fresh"),
        ):
            with self.subTest(field=field):
                value = json.loads(json.dumps(original))
                value["reports"][0][field] = invalid
                self.rewrite(value)
                with self.assertRaises((ValueError, TypeError)):
                    self.helper.verify(self.out)

    def test_every_metadata_provenance_field_and_type_is_checked(self):
        (self.root / "target.json").write_bytes(b"metadata")
        self.capture()
        original = self.manifest()
        for field in (
            "source_path",
            "device",
            "inode",
            "size",
            "mtime_ns",
            "ctime_ns",
            "sha256",
            "retained_path",
        ):
            with self.subTest(missing=field):
                value = json.loads(json.dumps(original))
                del value["metadata"][field]
                self.rewrite(value)
                with self.assertRaises((KeyError, ValueError, TypeError)):
                    self.helper.verify(self.out)
        for field, invalid in (
            ("source_path", None),
            ("device", True),
            ("inode", "2"),
            ("size", -1),
            ("mtime_ns", False),
            ("ctime_ns", []),
            ("sha256", "not-a-hash"),
            ("retained_path", "test/../../outside.json"),
        ):
            with self.subTest(invalid=field):
                value = json.loads(json.dumps(original))
                value["metadata"][field] = invalid
                self.rewrite(value)
                with self.assertRaises((ValueError, TypeError)):
                    self.helper.verify(self.out)


class GateRetentionControls(unittest.TestCase):
    run_ci = CiJunitIntegration.run_ci
    prepare_ci_fixture = CiJunitIntegration.prepare_ci_fixture
    run_ci_fixture = CiJunitIntegration.run_ci_fixture

    def retain(self, root):
        source = (ROOT / "scripts/gate/remote/gate-inner.sh").read_text()
        function = source.split("  retain_ci_receipts() {", 1)[1].split(
            "  step ci-receipts retain_ci_receipts", 1
        )[0]
        script = (
            'set -uo pipefail\nG="$PWD/gate"\nout="$G/out/current"\nretain_ci_receipts() {'
            + function
            + '\nretain_ci_receipts\nrc=$?\necho "$rc" > "$out/ci-receipts.exit"\nexit "$rc"\n'
        )
        return subprocess.run(
            ["bash", "-c", script],
            check=False,
            cwd=root,
            capture_output=True,
            text=True,
            timeout=10,
        )

    def gate_receipts(
        self,
        files,
        source_files=None,
        *,
        archive=False,
        ci_exit=0,
        copy_error=False,
        linked_logs=False,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            shutil.copyfile(
                ROOT / "scripts/ci-retain-junit.py", root / "scripts/ci-retain-junit.py"
            )
            out = root / "gate/out/current"
            out.mkdir(parents=True)
            (out / "ci.exit").write_text(f"{ci_exit}\n")
            if copy_error:
                (out / "ci-receipts").mkdir()
            source = "original.old-123" if archive else "original"
            for target, contents in (
                (root / ".artifacts/ci", files),
                (out.parent / source / "ci-receipts", source_files or {}),
            ):
                for name, data in contents.items():
                    path = target / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_bytes(data)
            if linked_logs:
                logs = root / ".artifacts/ci/logs"
                shutil.rmtree(logs)
                logs.symlink_to(out.parent / source / "ci-receipts/logs")
            result = self.retain(root)
            self.assertEqual((out / "ci.exit").read_text(), f"{ci_exit}\n")
            self.assertEqual(
                (out / "ci-receipts.exit").read_text(), f"{result.returncode}\n"
            )
            return result

    def test_fresh_passing_gate_verifies_only_with_a_report(self):
        for missing in (False, True):
            with self.subTest(missing=missing):
                result, receipt, files = self.run_ci(gate_run="current", missing=missing)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(receipt["status"], "missing" if missing else "retained")
                gate = self.gate_receipts(files)
                self.assertEqual(gate.returncode, int(missing), gate.stderr)
                if missing:
                    self.assertIn("JUnit evidence is missing", gate.stderr)

    def test_fresh_failing_gate_keeps_the_original_report_and_exit(self):
        result, receipt, files = self.run_ci(gate_run="current", failing=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt["status"], "retained")
        self.assertEqual(receipt["task_exit"], 29)
        self.assertIn(b'failures="1"', files["junit/test/default/junit.xml"])
        self.assertNotIn(b"sparkles-gate-junit-v1", files["logs/test.log"])
        gate = self.gate_receipts(files, ci_exit=result.returncode)
        self.assertEqual(gate.returncode, 0, gate.stderr)

    def test_server_only_gate_retains_and_replays_junit_without_doctests(self):
        result, receipt, original = self.run_ci(
            gate_run="original", selection="sparkles-server"
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["selection_packages"], "sparkles-server")
        self.assertEqual(receipt["status"], "retained", receipt)
        gate = self.gate_receipts(original)
        self.assertEqual(gate.returncode, 0, gate.stderr)
        replayed = self.reuse(original, selection="sparkles-server")
        gate = self.gate_receipts(replayed, original)
        self.assertEqual(gate.returncode, 0, gate.stderr)

    @unittest.skipUnless(shutil.which("mise"), "needs mise for actual cache replay")
    def test_real_mise_replays_an_attested_server_only_gate_pass(self):
        with tempfile.TemporaryDirectory() as directory:
            self.check_real_mise_replay(Path(directory))

    def check_real_mise_replay(self, root):
        self.prepare_ci_fixture(root, real_mise=True)
        runs = {}
        for name, gate_run, copy_error in (
            ("standalone", "", False),
            ("capture-error", "first", True),
            ("original", "original", False),
            ("replayed", "current", False),
        ):
            runs[name] = self.run_ci_fixture(
                root,
                gate_run=gate_run,
                copy_error=copy_error,
                selection="sparkles-server",
            )
        result, receipt, standalone = runs["standalone"]
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(receipt["status"], "retained")
        self.assertNotIn(b"sparkles-gate-junit-v1", standalone["logs/test.log"])
        result, receipt, _ = runs["capture-error"]
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertEqual(receipt["status"], "error")
        result, receipt, original = runs["original"]
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(receipt["status"], "retained", original["logs/test.log"])
        manifest_hash = sha256(original["junit/manifest.json"]).hexdigest()
        marker = f"sparkles-gate-junit-v1 original {manifest_hash}".encode()
        result, receipt, replayed = runs["replayed"]
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(receipt["status"], "reused", replayed["logs/test.log"])
        self.assertEqual(
            receipt["reused_from"], {"run": "original", "manifest_sha256": manifest_hash}
        )
        for files in (original, replayed):
            self.assertEqual(
                [
                    line
                    for line in files["logs/test.log"].splitlines()
                    if line.startswith(b"sparkles-gate-junit-v1 ")
                ],
                [marker],
            )
        fresh_gate = self.gate_receipts(original)
        reused_gate = self.gate_receipts(replayed, original)
        self.assertEqual(fresh_gate.returncode, 0, fresh_gate.stderr)
        self.assertEqual(reused_gate.returncode, 0, reused_gate.stderr)
        return runs

    def test_gate_with_no_changed_packages_needs_no_origin(self):
        result, receipt, files = self.run_ci(gate_run="current", selection="none")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "not-applicable")
        self.assertNotIn(b"sparkles-gate-junit-v1", files["logs/test.log"])
        gate = self.gate_receipts(files)
        self.assertEqual(gate.returncode, 0, gate.stderr)

    def test_gate_origin_capture_error_fails_task_and_preserves_diagnostic(self):
        result, receipt, files = self.run_ci(gate_run="current", copy_error=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt["task_exit"], 1)
        self.assertEqual(receipt["status"], "error")
        self.assertIn(".staging", receipt["error"])
        self.assertIn(b"JUnit retention cache-origin:", files["junit-retention.log"])
        self.assertNotIn(b"JUnit retention after:", files["junit-retention.log"])
        self.assertNotIn(b"sparkles-gate-junit-v1", files["logs/test.log"])
        gate = self.gate_receipts(files, ci_exit=result.returncode)
        self.assertEqual(gate.returncode, 1)

    def test_copy_failure_is_not_hidden(self):
        _, _, files = self.run_ci(gate_run="current")
        gate = self.gate_receipts(files, copy_error=True)
        self.assertEqual(gate.returncode, 1)

    def reuse(self, source_files, *, selection="all"):
        result, receipt, files = self.run_ci(
            gate_run="current",
            selection=selection,
            replayed_log=source_files["logs/test.log"].decode(),
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt["status"], "reused")
        self.assertEqual(receipt["task_exit"], 0)
        self.assertEqual(receipt["source_commit"], "2" * 40)
        self.assertEqual(receipt["reused_from"]["run"], "original")
        self.assertFalse(any(name.endswith(".xml") for name in files))
        return files

    def test_reused_gate_verifies_original_and_archived_reports(self):
        result, _, original = self.run_ci(gate_run="original")
        self.assertEqual(result.returncode, 0, result.stderr)
        files = self.reuse(original)
        for archive in (False, True):
            with self.subTest(archive=archive):
                gate = self.gate_receipts(files, original, archive=archive)
                self.assertEqual(gate.returncode, 0, gate.stderr)

    def test_reused_gate_refuses_a_symlinked_log_directory(self):
        _, _, original = self.run_ci(gate_run="original")
        gate = self.gate_receipts(self.reuse(original), original, linked_logs=True)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("test log directory is a symlink", gate.stderr)

    def test_reused_gate_refuses_missing_or_changed_original_evidence(self):
        _, _, original = self.run_ci(gate_run="original")
        files = self.reuse(original)
        for name in (
            "junit/manifest.json",
            "junit/test/default/junit.xml",
            "junit/target-metadata.json",
        ):
            for delete in (False, True):
                with self.subTest(name=name, delete=delete):
                    invalid = dict(original)
                    if delete:
                        del invalid[name]
                    elif name == "junit/manifest.json":
                        invalid[name] += b" "
                    else:
                        substitutions = (
                            (b'failures="0"', b'failures="1"')
                            if name.endswith(".xml")
                            else (b'"fixture"', b'"altered"')
                        )
                        invalid[name] = invalid[name].replace(*substitutions)
                        self.assertNotEqual(invalid[name], original[name])
                        self.assertEqual(len(invalid[name]), len(original[name]))
                    gate = self.gate_receipts(files, invalid)
                    self.assertEqual(gate.returncode, 1, gate.stderr)
                    if not delete:
                        message = (
                            "JUnit origin manifest is missing or differs"
                            if name == "junit/manifest.json"
                            else "retained bytes differ:"
                        )
                        self.assertIn(message, gate.stderr)
        gate = self.gate_receipts(files)
        self.assertIn("JUnit origin manifest is missing or differs", gate.stderr)

    def test_reused_gate_refuses_original_that_never_retained_junit(self):
        _, _, original = self.run_ci(gate_run="original", missing=True)
        gate = self.gate_receipts(self.reuse(original), original)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("has no matching passing JUnit evidence", gate.stderr)

    def test_reused_gate_refuses_report_unchanged_in_original_run(self):
        _, _, original = self.run_ci(gate_run="original", missing=True, preexisting=True)
        gate = self.gate_receipts(self.reuse(original), original)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("has no fresh JUnit report", gate.stderr)

    def test_reused_gate_refuses_reference_that_differs_from_test_log(self):
        _, _, original = self.run_ci(gate_run="original")
        files = self.reuse(original)
        manifest = json.loads(files["junit/manifest.json"])
        manifest["reused_from"]["run"] = "another"
        files["junit/manifest.json"] = json.dumps(manifest).encode()
        gate = self.gate_receipts(files, original)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("JUnit origin differs from the test log", gate.stderr)

    def test_reused_gate_refuses_reference_for_different_package_selection(self):
        _, _, original = self.run_ci(gate_run="original")
        files = self.reuse(original)
        manifest = json.loads(files["junit/manifest.json"])
        manifest["selection_packages"] = "sparkles-core"
        files["junit/manifest.json"] = json.dumps(manifest).encode()
        gate = self.gate_receipts(files, original)
        self.assertEqual(gate.returncode, 1)
        self.assertIn("has no matching passing JUnit evidence", gate.stderr)

    def test_gate_refuses_cached_success_without_one_well_formed_origin(self):
        _, _, original = self.run_ci(gate_run="original")
        marker = original["logs/test.log"].decode()
        for log in ("", marker + marker, marker.replace("original", "../original")):
            with self.subTest(log=log):
                result, receipt, files = self.run_ci(gate_run="current", replayed_log=log)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(receipt["status"], "error")
                self.assertIn("test log must name one JUnit origin", receipt["error"])
                self.assertEqual(self.gate_receipts(files, original).returncode, 1)


if __name__ == "__main__":
    unittest.main()
