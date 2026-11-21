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
# A module runs git when it names the program as an argument or runs a git subcommand in a
# shell snippet.
RUNS_GIT = re.compile(r"""['"]git['"]|\bgit (-C|init|commit|add|fetch|rev-parse|worktree|ls-files|config|clone|push|"""
                      r"""bundle|diff|merge-base|log|show|status|checkout|tag|update-ref|cat-file)\b""")
IMPORT = 'import git_isolation'
# Fixture tests that create repositories and commit in them, run under the hostile settings.
PROBES = ['test_remote_ci.RemoteCI.test_a_failing_ci_returns_its_exit_code',
          'test_changed_crates.RenamedFiles']


def git(*args, env=None):
    return subprocess.run(['git', *args], check=True, capture_output=True, text=True, env=env).stdout


class Isolation(unittest.TestCase):
    def test_every_module_that_runs_git_imports_the_isolation_first(self):
        missing = []
        for path in sorted(HERE.glob('test_*.py')):
            text = path.read_text()
            if not RUNS_GIT.search(text):
                continue
            first = next((line for line in text.splitlines() if re.match(r'(import|from) ', line)), '')
            if not first.startswith(IMPORT):
                missing.append(path.name)
        self.assertEqual(missing, [])

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
            config.write_text(f'[core]\n\thooksPath = {hooks}\n[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = false\n')
            hostile = dict(os.environ, GIT_DIR=str(decoy / '.git'), GIT_WORK_TREE=str(decoy),
                           GIT_CONFIG_GLOBAL=str(config), GIT_CONFIG_COUNT='1',
                           GIT_CONFIG_KEY_0='core.hooksPath', GIT_CONFIG_VALUE_0=str(hooks))
            code = "import sys, unittest; sys.path.insert(0, sys.argv[1]); unittest.main(module=None, argv=['unittest', *sys.argv[2:]])"
            r = subprocess.run([sys.executable, '-c', code, str(HERE), *PROBES], env=hostile,
                               capture_output=True, text=True, timeout=300)
            self.assertEqual(r.returncode, 0, r.stderr[-4000:])
            self.assertEqual(git('-C', str(decoy), 'for-each-ref', '--format=%(refname) %(objectname)', env=clean), before)
            self.assertEqual(sorted(p.name for p in (decoy / '.git/objects').rglob('*')), objects)
            self.assertEqual(git('-C', str(decoy), 'status', '--porcelain', env=clean), '')
            self.assertFalse(marker.exists(), marker.read_text() if marker.exists() else '')


if __name__ == '__main__':
    unittest.main()
