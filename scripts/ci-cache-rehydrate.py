#!/usr/bin/env python3
"""Rehydrate mbx 1.28.0 build-script helpers after a hosted dependency-cache restore."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

MBX_VERSION = "1.28.0"
CARGO_EXECUTABLE = re.compile(r"(?:build-script-|build_script_)[A-Za-z0-9_-]+")
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
    candidates = set()
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
        # Discover both sides: an orphan launcher has no .mbx-real name to find.
        # Cargo's executable names exclude sidecars and nested generated outputs.
        if root.parent.name == "build":
            candidates.update(
                root / name for name in files + directories if CARGO_EXECUTABLE.fullmatch(name)
            )
        candidates.update(
            root / name.removesuffix(".mbx-real") for name in files if name.endswith(".mbx-real")
        )
    destinations = set()
    launchers = 0
    for launcher in sorted(candidates):
        real = launcher.with_name(launcher.name + ".mbx-real")
        if real.is_symlink() or launcher.is_symlink():
            raise ValueError("Unexpected symlink at a cached build-script executable")
        if not launcher.is_file():
            raise ValueError(f"Missing cached build-script executable pair: {launcher}")
        if launcher.parent.parent.name != "build":
            raise ValueError(f"Unsupported cached build-script layout: {launcher}")
        # Read only enough to recognize a native ELF or the tiny supported launcher.
        with launcher.open("rb") as stream:
            data = stream.read(4097)
        if not real.exists() and data.startswith(b"\x7fELF"):
            continue
        match = LAUNCHER.fullmatch(data)
        if not match:
            raise ValueError(f"Unsupported cached build-script launcher: {launcher}")
        if not real.exists():
            # Cargo's build-script-build is an alias of rustc's hashed
            # build_script_build-<unit> launcher. mbx 1.28 preserves the real
            # binary only beside the hashed output and resolves this unique
            # sibling when Cargo invokes the alias. Match that lookup without
            # accepting a missing executable or unrelated launcher as a pair.
            prefix = launcher.name.replace("-", "_") + "-"
            siblings = [
                path for path in candidates
                if path.parent == launcher.parent and path.name.startswith(prefix)
                and (path.with_name(path.name + ".mbx-real").exists()
                     or path.with_name(path.name + ".mbx-real").is_symlink())
            ]
            if len(siblings) > 1:
                raise ValueError(f"Ambiguous cached build-script executable pair: {launcher}")
            if siblings:
                sibling = siblings[0]
                if not re.fullmatch(r"[0-9a-f]+", sibling.name[len(prefix):]):
                    raise ValueError(f"Unsupported cached build-script sibling: {sibling}")
                real = sibling.with_name(sibling.name + ".mbx-real")
                if sibling.is_symlink() or real.is_symlink():
                    raise ValueError("Unexpected symlink at a cached build-script executable")
                if not sibling.is_file():
                    raise ValueError(f"Missing cached build-script executable pair: {sibling}")
                with sibling.open("rb") as stream:
                    sibling_data = stream.read(4097)
                if sibling_data != data:
                    raise ValueError(f"Mismatched cached build-script alias: {launcher}")
        if not real.is_file():
            raise ValueError(f"Missing cached build-script executable pair: {real}")
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
