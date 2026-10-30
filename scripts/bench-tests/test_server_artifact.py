"""No-build server distribution contract and cleanup regressions."""
import importlib.util
import hashlib
import contextlib
import io
import json
import os
from pathlib import Path
import platform
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import textwrap
from types import SimpleNamespace
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


class OpenApiComparison(unittest.TestCase):
    def setUp(self):
        self.source = {"openapi": "3.1.0", "info": {"version": "0.0.0", "title": "Sparkles"},
                       "components": {"schemas": {"A": {"required": ["name"],
                           "properties": {"name": {"$ref": "#/components/schemas/Name"}}}}}}
        self.runtime = json.loads(json.dumps(self.source))
        self.runtime["info"]["version"] = "0.1.0"

    def check(self):
        artifact.check_openapi(json.dumps(self.runtime), json.dumps(self.source), "0.1.0")

    def test_runtime_version_is_the_only_permitted_difference(self):
        before = json.dumps(self.runtime, sort_keys=True)
        self.assertNotEqual(self.runtime, self.source)  # The old direct comparison refused this pair.
        self.check()
        self.assertEqual(json.dumps(self.runtime, sort_keys=True), before)

    def test_wrong_missing_and_nonstring_runtime_version_are_refused(self):
        for value in ("0.0.0", None, 1, True):
            with self.subTest(value=value):
                self.runtime["info"]["version"] = value
                with self.assertRaisesRegex(ValueError, "/info/version"):
                    self.check()
        del self.runtime["info"]["version"]
        with self.assertRaisesRegex(ValueError, "/info/version"):
            self.check()
        del self.runtime["info"]
        with self.assertRaisesRegex(ValueError, "/info/version"):
            self.check()

    def test_wrong_or_missing_source_placeholder_is_refused(self):
        for value in ("0.1.0", None, 1):
            with self.subTest(value=value):
                self.source["info"]["version"] = value
                with self.assertRaisesRegex(ValueError, "/info/version"):
                    self.check()
        del self.source["info"]["version"]
        with self.assertRaisesRegex(ValueError, "/info/version"):
            self.check()
        del self.source["info"]
        with self.assertRaisesRegex(ValueError, "/info/version"):
            self.check()

    def test_other_contract_changes_are_refused_with_json_pointers(self):
        cases = [
            (lambda d: d["info"].update(title="Changed"), "/info/title"),
            (lambda d: d["components"]["schemas"]["A"].update(required=[]), "/components/schemas/A/required"),
            (lambda d: d["components"]["schemas"]["A"]["properties"]["name"].update({"$ref": "#/other"}), "/components/schemas/A/properties/name/$ref"),
        ]
        for change, pointer in cases:
            with self.subTest(pointer=pointer):
                self.runtime = json.loads(json.dumps(self.source))
                self.runtime["info"]["version"] = "0.1.0"
                change(self.runtime)
                with self.assertRaisesRegex(ValueError, re.escape(pointer)):
                    self.check()

