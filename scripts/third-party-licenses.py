#!/usr/bin/env python3
"""Write THIRD_PARTY_LICENSES.md: the license and NOTICE files of every crate the
`sparkles` binary links (the normal dependencies of sparkles-server with its default
features, on every platform the flake builds for), from `cargo metadata`. Without an OUT
argument it also writes crates/sparkles-py/THIRD_PARTY_LICENSES.md for the Python wheel,
from that crate's own workspace and lock.

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


# Notices about data a crate carries, beyond the crate's own license. The EPSG terms of
# use ask every distributor to pass them on to recipients and to acknowledge IOGP's
# ownership.
EPSG_TERMS = """EPSG Dataset Terms of Use

Revised 8 April 2016

In this document the following definitions of terms apply:

- "Registry" means the EPSG Geodetic Parameter Registry;
- "EPSG Dataset" means EPSG Geodetic Parameter Dataset;
- "IOGP" means the International Association of Oil and Gas Producers, incorporated in
  England as a company limited by guarantee (number 1832064);
- "EPSG Facilities" means the Registry, the EPSG Dataset (published through the Registry
  or through a downloadable MS-Access file or through a set of SQL scripts that enable a
  user to create an Oracle, MySQL, PostgreSQL or other database and populate that
  database with the EPSG Dataset) and associated documentation consisting of the Release
  Notes and Guidance Notes 7.1 and 7.2;
- "the data" means the geodetic parameter data and associated metadata, contained in the
  EPSG Dataset; it also refers to any subset of data from the EPSG Dataset.

The EPSG Facilities are published by IOGP at no charge. Distribution for profit is
forbidden.

The EPSG Facilities are owned by IOGP. They are compiled by the Geodetic Subcommittee of
the IOGP from publicly available and member-supplied information.

In order to use the EPSG Facilities, you must agree to these Terms of Use. You may not use
the EPSG Facilities or any of them in whole or in part unless you agree to these Terms of
Use.

You can accept these Terms of Use by clicking the command button 'Accept Terms' upon
registering as a new user. You will also be required to accept any revised Terms of Use
prior to using or downloading any EPSG Facilities. You understand and agree that any use
of the EPSG Facilities or any of them, even if obtained without clicking acceptance, will
be acceptance of these Terms of Use.

The data may be used, copied and distributed subject to the following conditions:

Whilst every effort has been made to ensure the accuracy of the information contained in
the EPSG Facilities, neither the IOGP nor any of its members past present or future
warrants their accuracy or will, regardless of its or their negligence, assume liability
for any foreseeable or unforeseeable use made thereof, which liability is hereby
excluded. Consequently, such use is at your own risk. You are obliged to inform anyone to
whom you provide the EPSG Facilities of these Terms of Use.

DATA AND INFORMATION PROVIDED IN THE EPSG FACILITIES ARE PROVIDED "AS IS" WITHOUT WARRANTY
OF ANY KIND, EITHER EXPRESSED OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND/OR FITNESS FOR A PARTICULAR PURPOSE.

The data may be included in any commercial package provided that any commerciality is
based on value added by the provider and not on a value ascribed to the EPSG Dataset
which is made available at no charge.

Ownership of the EPSG Dataset by IOGP must be acknowledged in any publication or
transmission (by whatever means) thereof (including permitted modifications).

Subsets of information may be extracted from the dataset. Users are advised that
coordinate reference system and coordinate transformation descriptions are incomplete
unless all elements detailed as essential in IOGP Surveying and Positioning Guidance
Note 7-1 Annex A are included.

Essential elements should preferably be reproduced as described in the dataset.
Modification of parameter values is permitted as described in the table below to allow
change to the content of the information provided that numeric equivalence is achieved.
Numeric equivalence refers to the results of geodetic calculations in which the
parameters are used, for example (i) conversion of ellipsoid defining parameters, or
(ii) conversion of parameters between one and two standard parallel projection methods,
or (iii) conversion of parameters between 7-parameter geocentric transformation methods.

No data that has been modified other than as permitted in these Terms of Use shall be
attributed to the EPSG Dataset.

