#!/usr/bin/env python3
"""Assemble and verify unpublished Linux GNU server candidates; never publish."""
import argparse
import copy
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zlib

ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-unknown-linux-gnu"
DOC = "share/doc/sparkles"
REQUIRED = {
    "bin/sparkles", "share/man/man1/sparkles.1",
    "share/man/man1/sparkles-serve.1",
    "share/bash-completion/completions/sparkles",
    "share/zsh/site-functions/_sparkles",
    "share/fish/vendor_completions.d/sparkles.fish",
    *[f"{DOC}/{name}" for name in (
        "openapi.json", "LICENSE", "THIRD_PARTY_LICENSES.md",
        "THIRD_PARTY_LICENSES-UI.md", "build-record.json",
    )],
}
IMAGES = json.loads((ROOT / "scripts/server-artifact-images.json").read_text())


def run(args, **kwargs):
    return subprocess.run([str(x) for x in args], check=True, capture_output=True,
                          timeout=90, **kwargs).stdout


def sha256(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def require(condition, message):
    if not condition:
        raise ValueError(message)


def first_json_difference(left, right, pointer=""):
    """Return the first differing JSON pointer without printing document values."""
    if isinstance(left, dict) and isinstance(right, dict):
        for key in sorted(left.keys() | right.keys()):
            path = pointer + "/" + key.replace("~", "~0").replace("/", "~1")
            if key not in left or key not in right:
                return path
            difference = first_json_difference(left[key], right[key], path)
            if difference is not None:
                return difference
        return None
    if isinstance(left, list) and isinstance(right, list):
        for index, (a, b) in enumerate(zip(left, right)):
            difference = first_json_difference(a, b, pointer + "/" + str(index))
            if difference is not None:
                return difference
        return pointer if len(left) != len(right) else None
    if isinstance(left, bool) != isinstance(right, bool) or left != right:
        return pointer or "/"
    return None


def check_openapi(generated, source, version):
    """Compare the full runtime contract with the checked-in version placeholder."""
    runtime = json.loads(generated)
    checked_in = json.loads(source)
    for document, expected, label in ((runtime, version, "runtime"),
                                      (checked_in, "0.0.0", "source")):
        info = document.get("info") if isinstance(document, dict) else None
        require(isinstance(info, dict) and isinstance(info.get("version"), str) and
                info["version"] == expected,
                f"generated OpenAPI differs from source at /info/version ({label} version)")
    comparison = copy.deepcopy(runtime)
    comparison["info"]["version"] = "0.0.0"
    difference = first_json_difference(comparison, checked_in)
    require(difference is None, f"generated OpenAPI differs from source at {difference}")


def parse_elf(header, program, dynamic, versions):
    """Reject newer glibc, non-system libraries and build-host loader paths."""
    require("ELF64" in header and "Advanced Micro Devices X86-64" in header,
            "candidate must be ELF64 x86_64")
    interpreter = re.findall(r"Requesting program interpreter: ([^\]]+)", program)
    require(interpreter == ["/lib64/ld-linux-x86-64.so.2"], "unexpected GNU interpreter")
    require(not re.search(r"\((?:RPATH|RUNPATH)\)", dynamic), "runtime search path is forbidden")
    needed = re.findall(r"\(NEEDED\).*?\[([^\]]+)\]", dynamic)
    allowed = {"libgcc_s.so.1", "libm.so.6", "libc.so.6", "libpthread.so.0",
               "libdl.so.2", "librt.so.1", "ld-linux-x86-64.so.2"}
    require(needed and set(needed) <= allowed, f"non-baseline runtime dependencies: {needed}")
    glibc = sorted(set(re.findall(r"\bGLIBC_([A-Za-z0-9_.]+)", versions)))
    require(glibc, "missing GLIBC requirements")
    require(all(re.fullmatch(r"[0-9]+(?:\.[0-9]+)+", v) and
                tuple(map(int, v.split("."))) <= (2, 28) for v in glibc),
            f"requires GLIBC outside 2.28 baseline: {glibc}")
    require("/nix/store/" not in header + program + dynamic + versions, "Nix dependency")
    return {"interpreter": interpreter[0], "needed": sorted(needed), "glibc": glibc}


def audit_elf(binary):
    arguments = ("-h", "-l", "-d", "--version-info")
    reports = [run(["readelf", "--wide", flag, binary], text=True) for flag in arguments]
    return parse_elf(*reports), dict(zip(("header", "program", "dynamic", "versions"), reports))


def ui_inputs(source):
    ui = source / "ui/build"
    index = (ui / "index.html").read_text()
    require("The UI has not been built" not in index, "placeholder UI is forbidden")
    require((ui / "_app/version.json").is_file(), "missing production UI marker")
    scripts = sorted(ui.rglob("*.js"))
    wasm = sorted(ui.rglob("*.wasm"))
    require(scripts and wasm, "production JavaScript and formatter Wasm are required")
    require((ui / "licenses.txt").read_bytes() ==
            (source / "THIRD_PARTY_LICENSES-UI.md").read_bytes(), "stale UI notices")
    files = sorted(p for p in ui.rglob("*") if p.is_file())
    require(all(not p.is_symlink() and p.stat().st_size for p in files),
            "empty or linked UI asset")
    hashes = {p.relative_to(ui).as_posix(): sha256(p) for p in files}
    return {"files": hashes,
            "script": scripts[0].relative_to(ui).as_posix(),
            "wasm": wasm[0].relative_to(ui).as_posix()}


def check_debuglink(section, debug):
    end = section.find(b"\0")
    require(end > 0, "invalid GNU debug link")
    name = section[:end].decode("utf-8")
    require(name == Path(debug).name, "GNU debug link names another file")
    offset = (end + 4) & ~3
    require(len(section) == offset + 4, "invalid GNU debug link CRC size")
    expected = struct.unpack("<I", section[offset:])[0]
    require(zlib.crc32(Path(debug).read_bytes()) == expected, "GNU debug link CRC mismatch")


def archive(tree, destination, epoch):
    with destination.open("wb") as stream, gzip.GzipFile(
        fileobj=stream, mode="wb", filename="", mtime=epoch
    ) as compressed, tarfile.open(fileobj=compressed, mode="w") as output:
        for path in [tree, *sorted(tree.rglob("*"))]:
            require(not path.is_symlink(), "archive input contains a link")
            info = output.gettarinfo(str(path), path.relative_to(tree.parent).as_posix())
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = epoch
            info.mode = 0o755 if path.is_dir() or path == tree / "bin/sparkles" else 0o644
            if path.is_file():
                with path.open("rb") as content:
                    output.addfile(info, content)
            else:
                output.addfile(info)


def checksum_entries(path):
    entries = {}
    for line in Path(path).read_text().splitlines():
        match = re.fullmatch(r"([a-f0-9]{64})  ([A-Za-z0-9][A-Za-z0-9._-]*)", line)
        require(match is not None, "malformed checksum line")
        digest, name = match.groups()
        require(name not in entries, "duplicate checksum name")
        entries[name] = digest
    require(entries, "empty checksums")
    return entries


def verify(archive_path, checksums, destination, version, target=TARGET):
    require(target == TARGET, "unsupported candidate target")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9.-]+)?", version),
            "invalid package version")
    expected_root = f"sparkles-{version}-{target}"
    require(Path(archive_path).name == expected_root + ".tar.gz", "unexpected archive name")
    entries = checksum_entries(checksums)
    require(entries.get(Path(archive_path).name) == sha256(archive_path), "archive checksum mismatch")
    destination = Path(destination)
    require(not destination.exists(), "extraction destination already exists")
    with tarfile.open(archive_path, "r:gz") as source:
        members = []
        names = set()
        total = 0
        for member in source:
            require(len(members) < 10000, "archive exceeds candidate member limit")
            members.append(member)
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and ".." not in path.parts and
                    path.parts and path.parts[0] == expected_root and
                    member.name == path.as_posix(), "unsafe archive path")
            require(member.name not in names, "duplicate archive member")
            require(member.isfile() or member.isdir(), "archive links or special files are forbidden")
            require(not member.mode & 0o7000, "unsafe archive permissions")
            names.add(member.name)
            total += member.size
            require(total <= 4 * 1024**3, "archive exceeds candidate size limit")
        require({f"{expected_root}/{name}" for name in REQUIRED} <= names, "missing required archive files")
        files = {member.name: member for member in members if member.isfile()}
        require(all(f"{expected_root}/{name}" in files and
                    files[f"{expected_root}/{name}"].size > 0 for name in REQUIRED),
                "required member must be a nonempty file")
        require(files[f"{expected_root}/bin/sparkles"].mode & 0o111,
                "installed binary is not executable")
        # No extractall: parents are constructed only after the complete manifest passes.
        destination.mkdir(parents=True)
        try:
            for member in members:
                output = destination / member.name
                if member.isdir():
                    output.mkdir(parents=True, exist_ok=True)
                else:
                    output.parent.mkdir(parents=True, exist_ok=True)
                    with source.extractfile(member) as content, output.open("xb") as stream:
                        shutil.copyfileobj(content, stream)
                    output.chmod(member.mode & 0o777)
            root = destination / expected_root
            record = json.loads((root / DOC / "build-record.json").read_text())
            require(isinstance(record, dict) and record.get("schema_version") == 1,
                    "unknown build record schema")
            require(record.get("version") == version and record.get("target") == target,
                    "build record identity mismatch")
            require(record.get("candidate") is True and record.get("attested") is False,
                    "unsupported provenance claim")
            require(record.get("binary_sha256") == sha256(root / "bin/sparkles"),
                    "build record binary hash mismatch")
            document = json.loads((root / DOC / "openapi.json").read_text())
            require(isinstance(document, dict) and document.get("openapi") == "3.1.0",
                    "invalid OpenAPI document")
            return root, record
        except BaseException:
            shutil.rmtree(destination)
            raise