class DebugSplitContracts(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="sparkles-debug-split-")
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)

    @unittest.skipUnless(platform.system() == "Linux" and all(shutil.which(t) for t in ("rustc", "objcopy", "readelf")),
                         "real ELF probe needs Linux, rustc and binutils")
    def test_real_rust_elf_old_marker_failure_and_new_separation(self):
        source = self.work / "main.rs"
        source.write_text('fn main() { println!("debug section probe"); }\n')
        original = self.work / "original"
        subprocess.run(["rustc", "-C", "debuginfo=1", source, "-o", original], check=True, capture_output=True)
        expected_debug = self.work / "expected.debug"
        subprocess.run(["objcopy", "--only-keep-debug", original, expected_debug], check=True, capture_output=True)
        old = self.work / "old"
        shutil.copy2(original, old)
        subprocess.run(["objcopy", "--strip-debug", old], check=True, capture_output=True)
        subprocess.run(["objcopy", f"--add-gnu-debuglink={expected_debug}", old], check=True, capture_output=True)
        old_headers = subprocess.run(["readelf", "--wide", "--section-headers", old], check=True, capture_output=True, text=True).stdout
        self.assertIn(".debug_gdb_scripts", artifact.section_summary(old_headers))
        self.assertRegex(old_headers, r"\]\s+\.debug_")
        installed, debug, diagnostics = self.work / "sparkles", self.work / "sparkles.debug", self.work / "diagnostics"
        shutil.copy2(original, installed)
        before = subprocess.run(["readelf", "--wide", "--section-headers", installed], check=True, capture_output=True, text=True).stdout
        with contextlib.redirect_stdout(io.StringIO()):
            artifact.split_debug(installed, debug, diagnostics, before)
        after = (diagnostics / "sections-after.txt").read_text()
        sections = artifact.section_summary(after)
        self.assertIn(".symtab", sections)
        self.assertIn(".gnu_debuglink", sections)
        self.assertFalse([name for name in sections if name.startswith(".debug_")])
        self.assertEqual(debug.read_bytes(), expected_debug.read_bytes())
        self.assertIn(".debug_line", artifact.section_summary((diagnostics / "debug-sections.txt").read_text()))
        self.assertEqual(subprocess.run([installed], check=True, capture_output=True, text=True).stdout.strip(), "debug section probe")
        artifact.check_debuglink((diagnostics / "debuglink").read_bytes(), debug)
        self.assertEqual((diagnostics / "sections-before.txt").read_text(), before)

    def test_tool_failure_retains_stderr_and_before_headers(self):
        diag = self.work / "tool-error"
        def fake_run(args, **kwargs):
            raise subprocess.CalledProcessError(1, args, stderr=b"fixture objcopy failure")
        output = io.StringIO()
        with patch.object(artifact, "run", fake_run), contextlib.redirect_stdout(output):
            with self.assertRaises(subprocess.CalledProcessError):
                artifact.split_debug(self.work / "installed", self.work / "debug", diag, "before headers")
        self.assertEqual((diag / "sections-before.txt").read_text(), "before headers")
        self.assertEqual((diag / "tool-error.txt").read_text(), "fixture objcopy failure")
        self.assertIn("objcopy --only-keep-debug failed", output.getvalue())
        self.assertIn("fixture objcopy failure", output.getvalue())

    def test_unparsed_debug_section_is_still_refused(self):
        headers = "  [ 1] .symtab SYMTAB 0 0 0 00 0 0 1\n  [ 2] .debug_line malformed\n"
        def fake_run(args, **kwargs):
            return headers if str(args[0]) == "readelf" else b""
        with patch.object(artifact, "run", fake_run), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(ValueError, "unparsed readelf section"):
                artifact.split_debug(self.work / "installed", self.work / "debug", self.work / "unparsed", "before")

    def test_substantive_debug_sections_are_still_rejected_and_reported(self):
        for name in (".debug_line", ".debug_info"):
            with self.subTest(name=name):
                diag = self.work / name[1:]
                headers = f"  [ 1] .symtab SYMTAB 0 0 0 00 0 0 1\n  [ 2] {name} PROGBITS 0 0 0 00 A 0 0 1\n"
                def fake_run(args, **kwargs):
                    return headers if str(args[0]) == "readelf" else b""
                with patch.object(artifact, "run", fake_run), contextlib.redirect_stdout(io.StringIO()):
                    with self.assertRaisesRegex(ValueError, re.escape(name)):
                        artifact.split_debug(self.work / "installed", self.work / "debug", diag, "before")
                self.assertEqual((diag / "sections-after.txt").read_text(), headers)
                self.assertEqual(artifact.section_summary(headers)[name], "A")

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

    def test_pack_keeps_generated_openapi_bytes_after_placeholder_comparison(self):
        source = self.work / "source"
        source.mkdir()
        (source / "docs").mkdir()
        (source / "Cargo.toml").write_text('[workspace.package]\nversion="0.1.0"\n')
        (source / "Cargo.lock").write_text("fixture lock")
        (source / "docs/openapi.json").write_text('{"info":{"version":"0.0.0"}}')
        for name in ("LICENSE", "THIRD_PARTY_LICENSES.md", "THIRD_PARTY_LICENSES-UI.md"):
            (source / name).write_text("fixture notice")
        binary = self.work / "binary"
        binary.write_bytes(b"prebuilt fixture")
        output = self.work / "output"
        generated = b'{ "info": { "version": "0.1.0" } }\n'
        captured = []

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
                return generated
            if len(words) > 1 and words[1] == "man":
                tree = Path(words[-1]).parents[2]
                captured.append((tree / artifact.DOC / "openapi.json").read_bytes())
                raise ValueError("stop after capture")
            if words[-1] == "--version":
                return "sparkles 0.1.0" if words[0] == str(binary) else "mbx 1.28.0"
            raise AssertionError(words)

        with patch.object(artifact, "run", fake_run), patch.object(artifact, "ui_inputs", return_value={}), \
                patch.object(artifact, "audit_elf", return_value=({}, {})):
            with self.assertRaisesRegex(ValueError, "stop after capture"):
                artifact.pack(binary, source, output, artifact.IMAGES["builder"])
        self.assertEqual(captured, [generated])
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

    def test_debian_receipt_records_trust_and_refusal_precedes_server(self):
        self.fixture()
        output = self.work / "debian-evidence"
        fixture = {"version": 1, "sha256": "fixed"}
        with patch.object(smoke, "debian_trust_fixture", return_value=(self.work / "ca.pem", fixture)), \
             patch.object(smoke.Runtime, "start", side_effect=RuntimeError("stop before server")), \
             patch.object(smoke.Runtime, "cleanup"):
            with self.assertRaisesRegex(RuntimeError, "stop before server"):
                smoke.smoke(self.archive, self.sums, self.version, "debian11", output)
        receipt = json.loads((output / "receipt.json").read_text())
        self.assertEqual(receipt["trust_fixture"], fixture)
        self.assertFalse(receipt["passed"])
        output = self.work / "bad-debian-evidence"
        with patch.object(smoke, "debian_trust_fixture", side_effect=ValueError("bad trust")), \
             patch.object(smoke, "Runtime", side_effect=AssertionError("server must not start")):
            with self.assertRaisesRegex(ValueError, "bad trust"):
                smoke.smoke(self.archive, self.sums, self.version, "debian11", output)

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