Table 1: Permitted modifications of data (as given in the EPSG Dataset -> permitted
change for vendors/users to adopt)

Change of ellipsoid defining parameters.
1a  Ellipsoid parameters a and b -> a and 1/f; a and f; a and e; a and e2.
1b  Ellipsoid parameters a and 1/f -> a and b; a and f; a and e; a and e2.

Change of projection method.
2a  Lambert Conic Conformal (1 SP) method with projection parameters phiO and kO ->
    Lambert Conic Conformal (2 SP) method with projection parameters phi1 and phi2.
2b  Lambert Conic Conformal (2 SP) method with projection parameters phi1 and phi2 ->
    Lambert Conic Conformal (1 SP) method with projection parameters phiO and kO.
3a  Mercator (variant A) method with projection parameters phiO and kO ->
    Mercator (variant B) method with projection parameter phi1.
3b  Mercator (variant B) method with projection parameter phi1 ->
    Mercator (variant A) method with projection parameters phiO and kO.
4a  Hotine Oblique Mercator (variant A) method with projection parameters FE and FN ->
    Hotine Oblique Mercator (variant B) method with projection parameters EC and NC.
4b  Hotine Oblique Mercator (variant B) method with projection parameters EC and NC ->
    Hotine Oblique Mercator (variant A) method with projection parameters FE and FN.
5a  Polar Stereographic (Variant A) method with projection parameters phiO and kO ->
    Polar Stereographic (Variant B) method with projection parameter phiF.
5b  Polar Stereographic (Variant B) method with projection parameter phiF ->
    Polar Stereographic (Variant A) method with projection parameters phiO and kO.
5c  Polar Stereographic (Variant A) method with projection parameters phiO, kO, FE and
    FN -> Polar Stereographic (Variant C) method with projection parameters phiF, EF and
    NF.
5d  Polar Stereographic (Variant C) method with projection parameters phiF, EF and NF ->
    Polar Stereographic (Variant A) method with projection parameters phiO, kO, FE and
    FN.
5e  Polar Stereographic (Variant B) method with projection parameter FE and FN ->
    Polar Stereographic (Variant C) method with projection parameters EF and NF.
5f  Polar Stereographic (Variant C) method with projection parameters EF and NF ->
    Polar Stereographic (Variant B) method with projection parameter FE and FN.

Change of transformation method.
6a  Position Vector 7-parameter transformation method parameters RX RY and RZ ->
    Coordinate Frame transformation method with signs of position vector parameters RX
    RY and RZ reversed.
6b  Coordinate Frame transformation method parameters RX RY and RZ -> Position Vector
    7-parameter transformation method with signs of coordinate frame parameters RX RY and
    RZ reversed.
7   Concatenated transformation using geocentric methods (Geocentric translations,
    Position Vector 7-parameter transformation, Coordinate Frame rotation) -> Equivalent
    single geocentric transformation in which for each parameter the parameter values of
    the component steps have been summed.

Change of units.
8   NTv2 method grid file filename -> NTv2 method grid file relative storage path with
    file name including removal (if necessary) of "special characters" [spaces,
    parentheses, etc] which are replaced by underscore characters.
9   Parameter value -> Convert unit to another, for example from microradian to
    arc-second, using conversion factors obtained from the EPSG dataset Unit table.

Source: https://epsg.org/terms-of-use.html"""

DATA_NOTICES = {
    "crs-definitions": (
        "EPSG-derived coordinate reference system definitions",
        "The `crs-definitions` crate holds the proj4 and WKT definitions of the EPSG codes "
        "in the PostGIS `spatial_ref_sys` table, from which its authors generated it. They "
        "label the crate CC0-1.0. The definitions are derived from the EPSG Geodetic "
        "Parameter Dataset, which is owned by the International Association of Oil & Gas "
        "Producers (IOGP) and published at no charge under the EPSG terms of use below. "
        "Sparkles is free software and distributes the definitions at no charge.\n\n"
        "The proj4 definitions are a conversion of the EPSG data. Their parameters are "
        "rewritten in proj4's terms, and datum transformations are given as `+towgs84` "
        "Helmert parameters or `+nadgrids` grid names. They are not the EPSG Dataset and "
        "are not presented as EPSG data, and "
        "coordinates that Sparkles transforms with them are not attributed to EPSG.\n\n"
        "Anyone who passes this binary on must pass these terms on with it.\n",
        EPSG_TERMS,
    ),
}


