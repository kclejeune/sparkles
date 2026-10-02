"""The type stubs match the compiled module (A14, first half). Skipped without mypy."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import pytest

import sparkles

pytest.importorskip("mypy")


def test_stubtest(tmp_path: Path) -> None:
    env = dict(os.environ, MYPY_CACHE_DIR=str(tmp_path / "mypy-cache"))
    # the package directory the tests import (the build in target/py or an installation)
    env["PYTHONPATH"] = os.pathsep.join(
        [str(Path(sparkles.__file__).parent.parent), env.get("PYTHONPATH", "")]
    )
    r = subprocess.run(
        [
            sys.executable,
            "-m",
            "mypy.stubtest",
            "sparkles",
            # runtime names the stubs leave out on purpose: the unpickling helper
            "--ignore-missing-stub",
        ],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert r.returncode == 0, r.stdout + r.stderr


def test_package_exports() -> None:
    for name in sparkles.__all__:
        assert hasattr(sparkles, name), name
    assert (Path(sparkles.__file__).parent / "py.typed").exists()
