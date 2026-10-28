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
    # runtime names the stubs leave out on purpose: the unpickling helper, and the
    # module's generated __all__
    allowlist = tmp_path / "allowlist.txt"
    allowlist.write_text("sparkles._sparkles._literal\nsparkles._sparkles.__all__\n")
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
            "--allowlist",
            str(allowlist),
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


def test_async_consumer_types(tmp_path: Path) -> None:
    """An installed package accepts async use and refuses synchronous misuse."""
    source = tmp_path / "async_consumer.py"
    source.write_text(
        "from sparkles import QuerySolutions, QuerySolution, Quad, UpdateStats, aio\n"
        "async def consume() -> None:\n"
        "    async with aio.Dataset() as ds:\n"
        "        rows: QuerySolutions = await ds.select('SELECT * {}')\n"
        "        changed: UpdateStats = await ds.update('INSERT DATA {}')\n"
        "        async with (await ds.select_cursor('SELECT * {}')) as cursor:\n"
        "            async for row in cursor:\n"
        "                selected: QuerySolution = row\n"
        "        async with (await ds.graph_cursor('CONSTRUCT {} WHERE {}')) as graph:\n"
        "            async for quad in graph:\n"
        "                item: Quad = quad\n"
        "        async with ds.transaction() as tx:\n"
        "            staged: UpdateStats = await tx.update('INSERT DATA {}')\n"
    )
    env = dict(
        os.environ,
        MYPY_CACHE_DIR=str(tmp_path / "mypy-cache"),
        MYPYPATH=str(Path(sparkles.__file__).parent.parent),
    )
    command = [sys.executable, "-m", "mypy", "--no-incremental", str(source)]
    result = subprocess.run(
        command, capture_output=True, text=True, env=env, check=False, timeout=30
    )
    assert result.returncode == 0, result.stdout + result.stderr
    source.write_text(
        "from sparkles import aio\n"
        "async def wrong(ds: aio.Dataset, cursor: aio.QueryCursor) -> None:\n"
        "    await ds.select(123)\n"
        "    for row in cursor:\n"
        "        pass\n"
        "    with ds:\n"
        "        pass\n"
    )
    result = subprocess.run(
        command, capture_output=True, text=True, env=env, check=False, timeout=30
    )
    assert result.returncode == 1, result.stdout + result.stderr
    assert 'Argument 1 to "select"' in result.stdout
    assert 'has no attribute "__iter__"' in result.stdout
    assert 'has no attribute "__enter__"' in result.stdout
