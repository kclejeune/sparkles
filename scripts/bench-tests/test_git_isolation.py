"""Tests of git_isolation.py: the script tests' fixtures ignore host Git settings."""
import git_isolation  # noqa: F401, first, so no fixture sees the host Git settings
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
IMPORT = 'import git_isolation'
# Fixture tests run under the hostile settings: two that create repositories and commit in
# them, and one that runs a script whose own git commands read the checkout.
PROBES = ['test_remote_ci.RemoteCI.test_a_failing_ci_returns_its_exit_code',
          'test_changed_crates.RenamedFiles',
          'test_bench_record.RecordTests.test_manifest']


def isolated_first(text):
    """Whether the first import of a module's TEXT, after any __future__ import, is the
    Git isolation. Every test module needs it, since a test can run git through a script
    as well as directly."""
    for line in text.splitlines():
        if re.match(r'(import|from) ', line) and not line.startswith('from __future__'):
            return line.startswith(IMPORT)
    return False


def git(*args, env=None):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True, env=env).stdout


class Isolation(unittest.TestCase):
    def test_every_test_module_imports_the_isolation_first(self):
        missing = [p.name for p in sorted(HERE.glob('test_*.py')) if not isolated_first(p.read_text())]
        self.assertEqual(missing, [])

    def test_the_guard_rejects_a_module_that_runs_git_only_through_a_script(self):
        self.assertFalse(isolated_first('"""Doc."""\nimport subprocess\nsubprocess.run(["scripts/x.sh"])\n'))
        self.assertFalse(isolated_first('import os\nimport git_isolation\n'))
        self.assertTrue(isolated_first('"""Doc."""\nfrom __future__ import annotations\nimport git_isolation  # noqa\nimport os\n'))

    def test_the_scrubbed_environment_drops_host_git_settings(self):
        env = git_isolation.isolate({'GIT_DIR': '/elsewhere', 'GIT_WORK_TREE': '/elsewhere', 'GIT_CONFIG_COUNT': '1',
                                     'GIT_CONFIG_KEY_0': 'core.hooksPath', 'GIT_CONFIG_VALUE_0': '/hooks',
                                     'GIT_INDEX_FILE': '/index', 'HOME': '/home/x'})
        self.assertEqual(env['HOME'], '/home/x')
        for name in ('GIT_DIR', 'GIT_WORK_TREE', 'GIT_CONFIG_COUNT', 'GIT_CONFIG_KEY_0', 'GIT_INDEX_FILE'):
            self.assertNotIn(name, env)
        self.assertEqual(env['GIT_CONFIG_GLOBAL'], os.devnull)
        self.assertEqual(env['GIT_CONFIG_NOSYSTEM'], '1')
        self.assertEqual(os.listdir(env['GIT_TEMPLATE_DIR']), [])

    def test_fixtures_under_a_hostile_git_dir_and_global_config_leave_both_alone(self):
        with tempfile.TemporaryDirectory() as d:
            d = Path(d)
            decoy = d / 'decoy'
            clean = dict(git_isolation.isolate(dict(os.environ)))
            git('init', '-q', str(decoy), env=clean)
            (decoy / 'a').write_text('a\n')
            git('-C', str(decoy), 'add', 'a', env=clean)
            git('-C', str(decoy), 'commit', '-q', '--no-gpg-sign', '-m', 'decoy', env=clean)
            before = git('-C', str(decoy), 'for-each-ref', '--format=%(refname) %(objectname)', env=clean)
            objects = sorted(p.name for p in (decoy / '.git/objects').rglob('*'))
            hooks = d / 'hooks'
            hooks.mkdir()
            marker = d / 'hook-ran'
            for name in ('pre-commit', 'post-commit', 'commit-msg', 'post-checkout'):
                (hooks / name).write_text(f'#!/bin/sh\necho {name} >> {marker}\n')
                (hooks / name).chmod(0o755)
            config = d / 'gitconfig'
            # A status or diff runs the fsmonitor hook, which is how a script that only reads
            # the checkout would run host code.
            fsmonitor = d / 'fsmonitor'
            fsmonitor.write_text(f'#!/bin/sh\necho fsmonitor >> {marker}\n')
            fsmonitor.chmod(0o755)
            config.write_text(f'[core]\n\thooksPath = {hooks}\n\tfsmonitor = {fsmonitor}\n[commit]\n\tgpgsign = true\n'
                              f'[gpg]\n\tprogram = false\n')
            hostile = dict(os.environ, GIT_DIR=str(decoy / '.git'), GIT_WORK_TREE=str(decoy),
                           GIT_CONFIG_GLOBAL=str(config), GIT_CONFIG_COUNT='1',
                           GIT_CONFIG_KEY_0='core.hooksPath', GIT_CONFIG_VALUE_0=str(hooks))
            code = "import sys, unittest; sys.path.insert(0, sys.argv[1]); unittest.main(module=None, argv=['unittest', *sys.argv[2:]])"
            # Each probe runs alone, as a targeted run would, so no other module's import
            # isolates it.
            for probe in PROBES:
                r = subprocess.run([sys.executable, '-c', code, str(HERE), probe], env=hostile,
                                   capture_output=True, text=True, timeout=300)
                self.assertEqual(r.returncode, 0, r.stderr[-4000:])
                self.assertFalse(marker.exists(), probe + ': ' + (marker.read_text() if marker.exists() else ''))
            self.assertEqual(git('-C', str(decoy), 'for-each-ref', '--format=%(refname) %(objectname)', env=clean), before)
            self.assertEqual(sorted(p.name for p in (decoy / '.git/objects').rglob('*')), objects)
            self.assertEqual(git('-C', str(decoy), 'status', '--porcelain', env=clean), '')
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else '')


if __name__ == '__main__':
    unittest.main()
