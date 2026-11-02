#!/usr/bin/env python3
"""Choose the workspace crates whose Clippy and tests a change can affect.

`scripts/ci.sh --changed BASE` runs this to narrow a lane's `lint` and `test` tasks. It
compares the working tree with the merge base of BASE and HEAD, including uncommitted
and untracked files, and maps every changed file to the workspace packages that read it.
A file inside a package's directory belongs to that package. A few files outside the
crates are read by known packages' builds or tests, such as docs/openapi.json and ui/.
Files that no Rust build or test reads, such as most documentation and the scripts, map to
no package. The result then grows by every package that depends on a chosen one, through
normal, build and dev-dependencies, until nothing more is added.

Some files are shared roots that change what every package builds or tests, such as
Cargo.toml, Cargo.lock, rust-toolchain.toml, mise.toml, .config/, testsuite/, vendor/ and
crates/sparkles-core. A change to one of them, or to any file the rules below do not
cover, selects the whole workspace. The batch gate always runs the whole workspace.

Usage:
  changed-crates.py BASE         print `all`, `none` or the chosen packages, one per line,
                                 and the reason for each on standard error
  changed-crates.py --cargo-args [--doc | --nextest]
                                 print the package arguments for SPARKLES_CHANGED_PACKAGES:
                                 `--workspace` when it is unset or `all`, `none` when it is
                                 empty or `none`, and otherwise `-p NAME` for each name.
                                 With --doc only packages with a library target count,
                                 since only those have doctests. With --nextest the build
                                 stays `--workspace`, so the test binaries are the ones a
                                 full run builds and shares through the cache, and a
                                 filterset `-E package(A)|package(B)` picks the tests.
"""

import fnmatch
import json
import os
import re
import shutil
import subprocess
import sys

# Shared roots: a change here selects the whole workspace.
ALL = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "mise.toml",
    "clippy.toml",
    "rustfmt.toml",
    ".mbx.toml",
    ".cargo/*",
    ".config/*",
    "testsuite/*",
    "vendor/*",
    "crates/sparkles-core/*",
]

# Files outside the crates that known packages read in their builds or tests.
READ_BY = [
    ("docs/openapi.json", ["sparkles", "sparkles-client", "sparkles-server"]),
    ("docs/API.md", ["sparkles-server"]),
    ("ui/*", ["sparkles-server", "sparkles-fmt-wasm"]),
    ("scripts/client-types.py", ["sparkles-client"]),
    ("scripts/api-compat.py", ["sparkles-server"]),
    ("scripts/api-compat-tests/*", ["sparkles-server"]),
    ("scripts/bench-lubm/*", ["sparkles-reasoner"]),
    # tests/parity.rs checks that the named bindings exist in these sources
    ("js/engine/src/*", ["sparkles"]),
    ("jvm/sparkles-jena/src/*", ["sparkles"]),
]

# A literal path into another crate's directory, such as `#[path = "../../sparkles-shacl/…"]`
# or `include_str!("../../sparkles-fmt/tests/…")`, makes the reading package depend on
# that crate's files as a Cargo dependency would.
CROSS_READ = re.compile(r'"(?:\.\./)+(sparkles[a-z0-9-]*)/')

# Files outside the crates that no Rust build or test of the main workspace reads.
NO_RUST = [
    "*.md",
    "LICENSE",
    "docs/*",
    "scripts/*",
    ".github/*",
    ".githooks/*",
    "js/*",
    "jvm/*",
    "deploy/*",
    "nix/*",
    "flake.nix",
    "flake.lock",
    "Dockerfile",
    "compose.yaml",
    ".gitignore",
    ".dockerignore",
    ".pre-commit-config.yaml",
    ".artifacts/*",
]


LIBS = set()


def git(*args):
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def changed_files(base, rev=None):
    """The files changed since the merge base of BASE and HEAD, with uncommitted and
    untracked ones, or with REV the files that differ between that merge base and REV."""
    merge_base = git("merge-base", base, rev or "HEAD").strip()
    # --no-renames reports both the old and the new path of a moved file
    if rev:
        files = set(git("diff", "--name-only", "--no-renames", merge_base, rev).split("\n"))
    else:
        files = set(git("diff", "--name-only", "--no-renames", merge_base).split("\n"))
        files |= set(git("ls-files", "--others", "--exclude-standard").split("\n"))
    files.discard("")
    return sorted(files)


