"""The comparison binary must be copied from its own target under one lease."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class NativeBuildContract(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="sparkles-nsc-build-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.project = self.root / "source"
        self.project.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        fake_mbx = self.bin / "mbx"
        fake_mbx.write_text("""#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys
root=pathlib.Path(os.environ['BUILD_TEST_ROOT'])
args=sys.argv[1:]
with (root/'calls.jsonl').open('a') as stream:stream.write(json.dumps(args)+'\\n')
if args[:2]==['sparkles-task','--']:
    assert 'CARGO_TARGET_DIR' not in os.environ
    (root/'lease').touch()
    try:result=subprocess.run(args[2:]).returncode
    finally:(root/'lease').unlink()
    raise SystemExit(result)
assert args[:4]==['build','--release','-p','sparkles-server']
assert (root/'lease').exists(), 'build is outside target lease'
assert 'CARGO_TARGET_DIR' not in os.environ, 'shared target override survived'
if os.environ.get('BUILD_TEST_FAIL'):raise SystemExit(17)
target=root/'managed view'
(target/'release').mkdir(parents=True,exist_ok=True)
pathlib.Path('target').symlink_to(target,target_is_directory=True)
(target/'release/sparkles').write_bytes(b'complete native binary')
""")
        fake_mbx.chmod(0o755)
        copy = self.bin / "cp"
        copy.write_text("""#!/usr/bin/env python3
import os,pathlib,subprocess,sys
assert (pathlib.Path(os.environ['BUILD_TEST_ROOT'])/'lease').exists(), 'copy is outside lease'
raise SystemExit(subprocess.run([os.environ['BUILD_TEST_CP'],*sys.argv[1:]]).returncode)
""")
        copy.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        BUILD_TEST_ROOT=str(self.root), BUILD_TEST_CP=shutil.which("cp"),
                        CARGO_TARGET_DIR=str(self.root / "old shared target"))
        self.env.pop("CARGO_BUILD_TARGET", None)
        self.output = self.root / "staged binaries" / "sparkles"

    def build(self, *args, env=None):
        return subprocess.run(["bash", str(ROOT / "scripts/nsc-build.sh"), str(self.output), *args],
                              cwd=self.project, env=env or self.env, capture_output=True,
                              text=True, timeout=15)

    def test_default_target_and_copy_share_the_lease(self):
        result = self.build("--features", "text,geo", "--locked")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.output.read_bytes(), b"complete native binary")
        self.assertTrue((self.project / "target").is_symlink())
        self.assertFalse((self.root / "lease").exists())
        calls = [json.loads(line) for line in (self.root / "calls.jsonl").read_text().splitlines()]
        self.assertEqual(calls[1], ["build", "--release", "-p", "sparkles-server",
                                   "--features", "text,geo", "--locked"])

    def test_failed_build_cannot_replace_the_cached_binary(self):
        self.output.parent.mkdir()
        self.output.write_bytes(b"previous successful binary")
        result = self.build(env=dict(self.env, BUILD_TEST_FAIL="1"))
        self.assertEqual(result.returncode, 17, result.stderr)
        self.assertEqual(self.output.read_bytes(), b"previous successful binary")
        self.assertFalse(Path(str(self.output) + ".tmp").exists())
        self.assertFalse((self.root / "lease").exists())

    def test_output_overrides_refuse_before_any_build(self):
        for option in ("--target-dir=x", "--target-dir", "--target=x", "--profile=x",
                       "--manifest-path=x", "--config=build.target-dir=x",
                       "--config", "--config=alternate.toml",
                       "--config=build.target-dir = 'elsewhere'", "--example=other",
                       "--lib", "--package=other", "-Zbuild-std", "--artifact-dir=elsewhere",
                       "--features", "--features="):
            result = self.build(option)
            self.assertEqual(result.returncode, 2, (option, result.stderr))
        self.assertFalse((self.root / "calls.jsonl").exists())
        result = self.build(env=dict(self.env, CARGO_BUILD_TARGET="other-platform"))
        self.assertEqual(result.returncode, 2)
        self.assertFalse((self.root / "calls.jsonl").exists())


class RevisionBinaryCache(unittest.TestCase):
    """A cached revision binary is reused only for the cargo arguments it was built with."""

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="sparkles-nsc-cache-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        for name in ("nsc-bench.sh", "nsc-build.sh"):
            shutil.copy(ROOT / "scripts" / name, self.repo / "scripts" / name)
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
               "-c", "commit.gpgsign=false"]
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        subprocess.run([*git, "add", "."], check=True)
        subprocess.run([*git, "commit", "-q", "-m", "fixture"], check=True)
        self.sha = subprocess.run([*git, "rev-parse", "HEAD"], check=True, capture_output=True,
                                  text=True).stdout.strip()
        self.cache = self.root / "cache"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        tools = {
            # The build writes its own arguments into the binary.
            "mbx": """#!/usr/bin/env python3
