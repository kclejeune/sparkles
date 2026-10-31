"""Exercise package selection without building packages or starting benchmark servers."""

import json
import os
from pathlib import Path
import shutil
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
        self.store_tmp = tempfile.TemporaryDirectory(prefix="bench-tools-store-", dir="/tmp")
        self.addCleanup(self.store_tmp.cleanup)
        self.store = Path(self.store_tmp.name)
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
# Like zstd, a multi-output package installs its bin and man outputs. Nix roots each one as
# <link>-<output> and prints one path per output, here with the man output first.
outputs = ['man', 'bin'] if os.environ.get('MULTI_OUTPUT') else ['']
for output in outputs:
    suffix = '-' + output if output else ''
    store = pathlib.Path(os.environ['STORE_ROOT']) / (link.name + suffix)
    store.mkdir(parents=True, exist_ok=True)
    out_link = link.with_name(link.name + suffix)
    if not out_link.is_symlink():
        out_link.symlink_to(store)
    if output == 'man':
        (store / 'share' / 'man').mkdir(parents=True, exist_ok=True)
    else:
        (store / 'bin').mkdir(exist_ok=True)
        if not os.environ.get('MISSING_BIN'):
            for binary in ['qlever-index', 'qlever-server', 'tdb2.tdbloader', 'fuseki-server', 'oxigraph',
                           'fluree', 'aux', 'time', 'hyperfine']:
                p = store / 'bin' / binary
                p.write_text('#!/bin/sh\\nexit 0\\n')
                p.chmod(0o755)
    print(store)
