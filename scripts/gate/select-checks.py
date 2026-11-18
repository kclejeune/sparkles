#!/usr/bin/env python3
"""Choose the batch gate's checks from the files that a batch changes.

The batch gate runs this on the gate host in the gate's clean worktree, and
scripts/gate/gate.sh runs it on the laptop with --paths-only to decide whether a
conformance half is needed. It compares REV (default HEAD) with its merge base with BASE
and maps every changed file through RULES below to groups of checks. A file that no rule
covers runs everything, and so does a change to the build configuration or to the scripts
that run the checks. The workspace packages whose Clippy and Rust tests run come from
scripts/changed-crates.py, which adds every package that depends on a changed one.

The nightly gate runs every check with --force and does not use this selection.

Usage:
  select-checks.py --base BASE [--rev REV] [--paths-only]

It prints five lines, with the reasons on standard error:
  ci TASK...            the scripts/ci.sh tasks to run, or `all`
  conformance SUITE...  the conformance suites (w3c, shacl, shex) to run, possibly none
  spill MODE...         the spill matrices (plain, encrypted) to run, possibly none
  screen run|skip (R)   whether the batch query screen runs, and why
  packages P...         `all`, `none` or the packages for `scripts/ci.sh --changed`
--paths-only skips the package selection, which needs `cargo metadata`, and prints no
packages line and no Rust tasks. The conformance, spill and screen lines do not depend
on it.

Cargo.lock and mise.toml are read at both commits rather than taken as roots. A lock
change runs the Rust tasks for the packages whose locked dependencies it changes, and the
conformance suites when one of them is a package the suites check. A mise.toml change
runs the ci tasks and conformance suites whose definitions, or the definitions of the
tasks they name, it changes. Either runs everything when the two versions cannot be
compared that way, and a mise.toml change to anything but a task does too.
"""

import argparse
import fnmatch
import importlib.util
import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location("changed_crates", os.path.join(HERE, "..", "changed-crates.py"))
cc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(cc)

# The tasks of `scripts/ci.sh` without arguments, in its order. A test checks that this
# list matches the script.
CI_TASKS = [
    "fmt:check", "ui:fmt:check", "node:fmt:check", "lint:shell", "lint:doc-paths", "api:check",
    "test:bench-scripts", "lint", "ui:check", "fmt:wasm", "py:lint", "jvm:lint", "node:lint",
    "test", "test:quickstart", "ui:test", "client:types:check", "py:test", "jvm:rust-test",
    "jvm:test", "node:test", "node:client:check", "licenses:check",
]
CONFORMANCE = ["w3c", "shacl", "shex"]
SPILL = ["plain", "encrypted"]

# Groups of checks. `all` is every check above. `rust` is Clippy and the Rust tests, which
# run for the packages that changed-crates.py chooses rather than through a rule.
GROUPS = {
    # Cheap checks that run in every gate. Their task cache skips them when nothing they
    # read has changed.
    "base": ["fmt:check", "lint:shell", "lint:doc-paths", "api:check", "test:bench-scripts", "client:types:check"],
    "rust": ["lint", "test", "licenses:check"],
    "licenses": ["licenses:check"],
    "quickstart": ["test:quickstart"],
    "wasm": ["fmt:wasm"],
    "ui": ["ui:fmt:check", "ui:check", "ui:test", "licenses:check"],
    "py": ["py:lint", "py:test"],
    "jvm": ["jvm:lint", "jvm:rust-test", "jvm:test"],
    "node": ["node:fmt:check", "node:lint", "node:test", "node:client:check"],
    "conformance": [],
    "spill": [],
}

# The code that the W3C SPARQL, SHACL and ShEx suites exercise most directly: SPARQL
# parsing, algebra and evaluation, the RDF parsers, the XSD functions, GeoSPARQL, the
# SHACL, SHACLC and ShEx crates, and the suites' own harnesses, fixtures and known-failure
# lists. The Rust tests of the ci half run the same suites whenever sparkles-core changes,
# so this group adds their summaries and catches a change outside sparkles-core.
CONFORMANCE_PATHS = [
    "crates/sparkles-core/Cargo.toml",
    "crates/sparkles-core/src/sparql/*",
    "crates/sparkles-core/src/io.rs",
    "crates/sparkles-core/src/syntaxes.rs",
    "crates/sparkles-core/src/jena_formats.rs",
    "crates/sparkles-core/src/xsd.rs",
    "crates/sparkles-core/src/trix.rs",
    "crates/sparkles-core/src/trix/*",
    "crates/sparkles-core/src/geo/*",
    "crates/sparkles-core/tests/w3c.rs",
    "crates/sparkles-core/tests/w3c-known-failures.txt",
    "testsuite/geosparql/*",
    "testsuite/jena-arq/*",
    "crates/sparkles-shacl/Cargo.toml",
    "crates/sparkles-shacl/src/*",
    "crates/sparkles-shacl/tests/w3c_shacl.rs",
    "crates/sparkles-shacl/tests/known-failures.txt",
    "crates/sparkles-shacl/tests/shacl12/*",
    "crates/sparkles-shaclc/*",
    "crates/sparkles-shex/Cargo.toml",
    "crates/sparkles-shex/src/*",
    "crates/sparkles-shex/tests/shextest.rs",
    "crates/sparkles-shex/tests/known-failures.txt",
    "crates/sparkles-shex/tests/shex22/*",
]

