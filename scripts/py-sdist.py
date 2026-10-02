#!/usr/bin/env python3
"""Build the source distribution of the Python bindings (`mise run py:sdist`).

`maturin sdist` copies the crate and its path dependencies, but not the vendored
spargebra that the crate's `[patch.crates-io]` names (vendor/spargebra, see its
PATCHED.md). Without it a build from the sdist fails. This script runs maturin, adds the
vendored crate to the archive next to the others, and points the patch at it.

Usage: scripts/py-sdist.py [out dir]   (default: target/wheels)
"""

from __future__ import annotations

import gzip
import io
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VENDOR = ROOT / "vendor" / "spargebra"
# the patch as the crate's Cargo.toml writes it, and as it reads from the sdist's root
PATCH = 'spargebra = { path = "../../vendor/spargebra" }'
PATCHED = 'spargebra = { path = "../vendor/spargebra" }'


def main() -> None:
    out = Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "target" / "wheels").resolve()
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["maturin", "sdist", "--manifest-path", str(ROOT / "crates/sparkles-py/Cargo.toml"), "--out", tmp],
            check=True,
        )
        (built,) = Path(tmp).glob("*.tar.gz")
        target = out / built.name
        add_vendored(built, target)
    print(f"source distribution with the vendored spargebra: {target}")


def add_vendored(src: Path, dst: Path) -> None:
    buf = io.BytesIO()
    with tarfile.open(src, "r:gz") as tin, tarfile.open(fileobj=buf, mode="w", format=tarfile.PAX_FORMAT) as tout:
        members = tin.getmembers()
        top = members[0].name.split("/")[0]
        if any(m.name.startswith(f"{top}/vendor/spargebra/") for m in members):
            sys.exit("maturin now includes vendor/spargebra itself: drop this step")
        found = False
        for m in members:
            data = tin.extractfile(m).read() if m.isfile() else None  # type: ignore[union-attr]
            if m.name == f"{top}/sparkles-py/Cargo.toml":
                text = data.decode()  # type: ignore[union-attr]
                if PATCH not in text:
                    sys.exit(f"the patch line is not in {m.name}: {PATCH}")
                data = text.replace(PATCH, PATCHED).encode()
                m.size = len(data)
                found = True
            tout.addfile(m, io.BytesIO(data) if data is not None else None)
        if not found:
            sys.exit(f"{top}/sparkles-py/Cargo.toml is not in the sdist")
        for path in sorted(VENDOR.rglob("*")):
            if path.is_file():
                tout.add(path, arcname=f"{top}/vendor/spargebra/{path.relative_to(VENDOR).as_posix()}")
    # a fixed mtime keeps the archive reproducible for the same inputs
    with open(dst, "wb") as f, gzip.GzipFile(fileobj=f, mode="wb", mtime=0, filename="") as gz:
        gz.write(buf.getvalue())


if __name__ == "__main__":
    main()