""")
        nix.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        CALLS=str(self.calls), STORE_ROOT=str(self.store), ROOT=str(ROOT), WORK=str(self.work))
        for key in ("FLUREE_VERSION", "FLUREE", "QINDEX", "QSERVER", "TDBLOADER", "FUSEKI", "OXIGRAPH", "HYPERFINE"):
            self.env.pop(key, None)

    def shell(self, command, **env):
        return subprocess.run(["bash", "-euc", 'source "$1"; ' + command, "bash", str(TOOLS)],
                              env=self.env | env, capture_output=True, text=True)

    def nix_calls(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()] if self.calls.exists() else []

    def test_engines_ignore_path_and_keep_roots(self):
        for package, binary in [("qlever", "qlever-server"), ("apache-jena", "tdb2.tdbloader"),
                                ("apache-jena-fuseki", "fuseki-server"), ("oxigraph", "oxigraph"),
                                ("fluree", "fluree"), ("hyperfine", "hyperfine")]:
            with self.subTest(package=package):
                on_path = self.bin / binary
                on_path.write_text("#!/bin/sh\nexit 99\n")
                on_path.chmod(0o755)
                p = self.shell(f'nixbin {package} {binary}')
                self.assertEqual(p.returncode, 0, p.stderr)
                self.assertEqual(p.stdout.strip(), str(self.store / package / "bin" / binary))
                self.assertEqual((self.work / "gcroots" / package).resolve(), self.store / package)
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

    def test_multi_output_package_selects_the_output_with_the_binary(self):
        # The executable output is not assumed to come first, and every output keeps its root.
        for package, binary in [("auxiliary", "aux"), ("qlever", "qlever-server")]:
            with self.subTest(package=package):
                p = self.shell(f"nixbin {package} {binary}", MULTI_OUTPUT="1")
                self.assertEqual(p.returncode, 0, p.stderr)
                self.assertEqual(p.stdout, f"{self.store / (package + '-bin') / 'bin' / binary}\n")
                for output in ("bin", "man"):
                    self.assertEqual((self.work / "gcroots" / f"{package}-{output}").resolve(),
                                     self.store / f"{package}-{output}")

    def test_multi_output_package_without_the_binary_is_an_error(self):
        p = self.shell("nixbin auxiliary aux", MULTI_OUTPUT="1", MISSING_BIN="1")
        self.assertNotEqual(p.returncode, 0)
        self.assertEqual(p.stdout, "")
        self.assertIn("auxiliary does not provide bin/aux", p.stderr)

    def test_gnu_time_falls_back_to_a_multi_output_package(self):
        if shutil.which("gtime", path=self.env["PATH"]):
            self.skipTest("gtime on PATH takes precedence over the fallback")
        p = self.shell("gnu_time /nonexistent/time", MULTI_OUTPUT="1")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout, f"{self.store / 'time-bin' / 'bin' / 'time'}\n")
        self.assertIn("nixpkgs#time", self.nix_calls()[-1])
        p = self.shell('t=$(gnu_time /nonexistent/time); echo "timed with $t"', FAIL_NIX="1")
        self.assertEqual(p.returncode, 42)
        self.assertEqual(p.stdout, "")

    def test_engine_paths_are_safe_in_serialized_commands(self):
        # The runners embed executables in shell command strings passed to hyperfine.
        p = self.shell('binary=$(nixbin qlever qlever-index); bash -c "$binary --help"')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertTrue((self.work / "gcroots/qlever").is_symlink())

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

    def test_use_hyperfine_puts_the_flake_hyperfine_first(self):
        on_path = self.bin / "hyperfine"
        on_path.write_text("#!/bin/sh\nexit 99\n")
        on_path.chmod(0o755)
        p = self.shell('use_hyperfine; readlink "$(command -v hyperfine)"; hyperfine')
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.strip(), str(self.store / "hyperfine" / "bin" / "hyperfine"))
        self.assertIn(f"{ROOT}#hyperfine", self.nix_calls()[-1])

    def test_hyperfine_override_runs_under_any_name(self):
        on_path = self.bin / "hyperfine"
        on_path.write_text("#!/bin/sh\nexit 99\n")
        on_path.chmod(0o755)
        chosen = self.work / "hyperfine-1.20"
        chosen.write_text("#!/bin/sh\necho chosen\n")
        chosen.chmod(0o755)
        p = self.shell("use_hyperfine; hyperfine", HYPERFINE=str(chosen), FAIL_NIX="1")
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(p.stdout.strip(), "chosen")
        self.assertEqual(self.nix_calls(), [])

    def test_hyperfine_override_must_be_an_absolute_executable_file(self):
        tools = self.work / "tools"
        tools.mkdir()
        timer = tools / "timer"
        timer.write_text("#!/bin/sh\nexit 0\n")
        timer.chmod(0o755)
        for override in ["tools/timer", "timer", str(self.work / "missing"), str(tools)]:
            with self.subTest(override=override):
                p = subprocess.run(["bash", "-euc", 'source "$1"; use_hyperfine; echo started', "bash", str(TOOLS)],
                                   env=self.env | {"HYPERFINE": override, "FAIL_NIX": "1", "PATH": f"tools:{self.env['PATH']}"},
                                   cwd=self.work, capture_output=True, text=True)
                self.assertNotEqual(p.returncode, 0)
                self.assertEqual(p.stdout, "")
                self.assertIn("absolute path of an executable file", p.stderr)
        self.assertEqual(self.nix_calls(), [])

    def test_resolve_tools_leaves_the_timer_to_runners_that_time(self):
        p = self.shell("has() { false; }; resolve_tools", FAIL_NIX="1")
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

    def test_text_loader_uses_the_packaged_jdk(self):
        (self.work / "data.nt").write_text("<urn:s> <urn:p> <urn:o> .\n")
        loader = self.store / "loader"
        loader.write_text('#!/bin/sh\n[ -z "${JAVA_HOME+x}" ] || exit 74\nexit 43\n')
        loader.chmod(0o755)
        p = subprocess.run(["bash", str(ROOT / "scripts/bench-text.sh"), "1", str(self.work)],
                           env=self.env | {"ENGINES": "jena", "CLIENT_CPU": "none", "JAVA_HOME": "/old-jdk",
                                           "TDBLOADER": str(loader), "FUSEKI": str(loader)},
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 43, p.stderr)

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