# The streaming executor and the spill files that the plain and encrypted spill matrices
# put under a 1 MiB budget.
SPILL_PATHS = [
    "crates/sparkles-core/Cargo.toml",
    "crates/sparkles-core/src/spill.rs",
    "crates/sparkles-core/src/sparql/cursor.rs",
    "crates/sparkles-core/src/sparql/cursor/*",
    "crates/sparkles-core/src/sparql/ctx.rs",
    "crates/sparkles-core/src/sparql/mod.rs",
    "crates/sparkles-core/src/sparql/exec.rs",
    "crates/sparkles-core/src/sparql/query_memory.rs",
    "crates/sparkles-core/src/sparql/table.rs",
    "crates/sparkles-core/tests/w3c.rs",
    "crates/sparkles-core/tests/w3c-known-failures.txt",
]

# The surface the bindings wrap and the HTTP and CLI surface that api:check covers. A
# change here runs every binding's checks, and so does a change to Cargo.lock or the
# toolchain, which run everything.
BINDING_SURFACE = [
    "crates/sparkles/*",
    "docs/openapi.json",
    "docs/openapi-*.json",
    "docs/api-compatibility.toml",
    "docs/api-breaks.toml",
    "testsuite/api/*",
]

# (patterns, groups). A file takes the groups of every rule it matches, and a file that
# matches none runs everything. `*` matches across directories.
RULES = [
    # Build configuration and the scripts that run the checks.
    ([
        "Cargo.toml", "rust-toolchain.toml", ".mbx.toml", ".cargo/*", ".config/*",
        "clippy.toml", "rustfmt.toml", "vendor/*",
        "scripts/ci.sh", "scripts/gate/*", "scripts/cargo-sparkles-task", "scripts/mbx-policy-key.py",
        "scripts/changed-crates.py", "scripts/nextest-slow.py",
    ], ["all"]),
    # Files that only the cheap checks read: documentation, other scripts and their tests,
    # the GitHub workflows (which test:bench-scripts reads), deployment and Nix files.
    ([
        "*.md", "LICENSE", "docs/*", "scripts/*", ".github/*", ".githooks/*", "deploy/*", "nix/*",
        "flake.nix", "flake.lock", "Dockerfile", "compose.yaml", ".gitignore", ".dockerignore",
        ".pre-commit-config.yaml", ".shellcheckrc", ".gitattributes",
    ], ["base"]),
    # Workspace crates and the test suites: the Rust tasks come from changed-crates.py.
    (["crates/*", "testsuite/*"], ["base"]),
    (["THIRD_PARTY_LICENSES*.md", "scripts/third-party-licenses.py", "crates/*/Cargo.toml", "Cargo.lock"],
     ["licenses"]),
    # select() reads these two at both commits; see read_roots.
    (["Cargo.lock", "mise.toml"], ["base"]),
    (["README.md", "scripts/test-quickstart.py"], ["quickstart"]),
    (["scripts/build-fmt-wasm.sh"], ["wasm"]),
    (["ui/*"], ["base", "ui"]),
    # The Python tests also read Jena's patch fixtures from sparkles-core.
    (["crates/sparkles-py/*", "scripts/py-test.sh", "crates/sparkles-core/tests/patch/*"], ["base", "py"]),
    (["crates/sparkles-ffi/*", "jvm/*", "scripts/jvm-native.sh", "scripts/jvm-gradle.sh", "scripts/with-lock.py"],
     ["base", "jvm"]),
    (["crates/sparkles-node/*", "js/*", "scripts/node-native.sh"], ["base", "node"]),
    (BINDING_SURFACE, ["base", "py", "jvm", "node"]),
    (CONFORMANCE_PATHS, ["conformance"]),
    (SPILL_PATHS, ["spill"]),
]


# The mise tasks that the gate runs: the ci tasks, the conformance suites and the
# prerequisites that scripts/ci.sh builds before them.
CONFORMANCE_TASKS = {"test:w3c": "w3c", "test:shacl": "shacl", "test:shex": "shex"}
PREP_TASKS = ["ui:build", "ui:install", "node:install", "node:build", "jvm:native"]
# The packages whose code the conformance suites check.
CONFORMANCE_PACKAGES = {"sparkles-core", "sparkles-shacl", "sparkles-shaclc", "sparkles-shex"}