def pack(binary, source, destination, builder):
    source, binary, destination = Path(source).resolve(), Path(binary).resolve(), Path(destination).resolve()
    require(builder == IMAGES["builder"], "unrecognized builder image")
    require(not destination.exists(), "output directory already exists")
    require(not destination.is_relative_to((source / "target").resolve()),
            "artifact output must be outside the managed compiler target")
    require(run(["git", "-C", source, "status", "--porcelain", "--untracked-files=normal"],
                text=True).strip() == "", "candidate source must be committed and clean")
    version = tomllib.loads((source / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    commit = run(["git", "-C", source, "rev-parse", "HEAD"], text=True).strip()
    epoch = int(run(["git", "-C", source, "show", "-s", "--format=%ct", "HEAD"], text=True))
    require(run([binary, "--version"], text=True).strip() == f"sparkles {version}", "binary version mismatch")
    ui = ui_inputs(source)
    elf, reports = audit_elf(binary)
    sections = run(["readelf", "--wide", "--section-headers", binary], text=True)
    require(re.search(r"\]\s+\.debug_line\s", sections), "release line-table debug information is required")
    record = {"schema_version": 1, "candidate": True, "attested": False, "version": version, "target": TARGET,
              "commit": commit, "profile_requested": "release", "default_features_requested": True,
              "effective_features_reported": False, "builder": builder, "elf": elf,
              "cargo_lock_sha256": sha256(source / "Cargo.lock"), "ui": ui,
              "tools": {tool: run([tool, "--version"], text=True).splitlines()[0]
                        for tool in ("rustc", "mbx", "node", "pnpm", "wasm-bindgen", "objcopy")}}
    require(record["tools"]["mbx"].split()[-1] == "1.28.0", "unexpected Boxington version")
    destination.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".server-artifact-", dir=destination.parent) as work:
        work = Path(work)
        out = work / "out"
        out.mkdir()
        name = f"sparkles-{version}-{TARGET}"
        tree = work / name
        (tree / "bin").mkdir(parents=True)
        installed = tree / "bin/sparkles"
        shutil.copy2(binary, installed)
        doc = tree / DOC
        doc.mkdir(parents=True)
        for notice in ("LICENSE", "THIRD_PARTY_LICENSES.md", "THIRD_PARTY_LICENSES-UI.md"):
            require((source / notice).stat().st_size > 0, f"empty notice: {notice}")
            shutil.copyfile(source / notice, doc / notice)
        generated = run([installed, "openapi"])
        check_openapi(generated, (source / "docs/openapi.json").read_bytes(), version)
        (doc / "openapi.json").write_bytes(generated)
        run([installed, "man", "--dir", tree / "share/man/man1"])
        for shell, relative in (
            ("bash", "share/bash-completion/completions/sparkles"),
            ("zsh", "share/zsh/site-functions/_sparkles"),
            ("fish", "share/fish/vendor_completions.d/sparkles.fish"),
        ):
            path = tree / relative
            path.parent.mkdir(parents=True)
            path.write_bytes(run([installed, "completions", shell]))
        record["documents"] = {p.relative_to(tree).as_posix(): sha256(p)
                               for p in sorted(tree.rglob("*")) if p.is_file() and p != installed}
        debug_tree = work / (name + ".debug")
        debug_tree.mkdir()
        debug = debug_tree / "sparkles.debug"
        run(["objcopy", "--only-keep-debug", installed, debug])
        run(["objcopy", "--strip-debug", installed])
        run(["objcopy", f"--add-gnu-debuglink={debug}", installed])
        stripped = run(["readelf", "--wide", "--section-headers", installed], text=True)
        require(re.search(r"\]\s+\.symtab\s", stripped), "stripping removed the symbol table")
        require(not re.search(r"\]\s+\.debug_", stripped), "stripping retained debug information")
        section = work / "debuglink"
        run(["objcopy", f"--dump-section=.gnu_debuglink={section}", installed])
        check_debuglink(section.read_bytes(), debug)
        record["binary_sha256"] = sha256(installed)
        record["debug_sha256"] = sha256(debug)
        (doc / "build-record.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        archive(tree, out / (name + ".tar.gz"), epoch)
        archive(debug_tree, out / (name + ".debug.tar.gz"), epoch)
        (out / "elf-audit.json").write_text(json.dumps(reports, indent=2, sort_keys=True) + "\n")
        (out / "candidate.json").write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
        (out / "SHA256SUMS").write_text("".join(f"{sha256(p)}  {p.name}\n" for p in sorted(out.iterdir())))
        verify(out / (name + ".tar.gz"), out / "SHA256SUMS", work / "verified", version)
        out.rename(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    assembly = commands.add_parser("pack")
    assembly.add_argument("--binary", type=Path, required=True)
    assembly.add_argument("--source", type=Path, default=ROOT)
    assembly.add_argument("--out", type=Path, required=True)
    assembly.add_argument("--builder", required=True)
    checking = commands.add_parser("verify")
    checking.add_argument("--archive", type=Path, required=True)
    checking.add_argument("--checksums", type=Path, required=True)
    checking.add_argument("--out", type=Path, required=True)
    checking.add_argument("--version", required=True)
    args = parser.parse_args()
    if args.command == "pack":
        pack(args.binary, args.source, args.out, args.builder)
    else:
        verify(args.archive, args.checksums, args.out, args.version)


if __name__ == "__main__":
    try:
        main()
    except (ValueError, OSError, subprocess.SubprocessError, tarfile.TarError) as error:
        raise SystemExit(f"server-artifact: {error}")
