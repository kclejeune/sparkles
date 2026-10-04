#!/usr/bin/env python3
"""Check that every `crates/sparkles…` path cited in the documentation exists.

Scans README.md, docs/*.md and docs/specs/*.md (docs/plans is private and skipped), the
crates' README files, and with --code also the comments of the Rust sources. A path ends
at the first character that cannot be part of one. A trailing `:123` line number and
trailing punctuation are dropped, `…` and globs (`*`) are matched with glob, and
`{a,b}` alternatives are expanded. Paths inside a longer path or a URL, such as
`../../crates/x` or `crates.io/api/v1/crates/x`, are not citations of this repository and
are skipped, and so are the paths in PLANNED.

Exits 1 and lists `file:line: path` for each path that does not exist.
"""

import glob
import itertools
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PATH = re.compile(r"(?<![A-Za-z0-9_./-])crates/sparkles[A-Za-z0-9_\-./*{},…]*")

# Files and crates that a spec describes before they are written.
PLANNED = {
    "crates/sparkles-core/src/branch.rs",  # F09
    "crates/sparkles-ffi",  # P04
    "crates/sparkles-fmt/benches",  # X02
    "crates/sparkles-node",  # P05
    "crates/sparkles-py/tests/test_parity.py",  # P06 Phase 1
    "crates/sparkles/bindings.toml",  # P06 Phase 1
    "crates/sparkles/tests/parity.rs",  # P06 Phase 1
}


def expand(path):
    """The alternatives of `{a,b}` groups."""
    parts = re.split(r"(\{[^{}]*\})", path)
    choices = [p[1:-1].split(",") if p.startswith("{") else [p] for p in parts]
    return ["".join(c) for c in itertools.product(*choices)]


def exists(path):
    if any(path == p or path.startswith(p + "/") for p in PLANNED):
        return True
    path = path.replace("…", "*")
    if "*" in path:
        return bool(glob.glob(os.path.join(ROOT, path), recursive=True))
    return os.path.exists(os.path.join(ROOT, path))


def clean(path):
    # a sentence or list ends with punctuation; `crates/x.rs:12` cites a line
    path = re.sub(r":[0-9][0-9,\-]*$", "", path)
    while path and path[-1] in ".,:;":
        path = path[:-1]
    # an unbalanced brace is the end of a code span such as `{crates/a, crates/b}`
    if path.count("{") != path.count("}"):
        path = path.split("{")[0].rstrip("/")
    return path


def doc_files(with_code):
    files = ["README.md"]
    files += sorted(glob.glob("docs/*.md", root_dir=ROOT))
    files += sorted(glob.glob("docs/specs/*.md", root_dir=ROOT))
    files += sorted(glob.glob("crates/*/README.md", root_dir=ROOT))
    files += sorted(glob.glob("crates/*/*/README.md", root_dir=ROOT))
    for name in ("CLAUDE.md", "AGENTS.md"):
        if os.path.exists(os.path.join(ROOT, name)):
            files.append(name)
    if with_code:
        files += sorted(glob.glob("crates/**/*.rs", root_dir=ROOT, recursive=True))
    return [f for f in files if "/target/" not in f]


def comment_text(line):
    """The comment part of a Rust line (`//` and `///` and `//!`), or ''."""
    i = line.find("//")
    return line[i:] if i >= 0 else ""


def main():
    with_code = "--code" in sys.argv[1:]
    missing = []
    for rel in doc_files(with_code):
        is_rust = rel.endswith(".rs")
        with open(os.path.join(ROOT, rel), encoding="utf-8") as f:
            for n, line in enumerate(f, 1):
                text = comment_text(line) if is_rust else line
                for m in PATH.finditer(text):
                    path = clean(m.group(0))
                    if path.endswith("/") and path.count("/") == 1:
                        continue
                    for p in expand(path):
                        if not exists(p):
                            missing.append(f"{rel}:{n}: {p}")
    for m in missing:
        print(m)
    if missing:
        print(f"{len(missing)} cited paths do not exist", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