def metadata(target, manifest=os.path.join(ROOT, "Cargo.toml"), locked=True):
    """`cargo metadata` for a platform. Without `locked`, cargo may add what the
    manifest needs to its lock file first."""
    out = subprocess.run(
        [
            "cargo",
            "metadata",
            "--format-version",
            "1",
            *(["--locked"] if locked else []),
            "--filter-platform",
            target,
            "--manifest-path",
            manifest,
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
    # the Sparkles crates themselves, which another workspace (the Python bindings') links
    # as path dependencies, are not third-party
    own = os.path.join(ROOT, "crates") + os.sep
    return [pkgs[i] for i in seen if i not in members and not pkgs[i]["manifest_path"].startswith(own)]


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


BINARY_INTRO = (
    "The `sparkles` binary links the crates below (the dependencies of `sparkles-server` "
    "on Linux and macOS, and of `sparkles-fmt-wasm`, the formatter's WebAssembly module "
    "in the web UI). Their licenses and notices follow, each text once, with the crates "
    "that ship it. The binary also includes EPSG-derived coordinate reference system "
    "definitions, whose terms of use are under Data notices.\n"
)

WHEEL_INTRO = (
    "The `sparkles` Python package (its extension module, built from `crates/sparkles-py`) "
    "links the crates below on Linux and macOS. Their licenses and notices follow, each "
    "text once, with the crates that ship it.\n"
)


def render(pkgs, intro=BINARY_INTRO, lock="Cargo.lock"):
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
    w(intro)
    w(
        f"Generated by `scripts/third-party-licenses.py` from `{lock}` "
        "(`mise run licenses`); do not edit.\n"
    )
    w("## Crates\n")
    w("| Crate | Version | License | Files |")
    w("|---|---|---|---|")
    for p, files in crates:
        lic = (spdx(p) or "see files").replace("|", "\\|")
        w(f"| {p['name']} | {p['version']} | {lic} | {', '.join(files) or 'standard text'} |")
    w("")
    # data the crates carry under terms of their own
    data = [(p, DATA_NOTICES[p["name"]]) for p, _ in crates if p["name"] in DATA_NOTICES]
    if data:
        w("## Data notices\n")
        for p, (title, intro, terms) in data:
            w(f"### {title}: {p['name']} {p['version']}\n")
            w(intro)
            f = fence(terms)
            w(f"{f}text\n{terms}\n{f}\n")
    # notices first: Apache-2.0 asks for them to travel with the binary
    texts = sorted(by_text.values(), key=lambda t: (not t[1].lower().startswith("notice"), t[2][0], t[1]))
    w("## Texts\n")
    for text, name, users in texts:
        w(f"### {name}: {', '.join(users)}\n")
        f = fence(text)
        w(f"{f}text\n{text}\n{f}\n")
    return "\n".join(o)


def write_or_check(out, text, n, check):
    """Write `out`, or with `check` fail when it differs from `text`."""
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
    print(f"{out}: {n} crates", file=sys.stderr)


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
    write_or_check(out, render(pkgs.values()), len(pkgs), check)
    # the Python wheel, from its own workspace and lock (the notices travel in the wheel)
    if not args:
        crate = os.path.join(ROOT, "crates", "sparkles-py")
        pkgs = {}
        for t in ROOTS[0][1]:
            # `mise run licenses` brings the lock up to date, as the py:* tasks do
            meta = metadata(t, os.path.join(crate, "Cargo.toml"), locked=check)
            for p in linked(meta, "sparkles-py"):
                pkgs[p["id"]] = p
        text = render(pkgs.values(), WHEEL_INTRO, "crates/sparkles-py/Cargo.lock")
        write_or_check(os.path.join(crate, "THIRD_PARTY_LICENSES.md"), text, len(pkgs), check)


if __name__ == "__main__":
    main()