# The batch query screen (`bench:remote`, or `bench:nsc` as the fallback) times SPARQL
# queries against a release build of the server, so it runs for any change that can reach
# that build or the screen itself. It is skipped only when every changed file matches
# SCREEN_SKIP and none matches SCREEN_RUN. A file that neither list names runs it.
SCREEN_RUN = [
    # The build configuration of the release server.
    "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "mise.toml", ".cargo/*", ".mbx.toml", "vendor/*",
    # The scripts that build the revisions and run and summarise the screen. A test
    # checks that every script these name is listed here.
    "scripts/bench.sh", "scripts/bench-lib.sh", "scripts/bench-answers.py", "scripts/bench_hyperfine.py",
    "scripts/bench-summary.py", "scripts/bench-writes.py", "scripts/gen-data.py", "scripts/rss-probe.sh",
    "scripts/bench-remote.sh", "scripts/bench-gate.py", "scripts/bench-docs.py", "scripts/bench-executors.sh",
    "scripts/nsc-*", "scripts/remote-*", "scripts/build-remote.sh", "scripts/build-host.sh", "scripts/boat/*",
    "scripts/gate/config.sh", "scripts/mbx-policy-key.py", "scripts/cargo-sparkles-task", "scripts/latency.sh",
    # Files that the server's build reads from outside its crates.
    "docs/openapi.json", "docs/openapi-*.json", "docs/API.md", "ui/*",
]
SCREEN_SKIP = [
    "*.md", "LICENSE", "docs/*", ".github/*", ".githooks/*", "deploy/*", "nix/*", "flake.nix",
    "flake.lock", "Dockerfile", "compose.yaml", ".gitignore", ".dockerignore", ".pre-commit-config.yaml",
    ".shellcheckrc", ".gitattributes", "testsuite/*", "js/*", "jvm/*",
    # The CI, gate, binding, licence, documentation and release scripts and the script
    # tests. Other scripts run the screen, since many of them build or time the server.
    "scripts/gate/*", "scripts/bench-tests/*", "scripts/ci.sh", "scripts/ci-*", "scripts/premerge.sh",
    "scripts/changed-crates.py", "scripts/nextest-slow.py", "scripts/py-test.sh", "scripts/jvm-*.sh",
    "scripts/node-native.sh", "scripts/with-lock.py", "scripts/test-*", "scripts/third-party-licenses.py",
    "scripts/server-artifact*", "scripts/check-doc-*", "scripts/client-types.py", "scripts/api-compat*",
    "scripts/build-fmt-wasm.sh", "scripts/codex-review.sh", "scripts/install-mbx-container.sh",
    # Tests, benchmarks, examples and fuzz targets, which the server's release build leaves out.
    "crates/*/tests/*", "crates/*/benches/*", "crates/*/examples/*", "crates/*/fuzz/*",
    # Crates the screen never runs: OWL classification, SHACL, SHACLC and ShEx validation,
    # the HTTP client library, office and memory imports, the browser formatter and the
    # bindings, which are not part of the server.
    "crates/sparkles-owl/*", "crates/sparkles-shacl/*", "crates/sparkles-shaclc/*", "crates/sparkles-shex/*",
    "crates/sparkles-client/*", "crates/sparkles-office/*", "crates/sparkles-memory-import/*",
    "crates/sparkles-fmt-wasm/*", "crates/sparkles-py/*", "crates/sparkles-node/*", "crates/sparkles-ffi/*",
]


def screen(files):
    """Whether the batch query screen runs for FILES, and the reason."""
    if not files:
        return True, "no file changed"
    for f in files:
        if any(fnmatch.fnmatchcase(f, p) for p in SCREEN_RUN):
            return True, f"{f} changed"
        if not (any(fnmatch.fnmatchcase(f, p) for p in SCREEN_SKIP) or test_only(f)):
            return True, f"{f} can reach query execution"
    return False, "no changed file reaches query execution or the screen"


def read_roots(files, read):
    """The groups, ci tasks and conformance suites that changes to Cargo.lock and
    mise.toml call for, read at both commits through READ, with the reasons."""
    groups, tasks, conf, why = set(), set(), False, {}
    if "Cargo.lock" in files:
        members, reason = cc.lock_changes(*read("Cargo.lock"))
        if members == "all":
            groups.add("all")
            why["all"] = reason
        elif members & CONFORMANCE_PACKAGES:
            conf = True
            why["conformance"] = f"Cargo.lock changes the locked dependencies of {sorted(members & CONFORMANCE_PACKAGES)[0]}"
            groups.add("spill")
            why["spill"] = why["conformance"]
    if "mise.toml" in files:
        got, reason = cc.mise_changes(*read("mise.toml"))
        if got == "all":
            groups.add("all")
            why.setdefault("all", reason)
        else:
            changed, defs = got
            for root in CI_TASKS + list(CONFORMANCE_TASKS):
                hit = sorted(changed & cc.reached([root], defs))
                if not hit:
                    continue
                if root in CONFORMANCE_TASKS:
                    conf = True
                    why.setdefault("conformance", f"mise.toml changes the {hit[0]} task")
                else:
                    tasks.add(root)
                    why.setdefault(root, f"mise.toml changes the {hit[0]} task")
    if conf:
        groups.add("conformance")
    return groups, tasks, why


def test_only(path):
    """A unit-test file inside a crate's src directory, which changes no behaviour that
    the conformance suites see."""
    if "/src/" not in path:
        return False
    name = os.path.basename(path)
    return name == "tests.rs" or name.endswith("_tests.rs")


def groups_of(path):
    """The groups of checks that a change to PATH selects, or None when no rule covers it."""
    found = None
    for patterns, groups in RULES:
        if any(fnmatch.fnmatchcase(path, p) for p in patterns):
            if found is None:
                found = set()
            found |= set(groups)
    if found is not None and test_only(path):
        found -= {"conformance", "spill"}
    return found


def select(files, packages, read=None):
    """The selection for FILES, with PACKAGES the result of changed-crates.py (`all`,
    `none` or a list) or None to leave out the Rust tasks, and READ the old and new text
    of a path (changed-crates.py's reader). Returns a dict and the reasons."""
    why = {}
    chosen = set()
    extra = set()
    run_screen, why_screen = screen(files)
    if read is not None or {"Cargo.lock", "mise.toml"} & set(files):
        if read is None:
            chosen.add("all")
            why["all"] = "Cargo.lock or mise.toml changed and the selection cannot read them"
        else:
            g, extra, w = read_roots(files, read)
            chosen |= g
            for k, v in w.items():
                why.setdefault(k, v)
    if not files:
        chosen.add("all")
        why["all"] = "no file changed, so the gate checks everything"
    for f in files:
        g = groups_of(f)
        if g is None:
            g = {"all"}
            why.setdefault("all", f"{f} is not covered by the rules")
        for name in sorted(g):
            if name not in chosen:
                why.setdefault(name, f"{f} changed")
        chosen |= g
    if "all" in chosen:
        return {"ci": "all", "conformance": list(CONFORMANCE), "spill": list(SPILL), "packages": "all",
                "screen": (run_screen, why_screen)}, why
    tasks = set(extra)
    for name in chosen:
        tasks |= set(GROUPS[name])
    if packages is not None:
        if packages != "none":
            tasks |= set(GROUPS["rust"])
            why.setdefault("rust", f"packages {'all' if packages == 'all' else ' '.join(packages)}")
            if packages == "all" or "sparkles-server" in packages:
                tasks.add("test:quickstart")
            if packages == "all" or "sparkles-fmt-wasm" in packages:
                tasks.add("fmt:wasm")
    result = {
        "ci": [t for t in CI_TASKS if t in tasks],
        "conformance": list(CONFORMANCE) if "conformance" in chosen else [],
        "spill": list(SPILL) if "spill" in chosen else [],
        "screen": (run_screen, why_screen),
    }
    if packages is not None:
        result["packages"] = packages
    return result, why


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--base", required=True)
    ap.add_argument("--rev", default="HEAD")
    ap.add_argument("--paths-only", action="store_true")
    args = ap.parse_args()
    files = cc.changed_files(args.base, args.rev)
    read = cc.reader(args.base, args.rev)
    packages = None
    if not args.paths_only:
        result, reasons = cc.choose(args.base, files, args.rev, read)
        packages = result if isinstance(result, str) else list(result)
        if isinstance(result, str):
            print(f"select: packages {result}: {reasons}", file=sys.stderr)
    sel, why = select(files, packages, read)
    for name, reason in sorted(why.items()):
        print(f"select: {name}: {reason}", file=sys.stderr)
    print("ci " + (sel["ci"] if isinstance(sel["ci"], str) else " ".join(sel["ci"])))
    print(("conformance " + " ".join(sel["conformance"])).rstrip())
    print(("spill " + " ".join(sel["spill"])).rstrip())
    run_screen, why_screen = sel["screen"]
    print(f"screen {'run' if run_screen else 'skip'} ({why_screen})")
    if "packages" in sel:
        p = sel["packages"]
        print("packages " + (p if isinstance(p, str) else " ".join(p)))


if __name__ == "__main__":
    main()
