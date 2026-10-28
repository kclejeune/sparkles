#!/usr/bin/env python3
"""Rehydrate mbx 1.28.0 build-script helpers after a hosted dependency-cache restore."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

MBX_VERSION = "1.28.0"
LAUNCHER = re.compile(
    rb'\A#!/bin/sh\nMBX_BUILD_SCRIPT_SHIM_PATH="\$0" exec '
    rb'"\$\(dirname "\$0"\)/(?P<relative>\.\./\.\./\.mbx-build-script-shims/'
    rb'[0-9a-f]{64}/mbx)" "\$@"\n\Z'
)


def repair_launchers(target, binary):
    """Validate cached launchers without executing them, then publish fresh helpers."""
    target = Path(target)
    if target.is_symlink():
        raise ValueError("Swatinem must own an ordinary target directory")
    if not target.exists():
        return {"launchers": 0, "helpers": 0}
    if not target.is_dir():
        raise ValueError("Swatinem must own an ordinary target directory")
    target = target.resolve()
    destinations = set()
    launchers = 0
    for root, directories, files in os.walk(target, followlinks=False):
        root = Path(root)
        # Cargo profile/build directories and the helper tree must be ordinary paths.
        # Do not follow cached directory symlinks to discover executable pairs.
        for name in directories:
            path = root / name
            if name.endswith(".mbx-real"):
                raise ValueError(f"Unsupported cached build-script executable: {path}")
            if path.is_symlink() and (
                root == target or root.name == "build"
                or name in ("build", ".mbx-build-script-shims")
            ):
                raise ValueError(f"Unexpected symlink at a cached build-script directory: {path}")
        for name in files:
            if not name.endswith(".mbx-real"):
                continue
            real = root / name
            launcher = real.with_name(name.removesuffix(".mbx-real"))
            if real.is_symlink() or launcher.is_symlink():
                raise ValueError("Unexpected symlink at a cached build-script executable")
            if not real.is_file() or not launcher.is_file():
                raise ValueError(f"Missing cached build-script executable pair: {real}")
            if launcher.parent.parent.name != "build":
                raise ValueError(f"Unsupported cached build-script layout: {launcher}")
            # A supported launcher is tiny. Never read arbitrary binaries into memory.
            with launcher.open("rb") as stream:
                data = stream.read(4097)
            match = LAUNCHER.fullmatch(data)
            if not match:
                raise ValueError(f"Unsupported cached build-script launcher: {launcher}")
            lexical = launcher.parent / match["relative"].decode("ascii")
            destination = lexical.resolve()
            if not destination.is_relative_to(target):
                raise ValueError("Cached helper path escapes target")
            # Refuse even internal symlinks rather than replacing their destinations.
            normalized = Path(os.path.abspath(lexical))
            if destination != normalized or destination.is_symlink():
                raise ValueError("Unexpected symlink at a cached helper path")
            if destination.exists() and not destination.is_file():
                raise ValueError(f"Unsupported cached helper destination: {destination}")
            destinations.add(destination)
            launchers += 1
    # Validate every launcher before changing anything. Replacing a copied inode also
    # avoids overwriting a cached helper that is hard-linked to another store entry.
    for destination in sorted(destinations):
        destination.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=destination.parent) as temporary:
            staged = Path(temporary) / "mbx"
            shutil.copyfile(binary, staged)
            staged.chmod(0o755)
            os.replace(staged, destination)
    return {"launchers": launchers, "helpers": len(destinations)}


def main():
    binary = shutil.which("mbx")
    if binary is None:
        raise ValueError("The pinned mbx must be installed before cache hydration")
    version = subprocess.check_output([binary, "--version"], text=True).strip()
    if version != f"mbx {MBX_VERSION}":
        raise ValueError(f"Unsupported mbx version for cached launchers: {version}")
    receipt = repair_launchers("target", Path(binary).resolve())
    print(f"Rehydrated {receipt['helpers']} mbx helpers for {receipt['launchers']} cached build scripts")


if __name__ == "__main__":
    main()
