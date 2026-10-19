"""Exercise package selection without building packages or starting benchmark servers."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
TOOLS = ROOT / "scripts" / "bench-tools.sh"


class BenchToolsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="bench tools ")
        self.addCleanup(self.tmp.cleanup)
        self.work = Path(self.tmp.name)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.calls = self.work / "calls.jsonl"
        nix = self.bin / "nix"
        nix.write_text("""#!/usr/bin/env python3
import json, os, pathlib, sys
with open(os.environ['CALLS'], 'a') as f:
    f.write(json.dumps(sys.argv[1:]) + '\\n')
if os.environ.get('FAIL_NIX'):
    sys.exit(42)
if sys.argv[1] == 'eval':
    print('4.2.2', end='')
    sys.exit(0)
link = pathlib.Path(sys.argv[sys.argv.index('--out-link') + 1])
(link / 'bin').mkdir(parents=True, exist_ok=True)
if not os.environ.get('MISSING_BIN'):
    for binary in ['qlever-index', 'qlever-server', 'tdb2.tdbloader', 'fuseki-server', 'oxigraph', 'fluree', 'aux']:
        p = link / 'bin' / binary
        p.write_text('#!/bin/sh\\nexit 0\\n')
        p.chmod(0o755)
""")
        nix.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        CALLS=str(self.calls), ROOT=str(ROOT), WORK=str(self.work))
        for key in ("FLUREE_VERSION", "FLUREE", "QINDEX", "QSERVER", "TDBLOADER", "FUSEKI", "OXIGRAPH"):
            self.env.pop(key, None)

    def shell(self, command, **env):
        return subprocess.run(["bash", "-euc", 'source "$1"; ' + command, "bash", str(TOOLS)],
                              env=self.env | env, capture_output=True, text=True)

    def nix_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []

    def test_engines_ignore_path_and_keep_roots(self):
        for package, binary in [("qlever", "qlever-server"), ("apache-jena", "tdb2.tdbloader"),
                                ("apache-jena-fuseki", "fuseki-server"), ("oxigraph", "oxigraph"),
                                ("fluree", "fluree")]:
            with self.subTest(package=package):
                on_path = self.bin / binary
                on_path.write_text("#!/bin/sh\nexit 99\n")
                on_path.chmod(0o755)
                p = self.shell(f'nixbin {package} {binary}')
                self.assertEqual(p.returncode, 0, p.stderr)
                self.assertEqual(p.stdout.strip(), str(self.work / "gcroots" / package / "bin" / binary))
                call = self.nix_calls()[-1]
                self.assertIn(f"{ROOT}#{package}", call)
                self.assertIn("--no-update-lock-file", call)
                self.assertEqual(call[call.index("--inputs-from") + 1], str(ROOT))

    def test_auxiliary_tool_uses_path_or_locked_nixpkgs(self):
        p = self.shell("nixbin unused bash")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.nix_calls(), [])
        p = self.shell("nixbin auxiliary aux")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("nixpkgs#auxiliary", self.nix_calls()[-1])
        self.assertIn("--inputs-from", self.nix_calls()[-1])

    def test_failed_build_never_returns_a_binary(self):
        p = self.shell('binary=$(nixbin qlever qlever-server); echo "started $binary"', FAIL_NIX="1")
        self.assertEqual(p.returncode, 42)
        self.assertEqual(p.stdout, "")

    def test_missing_binary_is_an_error(self):
        p = self.shell("nixbin qlever qlever-server", MISSING_BIN="1")
        self.assertNotEqual(p.returncode, 0)
        self.assertEqual(p.stdout, "")
        self.assertIn("does not provide", p.stderr)

    def test_explicit_engine_overrides_skip_nix(self):
        p = self.shell('ENGINES="jena qlever oxigraph fluree"; '
                       'has() { [[ " $ENGINES " == *" $1 "* ]]; }; resolve_tools',
                       TDBLOADER="/custom/loader", FUSEKI="/custom/fuseki", QINDEX="/custom/index",
                       QSERVER="/custom/server", OXIGRAPH="/custom/oxigraph", FLUREE="/custom/fluree",
                       FLUREE_VERSION="other", FAIL_NIX="1")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.nix_calls(), [])

    def test_resolve_tools_propagates_build_failure(self):
        p = self.shell('has() { [[ "$1" == qlever ]]; }; resolve_tools; echo started', FAIL_NIX="1")
        self.assertEqual(p.returncode, 42)
        self.assertEqual(p.stdout, "")

    def test_fluree_version_must_match_pin(self):
        p = self.shell("fluree_bin", FLUREE_VERSION="4.2.2")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.calls.unlink()
        p = self.shell("fluree_bin", FLUREE_VERSION="4.1.0")
        self.assertNotEqual(p.returncode, 0)
        self.assertEqual(p.stdout, "")
        self.assertIn("set FLUREE", p.stderr)
        self.assertEqual(len(self.nix_calls()), 1)

    def test_text_runner_stops_when_qlever_build_fails(self):
        p = subprocess.run(["bash", str(ROOT / "scripts/bench-text.sh"), "1", str(self.work)],
                           env=self.env | {"ENGINES": "qlever", "FAIL_NIX": "1", "CLIENT_CPU": "none"},
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 42, p.stderr)
        self.assertIn(f"{ROOT}#qlever", self.nix_calls()[-1])
        self.assertFalse((self.work / "data.nt").exists())

    def test_watdiv_runner_uses_pinned_qlever(self):
        # Reuse prepared generator output so only the runner's engine setup executes.
        (self.work / "gen").mkdir()
        (self.work / "queries").mkdir()
        (self.work / "data.nt").write_text("<urn:s> <urn:p> <urn:o> .\n")
        (self.work / "gen/saved.txt").touch()
        (self.work / "gen/params").write_text("scale=1 seed=1\n")
        (self.work / "queries/instances").write_text("scale=1 seed=1 instances=1 draws=4\nC1-1\n")
        p = subprocess.run(["bash", str(ROOT / "scripts/bench-watdiv.sh"), "1", str(self.work)],
                           env=self.env | {"ENGINES": "qlever", "FAIL_NIX": "1", "CLIENT_CPU": "none",
                                           "WATDIV": str(self.work), "INSTANCES": "1", "WATDIV_SEED": "1"},
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 42, p.stderr)
        self.assertIn(f"{ROOT}#qlever", self.nix_calls()[-1])

    def test_billion_runner_uses_packaged_fluree(self):
        zstd = self.bin / "zstd"
        zstd.write_text("#!/bin/sh\nexit 0\n")
        zstd.chmod(0o755)
        p = subprocess.run(["bash", str(ROOT / "scripts/bench-billion.sh"), "report"],
                           env=self.env | {"ENGINES": "fluree", "FAIL_NIX": "1", "WORKDIR": str(self.work)},
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 42, p.stderr)
        self.assertIn(f"{ROOT}#fluree", self.nix_calls()[-1])


if __name__ == "__main__":
    unittest.main()
