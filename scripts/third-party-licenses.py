#!/usr/bin/env python3
"""Write THIRD_PARTY_LICENSES.md: the license and NOTICE files of every crate the
`sparkles` binary links (the normal dependencies of sparkles-server with its default
features, on every platform the flake builds for), from `cargo metadata`.

Identical texts are printed once, with the crates that ship them. A crate whose package
has no license file gets the standard text of its license (of the first of MIT,
Apache-2.0 and BSD-3-Clause its SPDX expression offers), with its authors as the
copyright holders. The output depends only on Cargo.lock and the crates' sources, so it
is reproducible: regenerate it when Cargo.lock changes (`mise run licenses`; `--check`
fails when it is out of date).

usage: third-party-licenses.py [--check] [OUT]
"""
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# what ships: the server binary on the platforms of flake.nix, and the formatter's
# WebAssembly module the UI embeds
ROOTS = [
    (
        "sparkles-server",
        [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ],
    ),
    ("sparkles-fmt-wasm", ["wasm32-unknown-unknown"]),
]
# file names (case-insensitive) that carry a license or a notice
LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense)([-._].*)?$", re.I)

# standard texts for crates without a license file ({holders}: the crate's authors)
MIT = """MIT License

Copyright (c) {holders}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""

BSD3 = """Copyright (c) {holders}

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE."""


def metadata(target):
    out = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            target,
            "--manifest-path",
            os.path.join(ROOT, "Cargo.toml"),
        ],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    return json.loads(out)


def linked(meta, package):
    """The packages `package` links: normal dependencies, transitively."""
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    members = set(meta["workspace_members"])
    root = next(p["id"] for p in meta["packages"] if p["name"] == package and p["id"] in members)
    seen, todo = set(), [root]
    while todo:
        id = todo.pop()
        if id in seen:
            continue
        seen.add(id)
        for d in nodes[id]["deps"]:
            if any(k["kind"] is None for k in d["dep_kinds"]):
                todo.append(d["pkg"])
    return [pkgs[i] for i in seen if i not in members]


def license_files(p):
    """(name, text) of the license and notice files at the top of a package, and its
    `license-file`."""
    dir = os.path.dirname(p["manifest_path"])
    names = sorted(f for f in os.listdir(dir) if LICENSE_FILE.match(f) and os.path.isfile(os.path.join(dir, f)))
    if p.get("license_file"):
        lf = os.path.normpath(p["license_file"])
        rel = os.path.relpath(lf if os.path.isabs(lf) else os.path.join(dir, lf), dir)
        if rel not in names and os.path.isfile(os.path.join(dir, rel)):
            names.append(rel)
    out = []
    for n in names:
        with open(os.path.join(dir, n), encoding="utf-8", errors="replace") as f:
            text = f.read().replace("\r\n", "\n").strip("\n")
        if text.strip():
            out.append((n, text))
    return out


def key(text):
    """Texts that differ only in white space are the same text."""
    return hashlib.sha256(" ".join(text.split()).encode()).hexdigest()


def spdx(p):
    """The crate's SPDX expression, with the old `/` separator as `OR`."""
    return re.sub(r"\s*/\s*", " OR ", p.get("license") or "")


def apache_text(by_text):
    """The Apache License 2.0 as some crate ships it (its terms, without an appendix)."""
    for text, _, _ in sorted(by_text.values(), key=lambda t: (t[2][0], t[1])):
        if text.lstrip().startswith("Apache License") and "Version 2.0, January 2004" in text:
            end = text.find("END OF TERMS AND CONDITIONS")
            if end > 0:
                return text[: end + len("END OF TERMS AND CONDITIONS")]
    raise SystemExit("no crate ships the Apache License 2.0 text")


def standard_text(p, by_text):
    """(license, text) for a crate without a license file."""
    ids = re.findall(r"[A-Za-z0-9.+-]+", spdx(p))
    holders = ", ".join(re.sub(r"\s*<[^>]*>", "", a) for a in p.get("authors") or []) or (
        f"the {p['name']} authors"
    )
    for id in ("MIT", "Apache-2.0", "BSD-3-Clause"):
        if id in ids:
            if id == "MIT":
                return id, MIT.format(holders=holders)
            if id == "BSD-3-Clause":
                return id, BSD3.format(holders=holders)
            return id, apache_text(by_text)
    raise SystemExit(f"{p['name']} {p['version']}: no license file and no standard text for {spdx(p)!r}")


def fence(text):
    """A code fence longer than any backtick run in `text`."""
    longest = max((len(m) for m in re.findall(r"`+", text)), default=0)
    return "`" * max(3, longest + 1)


def render(pkgs):
    by_text = {}  # hash -> (text, file name, [crate])
    crates = []
    bare = []
    for p in sorted(pkgs, key=lambda p: (p["name"], p["version"])):
        files = license_files(p)
        crates.append((p, [n for n, _ in files]))
        if not files:
            bare.append(p)
        for n, text in files:
            h = key(text)
            by_text.setdefault(h, (text, n, []))[2].append(f"{p['name']} {p['version']}")
    # crates without a file: the standard text of their license
    for p in bare:
        id, text = standard_text(p, by_text)
        h = key(text)
        by_text.setdefault(h, (text, f"{id} (standard text)", []))[2].append(
            f"{p['name']} {p['version']}"
        )
    o = []
    w = o.append
    w("# Third-party licenses\n")
    w(
        "The `sparkles` binary links the crates below (the dependencies of `sparkles-server` "
        "on Linux and macOS, and of `sparkles-fmt-wasm`, the formatter's WebAssembly module "
        "in the web UI). Their licenses and notices follow, each text once, with the crates "
        "that ship it.\n"
    )
    w(
        "Generated by `scripts/third-party-licenses.py` from `Cargo.lock` "
        "(`mise run licenses`); do not edit.\n"
    )
    w("## Crates\n")
    w("| Crate | Version | License | Files |")
    w("|---|---|---|---|")
    for p, files in crates:
        lic = (spdx(p) or "see files").replace("|", "\\|")
        w(f"| {p['name']} | {p['version']} | {lic} | {', '.join(files) or 'standard text'} |")
    w("")
    # notices first: Apache-2.0 asks for them to travel with the binary
    texts = sorted(by_text.values(), key=lambda t: (not t[1].lower().startswith("notice"), t[2][0], t[1]))
    w("## Texts\n")
    for text, name, users in texts:
        w(f"### {name}: {', '.join(users)}\n")
        f = fence(text)
        w(f"{f}text\n{text}\n{f}\n")
    return "\n".join(o)


def main():
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    out = args[0] if args else os.path.join(ROOT, "THIRD_PARTY_LICENSES.md")
    pkgs = {}
    for package, targets in ROOTS:
        for t in targets:
            for p in linked(metadata(t), package):
                pkgs[p["id"]] = p
    text = render(pkgs.values())
    if check:
        try:
            with open(out, encoding="utf-8") as f:
                same = f.read() == text
        except FileNotFoundError:
            same = False
        if not same:
            print(f"{out} is out of date: run `mise run licenses`", file=sys.stderr)
            sys.exit(1)
        return
    with open(out, "w", encoding="utf-8") as f:
        f.write(text)
    print(f"{out}: {len(pkgs)} crates", file=sys.stderr)


if __name__ == "__main__":
    main()
