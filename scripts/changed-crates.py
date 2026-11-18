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
Cargo.toml, rust-toolchain.toml, .config/, testsuite/ and vendor/. A change to one of
them, or to any file the rules below do not cover, selects the whole workspace. Two
roots are read more closely. A change to Cargo.lock selects the workspace packages whose
locked dependencies, normal, build or dev, reach an entry that changed, and the whole
workspace when the lock cannot be compared entry by entry. A change to mise.toml selects
the whole workspace when it changes anything but task definitions, or the definition of
the lint or test task or of a task they reach, and otherwise no package. A change to
sparkles-core selects it and its dependents like any other crate. The batch gate makes the same selection for a
batch through scripts/gate/select-checks.py, and the nightly gate runs the whole workspace.

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
    "rust-toolchain.toml",
    "clippy.toml",
    "rustfmt.toml",
    ".mbx.toml",
    ".cargo/*",
    ".config/*",
    "testsuite/*",
    "vendor/*",
]

# The mise tasks whose definitions change what the Rust tasks of ci build or run: lint,
# test and ui:build, which scripts/ci.sh builds for both before they start.
RUST_TASKS = ["lint", "test", "ui:build"]

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


def reader(base, rev=None):
    """A function that gives the text of a path at the merge base of BASE and REV (or
    HEAD) and at REV, or in the working tree without REV, with None for a missing file."""
    merge_base = git("merge-base", base, rev or "HEAD").strip()

    def show(commit, path):
        r = subprocess.run(["git", "show", f"{commit}:{path}"], capture_output=True, text=True)
        return r.stdout if r.returncode == 0 else None

    def read(path):
        if rev:
            new = show(rev, path)
        else:
            try:
                with open(path) as f:
                    new = f.read()
            except OSError:
                new = None
        return show(merge_base, path), new

    return read


def _toml(text):
    import tomllib

    return tomllib.loads(text) if text is not None else {}


def lock_changes(old_text, new_text):
    """The workspace members of Cargo.lock whose locked dependencies reach an entry that
    differs between two versions of the lock, as a set of names, or the string "all" with
    the reason when the two cannot be compared entry by entry. A member is a package
    without a source. Cargo.lock lists the normal, build and dev-dependencies of every
    member, so a changed entry that a member reaches can change what it builds or tests."""
    try:
        old, new = _toml(old_text), _toml(new_text)
    except Exception as e:  # noqa: BLE001, any parse error widens the selection
        return "all", f"Cargo.lock does not parse ({e})"
    if not old or not new:
        return "all", "Cargo.lock was added or removed"
    if {k: v for k, v in old.items() if k != "package"} != {k: v for k, v in new.items() if k != "package"}:
        return "all", "Cargo.lock changes more than its package entries"

    def key(p):
        return (p.get("name"), p.get("version"), p.get("source"))

    def entries(lock):
        out = {}
        for p in lock.get("package", []):
            if key(p) in out:
                return None
            out[key(p)] = p
        return out

    o, n = entries(old), entries(new)
    if o is None or n is None:
        return "all", "Cargo.lock lists a package twice"
    changed = {k for k in set(o) | set(n) if o.get(k) != n.get(k)}
    if not changed:
        return set(), "Cargo.lock lists the same packages"
    members = set()
    for es in (o, n):
        by_name = {}
        for k in es:
            by_name.setdefault(k[0], []).append(k)

        def resolve(dep):
            parts = dep.split(" ")
            ks = by_name.get(parts[0], [])
            if len(parts) > 1:
                ks = [k for k in ks if k[1] == parts[1]]
            if len(parts) > 2:
                src = " ".join(parts[2:]).strip("()")
                ks = [k for k in ks if k[2] == src]
            return ks

        for k, p in es.items():
            if k[2] is not None:
                continue
            seen, stack = set(), [k]
            while stack:
                cur = stack.pop()
                if cur in seen:
                    continue
                seen.add(cur)
                for dep in es[cur].get("dependencies", []):
                    ks = resolve(dep)
                    if not ks:
                        return "all", f"Cargo.lock names a dependency it does not list ({dep})"
                    stack.extend(ks)
            if seen & changed:
                members.add(k[0])
    if not members:
        return "all", "no workspace member reaches the changed Cargo.lock entries"
    return members, "Cargo.lock changes their locked dependencies"


def task_text(task):
    """The words of a mise task's definition that can name another task."""
    if isinstance(task, str):
        return task
    return json.dumps(task, sort_keys=True)


def mise_changes(old_text, new_text):
    """The mise tasks whose definitions differ between two versions of mise.toml, as a
    pair of the changed task names and the tasks of the new file, or the string "all" and
    the reason when anything other than a task definition changed."""
    try:
        old, new = _toml(old_text), _toml(new_text)
    except Exception as e:  # noqa: BLE001
        return "all", f"mise.toml does not parse ({e})"
    if not old or not new:
        return "all", "mise.toml was added or removed"
    other = sorted(k for k in set(old) | set(new) if k != "tasks" and old.get(k) != new.get(k))
    if other:
        return "all", f"mise.toml changes [{other[0]}]"
    ot, nt = old.get("tasks", {}), new.get("tasks", {})
    changed = {t for t in set(ot) | set(nt) if ot.get(t) != nt.get(t)}
    return (changed, {**ot, **nt}), None


def reached(roots, tasks):
    """ROOTS and every task that their definitions name, directly or not. A task counts
    as named when its name appears in a root's definition between word boundaries, which
    covers depends, wait_for and `mise run` lines and errs towards more tasks."""
    names = sorted(tasks, key=len, reverse=True)
    pattern = re.compile(r"(?<![\w:-])(" + "|".join(re.escape(n) for n in names) + r")(?![\w:-])") if names else None
    out, stack = set(), [r for r in roots if r in tasks]
    while stack:
        t = stack.pop()
        if t in out:
            continue
        out.add(t)
        if pattern:
            stack.extend(m.group(1) for m in pattern.finditer(task_text(tasks[t])))
    return out | set(roots)


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


def choose(base, files=None, rev=None, read=None):
    """The packages that the change since BASE can affect, from the files that
    changed_files lists or from FILES when given, with the reason for each. READ gives
    the old and new text of a path (see reader), for Cargo.lock and mise.toml."""
    packages = workspace()
    names = set(packages)
    # a cross read names a crate directory, which is the package's name in this workspace
    by_dir = {os.path.basename(rel): name for name, (rel, _) in packages.items()}
    packages = {n: (rel, {by_dir.get(d, d) for d in deps}) for n, (rel, deps) in packages.items()}
    chosen = {}
    for f in changed_files(base) if files is None else files:
        if matches(f, ALL):
            return "all", f"{f} is a shared root"
        if f == "Cargo.lock":
            read = read or reader(base, rev)
            hit, why = lock_changes(*read(f))
            if hit == "all":
                return "all", why
            for p in sorted(hit):
                if p in names:
                    chosen.setdefault(p, f)
                else:
                    return "all", f"Cargo.lock names {p}, which is not a workspace package"
            continue
        if f == "mise.toml":
            read = read or reader(base, rev)
            got, why = mise_changes(*read(f))
            if got == "all":
                return "all", why
            changed, tasks = got
            hit = sorted(changed & reached(RUST_TASKS, tasks))
            if hit:
                return "all", f"mise.toml changes the {hit[0]} task, which the Rust tasks use"
            continue
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