import json,os,pathlib,subprocess,sys
args=sys.argv[1:]
with open(os.environ['CACHE_TEST_CALLS'],'a') as stream:stream.write(json.dumps(args)+'\\n')
if args[:2]==['sparkles-task','--']:raise SystemExit(subprocess.run(args[2:]).returncode)
pathlib.Path('target/release').mkdir(parents=True,exist_ok=True)
pathlib.Path('target/release/sparkles').write_text('built with '+' '.join(args[4:]))
pathlib.Path('target/release/sparkles').chmod(0o755)
""",
            # Stop the run at the compiler check, after both binaries are staged.
            "readelf": "#!/bin/sh\nexit 0\n",
            "nsc": "#!/bin/sh\necho 'nsc must not run' >&2\nexit 99\n",
            "cargo": "#!/bin/sh\nexit 0\n",
            "rustc": "#!/bin/sh\necho 'rustc test'\n",
            "zstd": """#!/usr/bin/env python3
import shutil,sys
a=sys.argv[1:];shutil.copyfile([x for x in a if not x.startswith('-') and x!=a[a.index('-o')+1]][0],a[a.index('-o')+1])
""",
        }
        for name, text in tools.items():
            (self.bin / name).write_text(text)
            (self.bin / name).chmod(0o755)
        self.calls = self.root / "calls.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        SPARKLES_NSC_CACHE=str(self.cache), CACHE_TEST_CALLS=str(self.calls))
        self.env.pop("CARGO_TARGET_DIR", None)
        self.env.pop("CARGO_BUILD_TARGET", None)
        self.runs = 0

    def bench(self, *args):
        self.runs += 1
        out = self.root / f"out-{self.runs}"
        result = subprocess.run(["bash", str(self.repo / "scripts/nsc-bench.sh"), "--out", str(out),
                                 *args, self.sha, self.sha], env=self.env, capture_output=True,
                                text=True, timeout=30)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not built with the same known compilers", result.stderr)
        return (out / "stage/bin-A.zst").read_text(), (out / "manifest.txt").read_text()

    def builds(self):
        if not self.calls.exists():
            return 0
        return sum(json.loads(line)[0] == "build" for line in self.calls.read_text().splitlines())

    def test_other_cargo_args_do_not_reuse_the_default_binary(self):
        binary, _ = self.bench()
        self.assertEqual(binary, "built with ")
        self.assertEqual(self.builds(), 1)
        binary, manifest = self.bench("--cargo-args", "--features text")
        self.assertEqual(binary, "built with --features text")
        self.assertIn("cargo --features text", manifest)
        self.assertEqual(self.builds(), 2)
        # Each entry is then reused for its own arguments.
        self.assertEqual(self.bench()[0], "built with ")
        self.assertEqual(self.bench("--cargo-args", "--features text")[0], "built with --features text")
        self.assertEqual(self.builds(), 2)

    def test_existing_default_entry_is_reused_and_unrecorded_entry_is_rebuilt(self):
        entry = self.cache / "bin" / self.sha
        entry.mkdir(parents=True)
        (entry / "sparkles").write_text("earlier default build")
        (entry / "sparkles").chmod(0o755)
        (entry / "build.txt").write_text("built: earlier\ncargo args: \nrustc test\n")
        self.assertEqual(self.bench()[0], "earlier default build")
        self.assertEqual(self.builds(), 0)
        (entry / "build.txt").unlink()
        self.assertEqual(self.bench()[0], "built with ")
        self.assertEqual(self.builds(), 1)
        self.assertIn("cargo args: \n", (entry / "build.txt").read_text())


if __name__ == "__main__":
    unittest.main()
