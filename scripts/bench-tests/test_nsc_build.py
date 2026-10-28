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


if __name__ == "__main__":
    unittest.main()
