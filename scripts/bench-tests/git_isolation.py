"""Isolate the Git that script tests and their fixtures run from the host's Git settings.

Every test module imports this module first, since a test can run git through a script as
well as directly, and a targeted run of one module must be isolated too. The import scrubs
the process environment that every subprocess inherits. It removes every GIT_* variable, so
a GIT_DIR or GIT_WORK_TREE set by a hook or a shell cannot make a fixture's `git init` or
commit land in another repository, and GIT_CONFIG_* cannot add settings. It sets
GIT_CONFIG_GLOBAL=/dev/null and GIT_CONFIG_NOSYSTEM=1, so the user's and the system's
configuration, with core.hooksPath, commit signing and init.templateDir, are not read, and
it points GIT_TEMPLATE_DIR at an empty directory, so a new repository gets no hooks. A
fixed author and committer identity replaces the one the global configuration gave.
Repository-local configuration still applies, so tests that read the checkout itself see
its own settings.

test_git_isolation.py checks that every test module imports this module first, and that
fixture tests run alone under a hostile GIT_DIR and global configuration leave both alone.
"""
import atexit
import os
import shutil
import tempfile

IDENTITY = {'GIT_AUTHOR_NAME': 'fixture', 'GIT_AUTHOR_EMAIL': 'fixture@invalid',
            'GIT_COMMITTER_NAME': 'fixture', 'GIT_COMMITTER_EMAIL': 'fixture@invalid'}

_template = None


def isolate(environ=os.environ):
    """Scrub ENVIRON of host Git settings in place and return it. Repeated calls reuse
    the first call's empty template directory."""
    global _template
    for name in [n for n in environ if n.startswith('GIT_')]:
        del environ[name]
    if _template is None or not os.path.isdir(_template):
        _template = tempfile.mkdtemp(prefix='git-template-')
        atexit.register(shutil.rmtree, _template, True)
    environ.update({'GIT_CONFIG_GLOBAL': os.devnull, 'GIT_CONFIG_NOSYSTEM': '1',
                    'GIT_TEMPLATE_DIR': _template}, **IDENTITY)
    return environ


isolate()
