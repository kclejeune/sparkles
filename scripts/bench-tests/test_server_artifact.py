"""No-build server distribution contract and cleanup regressions."""
import importlib.util
import hashlib
import io
import json
from pathlib import Path
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import unittest
from unittest.mock import patch
import zlib

ROOT = Path(__file__).resolve().parents[2]


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


artifact = module("artifact_test", "server-artifact.py")
smoke = module("installed_test", "server-installed-smoke.py")


class ArchiveContracts(unittest.TestCase):
    def setUp(self):
        self.uid = patch.object(smoke.os, "getuid", return_value=1500)
        self.gid = patch.object(smoke.os, "getgid", return_value=1600)
        self.uid.start()
        self.gid.start()
        self.addCleanup(self.uid.stop)
        self.addCleanup(self.gid.stop)
        self.temp = tempfile.TemporaryDirectory(prefix="server artifact contracts ")
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.version = "0.1.0"
        self.name = f"sparkles-{self.version}-{artifact.TARGET}"
        self.archive = self.work / (self.name + ".tar.gz")
        self.sums = self.work / "SHA256SUMS"
        self.destination = self.work / "installed"

    def fixture(self, omit=None, extra=(), executable=True, record=None):
        build = {"schema_version": 1, "candidate": True, "attested": False, "version": self.version,
                 "target": artifact.TARGET, "commit": "a" * 40,
                 "binary_sha256": hashlib.sha256(b"fixture\n").hexdigest(),
                 "builder": artifact.IMAGES["builder"], "default_features_requested": True}
        build.update(record or {})
        with tarfile.open(self.archive, "w:gz") as output:
            for name in sorted(artifact.REQUIRED - {omit}):
                content = b"fixture\n"
                if name.endswith("build-record.json"):
                    content = json.dumps(build).encode()
                if name.endswith("openapi.json"):
                    content = b'{"openapi":"3.1.0"}'
                member = tarfile.TarInfo(self.name + "/" + name)
                member.size = len(content)
                member.mode = 0o755 if executable and name == "bin/sparkles" else 0o644
                output.addfile(member, io.BytesIO(content))
            for member in extra:
                output.addfile(member)
        self.sums.write_text(f"{artifact.sha256(self.archive)}  {self.archive.name}\n")

    def verify(self):
        return artifact.verify(self.archive, self.sums, self.destination, self.version)

    def test_valid_distribution_under_path_with_spaces(self):
        self.fixture()
        root, record = self.verify()
        self.assertTrue((root / "bin/sparkles").stat().st_mode & 0o111)
        self.assertEqual(record["commit"], "a" * 40)

    def test_tampered_archive_is_rejected_before_extraction(self):
        self.fixture()
        with self.archive.open("ab") as stream:
            stream.write(b"changed")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()
        self.assertFalse(self.destination.exists())

    def test_missing_notices_openapi_and_man_page_fail(self):
        for name in (f"{artifact.DOC}/LICENSE", f"{artifact.DOC}/THIRD_PARTY_LICENSES.md",
                     f"{artifact.DOC}/THIRD_PARTY_LICENSES-UI.md", f"{artifact.DOC}/openapi.json",
                     "share/man/man1/sparkles-serve.1"):
            with self.subTest(name=name):
                self.fixture(omit=name)
                with self.assertRaisesRegex(ValueError, "required"):
                    self.verify()
                self.assertFalse(self.destination.exists())

    def test_nonexecutable_and_wrong_identity_fail(self):
        self.fixture(executable=False)
        with self.assertRaisesRegex(ValueError, "executable"):
            self.verify()
        self.fixture(record={"version": "9.9.9"})
        with self.assertRaisesRegex(ValueError, "identity"):
            self.verify()
        self.assertFalse(self.destination.exists())
        self.fixture(record={"schema_version": 99})
        with self.assertRaisesRegex(ValueError, "schema"):
            self.verify()
        self.assertFalse(self.destination.exists())

    def test_extraction_rejects_traversal_absolute_paths_links_and_special_files(self):
        for name, kind in (("../escape", tarfile.REGTYPE), ("/escape", tarfile.REGTYPE),
                           (self.name + "/link", tarfile.SYMTYPE),
                           (self.name + "/hard", tarfile.LNKTYPE),
                           (self.name + "/fifo", tarfile.FIFOTYPE)):
            with self.subTest(name=name, kind=kind):
                member = tarfile.TarInfo(name)
                member.type = kind
                member.linkname = "../../escape"
                self.fixture(extra=[member])
                with self.assertRaisesRegex(ValueError, "unsafe|forbidden"):
                    self.verify()
                self.assertFalse(self.destination.exists())

    def test_duplicate_member_and_checksum_names_fail(self):
        member = tarfile.TarInfo(self.name + "/bin/sparkles")
        self.fixture(extra=[member])
        with self.assertRaisesRegex(ValueError, "duplicate archive"):
            self.verify()
        self.fixture()
        self.sums.write_text(self.sums.read_text() * 2)
        with self.assertRaisesRegex(ValueError, "duplicate checksum"):
            self.verify()

    def test_provenance_claim_and_unsafe_checksum_fail(self):
        self.fixture(record={"attested": True})
        with self.assertRaisesRegex(ValueError, "provenance"):
            self.verify()
        self.assertFalse(self.destination.exists())
        self.sums.write_text("a" * 64 + "  ../candidate\n")
        with self.assertRaisesRegex(ValueError, "checksum"):
            self.verify()

    def test_placeholder_and_missing_wasm_are_rejected(self):
        ui = self.work / "ui/build"
        (ui / "_app").mkdir(parents=True)
        (ui / "index.html").write_text("The UI has not been built")
        with self.assertRaisesRegex(ValueError, "placeholder"):
            artifact.ui_inputs(self.work)
        (ui / "index.html").write_text("<html>production</html>")
        (ui / "_app/version.json").write_text("{}")
        (ui / "_app/app.js").write_text("// application")
        with self.assertRaisesRegex(ValueError, "Wasm"):
            artifact.ui_inputs(self.work)

    def test_debuglink_crc_and_name_must_match(self):
        debug = self.work / "sparkles.debug"
        debug.write_bytes(b"separate debug information")
        name = b"sparkles.debug\0"
        section = name + b"\0" * (-len(name) % 4) + struct.pack("<I", zlib.crc32(debug.read_bytes()))
        artifact.check_debuglink(section, debug)
        debug.write_bytes(b"corrupt information")
        with self.assertRaisesRegex(ValueError, "CRC"):
            artifact.check_debuglink(section, debug)
        with self.assertRaisesRegex(ValueError, "another file"):
            artifact.check_debuglink(section, self.work / "other.debug")

    def test_incomplete_pack_does_not_publish_output(self):
        source = self.work / "source"
        source.mkdir()
        (source / "Cargo.toml").write_text('[workspace.package]\nversion="0.1.0"\n')
        output = self.work / "output"

        def fake_run(args, **kwargs):
            if "status" in args:
                return ""
            if "rev-parse" in args:
                return "a" * 40
            if "show" in args:
                return "1000"
            return "sparkles 9.9.9"

        with patch.object(artifact, "run", fake_run):
            with self.assertRaisesRegex(ValueError, "version mismatch"):
                artifact.pack(self.work / "binary", source, output, artifact.IMAGES["builder"])
        self.assertFalse(output.exists())

    def test_late_pack_failure_removes_partial_distribution(self):
        source = self.work / "source"
        source.mkdir()
        (source / "docs").mkdir()
        (source / "Cargo.toml").write_text('[workspace.package]\nversion="0.1.0"\n')
        (source / "Cargo.lock").write_text("fixture lock")
        (source / "docs/openapi.json").write_text('{"openapi":"3.1.0"}')
        for name in ("LICENSE", "THIRD_PARTY_LICENSES.md", "THIRD_PARTY_LICENSES-UI.md"):
            (source / name).write_text("fixture notice")
        binary = self.work / "binary"
        binary.write_bytes(b"prebuilt fixture")
        output = self.work / "output"

        def fake_run(args, **kwargs):
            words = [str(arg) for arg in args]
            if "status" in words:
                return ""
            if "rev-parse" in words:
                return "a" * 40
            if "show" in words:
                return "1000"
            if "--section-headers" in words:
                return "[12] .debug_line PROGBITS"
            if words[-1] == "openapi":
                return b'{"openapi":"outdated"}'
            if words[-1] == "--version":
                return "sparkles 0.1.0" if words[0] == str(binary) else "mbx 1.28.0"
            raise AssertionError(words)

        with patch.object(artifact, "run", fake_run), patch.object(artifact, "ui_inputs", return_value={}), \
                patch.object(artifact, "audit_elf", return_value=({}, {})):
            with self.assertRaisesRegex(ValueError, "OpenAPI differs"):
                artifact.pack(binary, source, output, artifact.IMAGES["builder"])
        self.assertFalse(output.exists())
        self.assertFalse(list(self.work.glob(".server-artifact-*")))

    def test_start_failure_cleans_container_and_records_failure(self):
        self.fixture()
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if command[1] == "start":
                raise subprocess.TimeoutExpired(command, 45)
            return subprocess.CompletedProcess(command, 0, "", "")

        with patch.object(smoke.subprocess, "run", execute):
            with self.assertRaises(subprocess.TimeoutExpired):
                smoke.smoke(self.archive, self.sums, self.version, "rocky8", self.work / "evidence")
        self.assertTrue(any(command[1:3] == ["rm", "--force"] for command in calls))
        create = next(command for command in calls if command[1] == "create")
        self.assertIn("--pull=never", create)
        self.assertIn("--read-only", create)
        self.assertIn("127.0.0.1::3030", create)
        self.assertEqual(create[create.index("--user") + 1], "1500:1600")
        self.assertNotIn(str(ROOT), " ".join(create))
        receipt = json.loads((self.work / "evidence/receipt.json").read_text())
        self.assertFalse(receipt["passed"])
        self.assertEqual(receipt["container_user"], {"uid": 1500, "gid": 1600})

    def test_create_timeout_also_cleans_container(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            if command[1] == "create":
                raise subprocess.TimeoutExpired(command, 45)
            return subprocess.CompletedProcess(command, 0, "", "")

        runtime = smoke.Runtime("image", self.work, self.work, self.work / "logs", execute)
        try:
            with self.assertRaises(subprocess.TimeoutExpired):
                runtime.start()
        finally:
            runtime.cleanup()
        self.assertTrue(any(command[1:3] == ["rm", "--force"] for command in calls))

    def test_killed_shutdown_is_not_success(self):
        def execute(command, **kwargs):
            output = '{"ExitCode":137,"OOMKilled":false}' if command[1] == "inspect" else ""
            return subprocess.CompletedProcess(command, 0, output, "")
        runtime = smoke.Runtime("image", self.work, self.work, self.work / "logs", execute)
        with self.assertRaisesRegex(ValueError, "gracefully"):
            runtime.stop()

    def test_cleanup_failure_is_not_success(self):
        def execute(command, **kwargs):
            return subprocess.CompletedProcess(command, int(command[1] == "rm"), "", "daemon unavailable")
        runtime = smoke.Runtime("image", self.work, self.work, self.work / "logs", execute)
        runtime.created = True
        with self.assertRaisesRegex(ValueError, "remove"):
            runtime.cleanup()
        self.assertTrue(runtime.created)

    def test_root_caller_is_refused_before_docker_creation(self):
        calls = []

        def execute(command, **kwargs):
            calls.append(command)
            raise AssertionError("Docker must not be called for a root caller")

        with patch.object(smoke.os, "getuid", return_value=0), \
                patch.object(smoke.os, "getgid", return_value=0):
            with self.assertRaisesRegex(ValueError, "non-root"):
                smoke.Runtime("image", self.work, self.work, self.work / "logs", execute).start()
        self.assertEqual(calls, [])

    def test_root_group_is_also_refused_before_docker_creation(self):
        def execute(command, **kwargs):
            raise AssertionError("Docker must not be called with root group")

        with patch.object(smoke.os, "getgid", return_value=0):
            with self.assertRaisesRegex(ValueError, "non-root"):
                smoke.Runtime("image", self.work, self.work, self.work / "logs", execute).start()

    def test_root_smoke_records_failure_without_docker_execution(self):
        self.fixture()
        output = self.work / "root-evidence"
        with patch.object(smoke.os, "getuid", return_value=0), \
                patch.object(smoke.os, "getgid", return_value=0), \
                patch.object(smoke.subprocess, "run", side_effect=AssertionError("unexpected Docker")):
            with self.assertRaisesRegex(ValueError, "non-root"):
                smoke.smoke(self.archive, self.sums, self.version, "rocky8", output)
        receipt = json.loads((output / "receipt.json").read_text())
        self.assertFalse(receipt["passed"])
        self.assertEqual(receipt["container_user"], {"uid": 0, "gid": 0})


class ElfContracts(unittest.TestCase):
    def test_gnu_floor_and_dependency_audit(self):
        header = "ELF64 Advanced Micro Devices X86-64"
        program = "[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]"
        dynamic = "(NEEDED) Shared library: [libc.so.6]"
        self.assertEqual(artifact.parse_elf(header, program, dynamic, "GLIBC_2.2.5 GLIBC_2.28")["glibc"],
                         ["2.2.5", "2.28"])
        for versions in ("GLIBC_2.29", "GLIBC_PRIVATE", "GLIBC_ABI_DT_RELR"):
            with self.subTest(versions=versions), self.assertRaisesRegex(ValueError, "baseline"):
                artifact.parse_elf(header, program, dynamic, versions)
        for extra in ("(RUNPATH) [/nix/store/x/lib]", "(NEEDED) [libstdc++.so.6]"):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                artifact.parse_elf(header, program, dynamic + extra, "GLIBC_2.28")


class WorkflowContracts(unittest.TestCase):
    def test_outer_artifact_task_activates_the_locked_formatter_tool(self):
        config = tomllib.loads((ROOT / "mise.toml").read_text())
        key = "github:wasm-bindgen/wasm-bindgen"
        formatter_pin = config["tasks"]["ui:wasm"]["tools"][key]
        outer_pin = config["tasks"]["server:artifact"].get("tools", {}).get(key)
        locked = next(package["version"] for package in
                      tomllib.loads((ROOT / "Cargo.lock").read_text())["package"]
                      if package["name"] == "wasm-bindgen")
        self.assertEqual(outer_pin, formatter_pin,
                         "the parent packer must retain the formatter task's tool activation")
        self.assertEqual(outer_pin, locked)

    def test_builder_pin_does_not_drift_from_candidate_configuration(self):
        workflow = (ROOT / ".github/workflows/server-artifacts.yml").read_text()
        self.assertEqual(workflow.count(artifact.IMAGES["builder"]), 2)
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("packages: write", workflow)
        self.assertNotIn("CARGO_TARGET_DIR", workflow)
        self.assertNotIn("--target-dir", workflow)
        self.assertIn("github-cache-mode: objects", workflow)
        self.assertIn("version: 1.28.0", workflow)


if __name__ == "__main__":
    unittest.main()
