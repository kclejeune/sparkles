#!/usr/bin/env python3
"""Inert retention checks; fake tools never invoke Cargo or MBX builds."""

import importlib.util
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]


class CiJunitIntegration(unittest.TestCase):
    def run_ci(
        self,
        *,
        failing=False,
        missing=False,
        profiles=("default",),
        later=False,
        copy_error=False,
    ):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / "bin").mkdir()
            target = root / "cache/targets/v1/key"
            target.mkdir(parents=True)
            (root / "target").symlink_to(target)
            target.with_suffix(".json").write_text('{"workspace_root":"fixture"}\n')
            for name in ("ci.sh", "ci-retain-junit.py"):
                if (ROOT / "scripts" / name).exists():
                    shutil.copyfile(ROOT / "scripts" / name, root / "scripts" / name)
            tools = {
                "git": '#!/bin/sh\ncase "$1" in rev-parse) echo 1111111111111111111111111111111111111111;; status) :;; *) exit 1;; esac\n',
                "mbx": '#!/bin/bash\nshift 2\ntouch "$PWD/lease-live"\n"$@"\nrc=$?\nrm -rf "$PWD/cache" "$PWD/target" "$PWD/lease-live"\nexit "$rc"\n',
                "mise": """#!/usr/bin/env python3
import os,pathlib,sys
root=pathlib.Path.cwd(); task=sys.argv[-1]
if task=='test':
 assert (root/'lease-live').exists()
 if os.environ['MISSING']=='0':
  for profile in os.environ['PROFILES'].split(','):
   p=root/'target/nextest'/profile/'junit.xml'; p.parent.mkdir(parents=True,exist_ok=True)
   p.write_bytes(('<testsuites fixture="'+profile+'"><testsuite failures="'+os.environ['FAIL']+'"/></testsuites>\\n').encode())
 if os.environ['COPY_ERROR']=='1': (root/'.artifacts/ci/junit/.staging').mkdir()
 sys.exit(29 if os.environ['FAIL']=='1' else 0)
if task=='node:test':
 p=root/'target/nextest/default/junit.xml';p.write_text('overwritten by later task')
""",
            }
            for name, text in tools.items():
                p = root / "bin" / name
                p.write_text(text)
                p.chmod(0o755)
            env = dict(
                os.environ,
                PATH=str(root / "bin") + os.pathsep + os.environ["PATH"],
                FAIL="1" if failing else "0",
                MISSING="1" if missing else "0",
                PROFILES=",".join(profiles),
                COPY_ERROR="1" if copy_error else "0",
            )
            env.pop("SPARKLES_CI_LEASED", None)
            tasks = ["test", "node:test"] if later else ["test"]
            result = subprocess.run(
                ["bash", "scripts/ci.sh", "--jobs", "1", *tasks],
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
        value["version"] = 2
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
        for version in (True, 2):
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
        with (
            mock.patch.object(
                self.helper,
                "read_regular",
                side_effect=AssertionError("must refuse before file reads"),
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
    def gate_copy(self, missing=False, copy_error=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            (root / ".artifacts/ci").mkdir(parents=True)
            shutil.copyfile(
                ROOT / "scripts/ci-retain-junit.py", root / "scripts/ci-retain-junit.py"
            )
            out = root / "gate-out"
            out.mkdir()
            (out / "ci.exit").write_text("29\n")
            spec = importlib.util.spec_from_file_location(
                "gate_fixture_helper", root / "scripts/ci-retain-junit.py"
            )
            helper = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(helper)
            args = type("Args", (), {"base": "", "packages": "all", "selected": True})()
            retained = root / ".artifacts/ci/junit"
            with mock.patch.object(
                helper.subprocess, "check_output", side_effect=["a" * 40, ""]
            ):
                helper.init(root, retained, args)
            helper.before(root, retained)
            if not missing:
                report = root / "target/nextest/default/junit.xml"
                report.parent.mkdir(parents=True)
                report.write_bytes(
                    b'<testsuites><testsuite failures="1"/></testsuites>\n'
                )
            helper.after(root, retained, 0 if missing else 29)
            if copy_error:
                (out / "ci-receipts").mkdir()
            source = (ROOT / "scripts/gate/remote/gate-inner.sh").read_text()
            function = source.split("  retain_ci_receipts() {", 1)[1].split(
                "  step ci-receipts retain_ci_receipts", 1
            )[0]
            script = (
                'set -uo pipefail\nout="$PWD/gate-out"\nretain_ci_receipts() {'
                + function
                + '\nretain_ci_receipts\nrc=$?\necho "$rc" > "$out/ci-receipts.exit"\nexit "$rc"\n'
            )
            result = subprocess.run(
                ["bash", "-c", script],
                check=False,
                cwd=root,
                capture_output=True,
                text=True,
                timeout=10,
            )
            self.assertEqual((out / "ci.exit").read_text(), "29\n")
            return result.returncode, int((out / "ci-receipts.exit").read_text())

    def test_valid_copy_has_separate_status_without_rewriting_ci_failure(self):
        self.assertEqual(self.gate_copy(), (0, 0))

    def test_missing_report_fails_evidence_only(self):
        self.assertEqual(self.gate_copy(missing=True), (1, 1))

    def test_copy_failure_is_not_hidden(self):
        self.assertEqual(self.gate_copy(copy_error=True), (1, 1))


if __name__ == "__main__":
    unittest.main()