class RuntimeTrustAndReadiness(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        self.addCleanup(self.tmp.cleanup)

    def runtime(self, execute):
        return smoke.Runtime("image", self.root, self.root, self.root / "logs", execute, user=(1000, 100))

    def test_startup_reset_then_ready_and_no_other_retry(self):
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            result = "127.0.0.1:3030" if command[1] == "port" else "true"
            return subprocess.CompletedProcess(command, 0, result, "")
        runtime = self.runtime(execute)
        with patch.object(runtime, "request", side_effect=[ConnectionResetError(), (b"ok", "text/plain")]), \
             patch.object(runtime, "binary", return_value=""), patch.object(smoke.time, "sleep"):
            runtime.start()
        self.assertEqual(sum(c[1] == "inspect" for c in calls), 1)
        runtime = self.runtime(execute)
        with patch.object(runtime, "request", return_value=(b"ok", "text/plain")), \
             patch.object(runtime, "binary", side_effect=RuntimeError("ping failed")):
            with self.assertRaisesRegex(RuntimeError, "ping failed"):
                runtime.start()

    def test_repeated_resets_timeout_and_stopped_container_refuses(self):
        def execute(command, **kwargs):
            return subprocess.CompletedProcess(command, 0, "127.0.0.1:3030" if command[1] == "port" else "true", "")
        runtime = self.runtime(execute)
        with patch.object(runtime, "request", side_effect=ConnectionResetError()), \
             patch.object(smoke.time, "monotonic", side_effect=[0, 0, 121]), patch.object(smoke.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "120 seconds"):
                runtime.start()
        def stopped(command, **kwargs):
            return subprocess.CompletedProcess(command, 0, "127.0.0.1:3030" if command[1] == "port" else "false", "")
        runtime = self.runtime(stopped)
        with patch.object(runtime, "request", side_effect=ConnectionResetError()):
            with self.assertRaisesRegex(ValueError, "exited during startup"):
                runtime.start()

    def test_donor_cleanup_even_when_create_or_copy_times_out(self):
        for step in ("create", "cp"):
            calls = []
            def execute(command, **kwargs):
                calls.append(command)
                if command[1] == step: raise subprocess.TimeoutExpired(command, 45)
                return subprocess.CompletedProcess(command, 0, "", "")
            with self.subTest(step=step), self.assertRaises(subprocess.TimeoutExpired):
                smoke.debian_trust_fixture(self.root, execute)
            self.assertEqual(calls[-1][1:3], ["rm", "--force"])
            self.assertIn("--pull=never", calls[0])
            self.assertEqual(calls[0][-1], artifact.IMAGES["runtimes"]["rocky8"])

    def test_invalid_digest_and_pem_are_refused(self):
        def execute(command, **kwargs):
            if command[1] == "cp":
                self.assertTrue(command[2].endswith(":/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem"))
                Path(command[3]).write_bytes(b"invalid")
            return subprocess.CompletedProcess(command, 0, "", "")
        with self.assertRaisesRegex(ValueError, "size or digest"):
            smoke.debian_trust_fixture(self.root, execute)
        fixture = dict(artifact.IMAGES["debian_trust_fixture"], size=7, sha256=hashlib.sha256(b"invalid").hexdigest())
        with patch.dict(smoke.artifact.IMAGES, {"debian_trust_fixture": fixture}):
            with self.assertRaises(smoke.ssl.SSLError):
                smoke.debian_trust_fixture(self.root, execute)

    def test_fixture_pin_and_refuse_nonregular_or_empty(self):
        fixture = smoke.artifact.IMAGES["debian_trust_fixture"]
        self.assertEqual(fixture["version"], 1)
        self.assertEqual(fixture["donor_runtime"], "rocky8")
        self.assertEqual(fixture["path"], "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem")
        self.assertEqual(fixture["size"], 222082)
        self.assertEqual(fixture["sha256"], "a0a9885edfce047620854ee7bb4214dfc2a19d666b7fc2c9def6f439c69e7739")
        for kind in ("empty", "symlink"):
            target = self.root / "debian-ca-bundle.pem"
            target.unlink(missing_ok=True)
            def execute(command, **kwargs):
                if command[1] == "cp":
                    if kind == "empty": target.write_bytes(b"")
                    else: target.symlink_to(self.root / "missing")
                return subprocess.CompletedProcess(command, 0, "", "")
            with self.subTest(kind=kind), self.assertRaises(ValueError):
                smoke.debian_trust_fixture(self.root, execute)

    def test_data_requests_do_not_retry_connection_errors(self):
        runtime = self.runtime(lambda *args, **kwargs: None)
        runtime.base = "http://127.0.0.1:1"
        with patch.object(smoke.urllib.request, "urlopen", side_effect=ConnectionResetError()) as call:
            with self.assertRaises(ConnectionResetError): runtime.request("/ds/sparql")
            self.assertEqual(call.call_count, 1)

    def test_rocky_runtime_does_not_mount_trust_fixture(self):
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, "127.0.0.1:3030", "")
        runtime = self.runtime(execute)
        with patch.object(runtime, "request", return_value=(b"", "")), patch.object(runtime, "binary", return_value=""):
            runtime.start()
        self.assertFalse(any("SSL_CERT_FILE" in arg or "sparkles-trust" in arg for arg in calls[0]))

    def test_runtime_trust_is_single_read_only_mount(self):
        calls = []
        def execute(command, **kwargs):
            calls.append(command)
            return subprocess.CompletedProcess(command, 0, "127.0.0.1:3030", "")
        runtime = smoke.Runtime("image", self.root, self.root, self.root / "logs", execute, user=(1000,100), trust_file=self.root / "ca.pem")
        with patch.object(runtime, "request", return_value=(b"", "")), patch.object(runtime, "binary", return_value=""):
            runtime.start()
        command = calls[0]
        self.assertIn(f"type=bind,source={self.root / 'ca.pem'},target=/opt/sparkles-trust/ca.pem,readonly", command)
        self.assertIn("SSL_CERT_FILE=/opt/sparkles-trust/ca.pem", command)
        self.assertNotIn(str(ROOT), " ".join(command))


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
    def builder_fixture(self, work):
        source = work / "checked out source"
        (source / "scripts").mkdir(parents=True)
        for name in ("server-artifact-build.sh", "server-artifact-images.json"):
            shutil.copyfile(ROOT / "scripts" / name, source / "scripts" / name)
        env = {key: value for key, value in os.environ.items()
               if not key.startswith("GIT_") and key != "CARGO_TARGET_DIR"}
        env.update({"GIT_CONFIG_GLOBAL": str(work / "gitconfig"), "GIT_CONFIG_NOSYSTEM": "1"})
        for arguments in (("init",), ("add", "."),
                          ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                           "commit", "-m", "Fixture")):
            subprocess.run(["git", "-C", str(source), *arguments], check=True,
                           capture_output=True, env=env)
        tools = work / "tools"
        tools.mkdir()
        (tools / "mbx").write_text('''#!/bin/bash
echo "$*" >> "$BUILDER_CALLS"
case "$1" in
  --version) echo 'mbx 1.28.0';;
  setup) exit 0;;
  sparkles-task) exit 42;;
  *) exit 99;;
esac
''')
        (tools / "uname").write_text("#!/bin/sh\necho x86_64\n")
        cargo = work / "data/mbx/bin/cargo"
        cargo.parent.mkdir(parents=True)
        cargo.write_text("#!/bin/sh\necho 'fixture Cargo shim'\n")
        for tool in (tools / "mbx", tools / "uname", cargo):
            tool.chmod(0o755)
        env.update({"PATH": f"{tools}:{env['PATH']}", "XDG_DATA_HOME": str(work / "data"),
                    "BUILDER_CALLS": str(work / "builder-calls"),
                    "AUDITWHEEL_POLICY": "manylinux_2_28", "GIT_TEST_ASSUME_DIFFERENT_OWNER": "1",
                    "SPARKLES_ARTIFACT_BUILDER_IMAGE": artifact.IMAGES["builder"],
                    "GITHUB_WORKSPACE": str(source)})
        return source, env

    def run_builder(self, source, env):
        return subprocess.run(["bash", str(source / "scripts/server-artifact-build.sh"),
                               str(source.parent / "output")], env=env, capture_output=True,
                              text=True, timeout=20)

    def trust_workspace(self, source, env):
        workflow = (ROOT / ".github/workflows/server-artifacts.yml").read_text()
        code = textwrap.dedent(workflow.split("# BEGIN SERVER ARTIFACT WORKSPACE TRUST", 1)[1]
                               .split("# END SERVER ARTIFACT WORKSPACE TRUST", 1)[0])
        subprocess.run(["bash", "-euo", "pipefail", "-c", code], env=env, cwd=source,
                       check=True, capture_output=True, text=True, timeout=20)

    def test_untrusted_source_stops_before_any_build(self):
        with tempfile.TemporaryDirectory(prefix="artifact ownership ") as temporary:
            work = Path(temporary)
            source, env = self.builder_fixture(work)
            result = self.run_builder(source, env)
            self.assertIn("dubious ownership", result.stderr)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotEqual(result.returncode, 42, "Git failure must not reach the build lease")
            self.assertNotIn("sparkles-task", (work / "builder-calls").read_text())

    def test_workspace_trust_is_exact_and_preserves_dirty_source_refusal(self):
        for dirty in (None, "tracked", "untracked"):
            with self.subTest(dirty=dirty), \
                    tempfile.TemporaryDirectory(prefix="artifact ownership ") as temporary:
                work = Path(temporary)
                source, env = self.builder_fixture(work)
                self.trust_workspace(source, env)
                safe = subprocess.run(["git", "config", "--global", "--get-all", "safe.directory"],
                                      env=env, check=True, capture_output=True, text=True).stdout
                self.assertEqual(safe.splitlines(), [str(source)])
                if dirty == "tracked":
                    with (source / "scripts/server-artifact-build.sh").open("a") as stream:
                        stream.write("\n# changed tracked source\n")
                elif dirty == "untracked":
                    (source / "untracked-source").write_text("changed\n")
                result = self.run_builder(source, env)
                if dirty:
                    self.assertIn("source must be committed and clean", result.stderr)
                    self.assertNotEqual(result.returncode, 42)
                    self.assertNotIn("sparkles-task", (work / "builder-calls").read_text())
                else:
                    self.assertEqual(result.returncode, 42, result.stderr)

    def preflight(self, total=15 * 1024**3, available=14 * 1024**3,
                  free=24 * 1024**3, cpus=4, missing=(), machine="x86_64"):
        workflow = (ROOT / ".github/workflows/server-artifacts.yml").read_text()
        code = textwrap.dedent(workflow.split("# BEGIN SERVER ARTIFACT PREFLIGHT", 1)[1]
                               .split("# END SERVER ARTIFACT PREFLIGHT", 1)[0])

        def read_text(path, *args, **kwargs):
            if str(path) == "/proc/meminfo":
                return f"MemTotal: {total // 1024} kB\nMemAvailable: {available // 1024} kB\n"
            if str(path) == "/etc/os-release":
                return 'ID="almalinux"\nVERSION_ID="8"\n'
            raise AssertionError(f"unexpected preflight read: {path}")

        output = io.StringIO()
        failure = None
        with patch.object(Path, "read_text", read_text), \
                patch.object(platform, "system", return_value="Linux"), \
                patch.object(platform, "machine", return_value=machine), \
                patch.object(os, "cpu_count", return_value=cpus), \
                patch.object(shutil, "disk_usage", return_value=SimpleNamespace(free=free)), \
                patch.object(shutil, "which", side_effect=lambda tool: None if tool in missing else f"/bin/{tool}"), \
                contextlib.redirect_stdout(output):
            try:
                exec(compile(code, "workflow-capacity-preflight", "exec"), {})
            except SystemExit as error:
                failure = str(error)
        return json.loads(output.getvalue()), failure

    def test_preflight_accepts_exact_capacity_boundaries(self):
        record, failure = self.preflight()
        self.assertIsNone(failure)
        self.assertEqual(record["failures"], [])
        self.assertEqual(record["capacity"]["host_memory_total_bytes"], 15 * 1024**3)

    def test_preflight_rejects_insufficient_host_memory_and_storage(self):
        for argument, value, reason in (
            ("total", 15 * 1024**3 - 1024, "physical host memory"),
            ("available", 14 * 1024**3 - 1024, "available host memory"),
            ("free", 24 * 1024**3 - 1, "free workspace storage"),
            ("cpus", 3, "host CPUs"),
        ):
            with self.subTest(argument=argument):
                record, failure = self.preflight(**{argument: value})
                self.assertIn(reason, failure)
                self.assertTrue(record["failures"])

    def test_preflight_rejects_missing_tools_and_wrong_architecture(self):
        record, failure = self.preflight(missing=("cmake", "objcopy"))
        self.assertIn("missing native build tools", failure)
        self.assertEqual(record["capacity"]["missing_tools"], ["cmake", "objcopy"])
        _, failure = self.preflight(machine="aarch64")
        self.assertIn("Linux x86_64", failure)

    def test_main_ref_and_resource_limits_are_exact_and_precede_builds(self):
        workflow = (ROOT / ".github/workflows/server-artifacts.yml").read_text()
        branches = re.search(r"^    branches: \[([^\]]+)\]$", workflow, re.M).group(1)
        self.assertEqual([value.strip() for value in branches.split(",")], ["main"])
        self.assertIn("options: --cpus 4 --memory 14g --memory-swap 14g --pids-limit 2048", workflow)
        self.assertEqual(re.findall(r"timeout-minutes: (\d+)", workflow), ["90", "10"])
        self.assertLess(workflow.index("# BEGIN SERVER ARTIFACT PREFLIGHT"),
                        workflow.index("actions-rust-lang/setup-rust-toolchain"))
        self.assertLess(workflow.index("# BEGIN SERVER ARTIFACT PREFLIGHT"),
                        workflow.index("mise run server:artifact"))

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