def workspace():
    meta = json.loads(
        subprocess.run(
            [shutil.which("mbx") or "cargo", "metadata", "--format-version", "1", "--no-deps", "--offline"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    root = meta["workspace_root"]
    members = set(meta["workspace_members"])
    packages = {}
    for p in meta["packages"]:
        if p["id"] not in members:
            continue
        rel = os.path.relpath(os.path.dirname(p["manifest_path"]), root)
        deps = {d["name"] for d in p["dependencies"] if d.get("path")}
        deps |= cross_reads(os.path.join(root, rel))
        packages[p["name"]] = (rel, deps)
        if any("lib" in t["kind"] or "proc-macro" in t["kind"] for t in p["targets"]):
            LIBS.add(p["name"])
    return packages


def cross_reads(directory):
    """The crate directory names that literal paths in the Rust files under DIRECTORY name."""
    names = set()
    for base, dirs, files in os.walk(directory):
        dirs[:] = [d for d in dirs if d != "target"]
        for f in files:
            if f.endswith(".rs"):
                with open(os.path.join(base, f), errors="replace") as fh:
                    names |= set(CROSS_READ.findall(fh.read()))
    names.discard(os.path.basename(directory))
    return names


def matches(path, patterns):
    return any(fnmatch.fnmatchcase(path, pat) for pat in patterns)


def choose(base, files=None):
    """The packages that the change since BASE can affect, from the files that
    changed_files lists or from FILES when given, with the reason for each."""
    packages = workspace()
    names = set(packages)
    # a cross read names a crate directory, which is the package's name in this workspace
    by_dir = {os.path.basename(rel): name for name, (rel, _) in packages.items()}
    packages = {n: (rel, {by_dir.get(d, d) for d in deps}) for n, (rel, deps) in packages.items()}
    chosen = {}
    for f in changed_files(base) if files is None else files:
        if matches(f, ALL):
            return "all", f"{f} is a shared root"
        owner = None
        for name, (rel, _) in packages.items():
            if f.startswith(rel + "/") and (owner is None or len(rel) > len(packages[owner][0])):
                owner = name
        if owner:
            chosen.setdefault(owner, f)
            continue
        hit = False
        for pat, pkgs in READ_BY:
            if fnmatch.fnmatchcase(f, pat):
                hit = True
                for p in pkgs:
                    if p in names:
                        chosen.setdefault(p, f)
        if hit or matches(f, NO_RUST):
            continue
        if f.startswith("crates/"):
            # a crate directory outside the workspace, such as a binding's own workspace
            continue
        return "all", f"{f} is not covered by the rules"
    reasons = {p: f"{f} changed" for p, f in chosen.items()}
    # every package that depends on a chosen one, directly or not
    grew = True
    while grew:
        grew = False
        for name, (_, deps) in packages.items():
            if name in reasons:
                continue
            via = sorted(deps & set(reasons))
            if via:
                reasons[name] = f"depends on {via[0]}"
                grew = True
    if not reasons:
        return "none", "no workspace package reads a changed file"
    return sorted(reasons), reasons


def cargo_args(mode=None):
    v = os.environ.get("SPARKLES_CHANGED_PACKAGES")
    if v is None or v.strip() == "all":
        return "--workspace"
    names = v.split()
    if names and names != ["none"] and mode == "doc":
        workspace()
        names = [n for n in names if n in LIBS]
    if not names or names == ["none"]:
        return "none"
    if mode == "nextest":
        return "--workspace -E " + "|".join(f"package({n})" for n in names)
    return " ".join(f"-p {n}" for n in names)


def main():
    if sys.argv[1:2] == ["--cargo-args"] and sys.argv[2:] in ([], ["--doc"], ["--nextest"]):
        print(cargo_args(sys.argv[2][2:] if len(sys.argv) > 2 else None))
        return
    if len(sys.argv) != 2 or sys.argv[1].startswith("-"):
        sys.exit(__doc__)
    result, why = choose(sys.argv[1])
    if isinstance(result, str):
        print(f"changed-crates: {result}: {why}", file=sys.stderr)
        print(result)
        return
    for name in result:
        print(f"changed-crates: {name}: {why[name]}", file=sys.stderr)
        print(name)


if __name__ == "__main__":
    main()
