#!/usr/bin/env python3
"""Receipts and compatibility checks for the opt-in hosted cache comparison."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

SOURCE = "55531c16e81f492b72f86f5f575f1659391cf7ac"
MBX = "1.28.0"
RUST = "1.99.1"
NEXTEST = "0.9.146"
EXPECTED_BINARIES = 243
ENV_KEYS = (
    "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG", "CARGO_BUILD_JOBS",
    "CARGO_TARGET_DIR", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
    "MBX_TARGET_VIEWS", "MBX_CACHE_LINKS", "MBX_INCREMENTAL", "MBX_EAGER_INCREMENTAL",
    "MBX_SHARE_OUT_DIR", "MBX_BUILD_SCRIPT_EXECUTION", "MBX_REMOTE_URL",
    "MBX_REMOTE_NAMESPACE", "MBX_REMOTE_TRUST_WRITES", "MBX_REMOTE_MODE",
    "MBX_GC_MAX_SIZE", "MBX_GC_MIN_FREE_SIZE", "MBX_HTTP_TIMEOUT",
)
LAUNCHER = re.compile(
    r'\A#!/bin/sh\nMBX_BUILD_SCRIPT_SHIM_PATH="\$0" exec '
    r'"\$\(dirname "\$0"\)/(?P<relative>\.\./\.\./\.mbx-build-script-shims/'
    r'[0-9a-f]{64}/mbx)" "\$@"\n\Z'
)
ANSI = re.compile(r"\x1b\[[0-9;]*m")


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def output(*command):
    return subprocess.check_output(command, text=True).strip()


def digest(path):
    with Path(path).open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def results():
    path = Path(os.environ["CACHE_COMPARISON_RESULTS"])
    path.mkdir(parents=True, exist_ok=True)
    return path


def repair_launchers(target, binary):
    """Recreate helpers pruned by Swatinem; never evaluate cached shell text."""
    target = Path(target)
    if target.is_symlink():
        raise ValueError("Swatinem must own an ordinary target directory")
    if not target.exists():
        return {"launchers": 0, "helpers": 0}
    target = target.resolve()
    destinations = set()
    launchers = 0
    for real in target.rglob("*.mbx-real"):
        launcher = real.with_name(real.name.removesuffix(".mbx-real"))
        if launcher.is_symlink() or real.is_symlink():
            raise ValueError("Unexpected symlink at a cached build-script executable")
        # A supported launcher is tiny; do not read arbitrary executables into memory.
        with launcher.open("rb") as stream:
            data = stream.read(4097)
        if len(data) > 4096:
            raise ValueError(f"Unsupported cached build-script launcher: {launcher}")
        match = LAUNCHER.fullmatch(data.decode("utf-8"))
        if not match:
            raise ValueError(f"Unsupported cached build-script launcher: {launcher}")
        destination = (launcher.parent / match["relative"]).resolve()
        if not destination.is_relative_to(target):
            raise ValueError("Cached helper path escapes target")
        destinations.add(destination)
        launchers += 1
    # Validate every launcher before changing the target.
    for destination in destinations:
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(binary, destination)
        destination.chmod(0o755)
    return {"launchers": launchers, "helpers": len(destinations)}


def run_phase(name, command, *, json_stdout=False):
    root = results()
    resource_path = root / f"{name}.resource.json"
    resource_format = (
        '{"elapsed_seconds": %e, "user_seconds": %U, "system_seconds": %S, '
        '"max_rss_kib": %M, "filesystem_inputs": %I, "filesystem_outputs": %O}'
    )
    started = utc()
    before = time.monotonic()
    timed = ["/usr/bin/time", "-f", resource_format, "-o", str(resource_path), "--", *command]
    log_path = root / f"{name}.log"
    if json_stdout:
        with (root / f"{name}.json").open("w") as stdout, log_path.open("w") as stderr:
            completed = subprocess.run(timed, stdout=stdout, stderr=stderr, check=False)
        status = completed.returncode
        print(log_path.read_text(), end="", flush=True)
    else:
        with log_path.open("w") as log:
            with subprocess.Popen(timed, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True) as process:
                for line in process.stdout:
                    log.write(line)
                    print(line, end="", flush=True)
                status = process.wait()
    metrics = json.loads(resource_path.read_text().splitlines()[-1])
    write_json(root / f"{name}.phase.json", {
        "command": command, "started_at": started, "completed_at": utc(),
        "wall_seconds": time.monotonic() - before, "exit_code": status, **metrics,
    })
    if status:
        raise subprocess.CalledProcessError(status, command)


def assert_controls():
    source = output("git", "rev-parse", "HEAD")
    if source != SOURCE or output("git", "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Experiment source must be the clean, exact pinned commit")
    mbx = output("mbx", "--version")
    rustc = output("rustc", "-vV")
    nextest = output("cargo-nextest", "nextest", "--version")
    if not re.search(r"\b" + re.escape(MBX) + r"\b", mbx):
        raise ValueError("Wrong mbx version")
    if not rustc.startswith(f"rustc {RUST} ") or not nextest.startswith(f"cargo-nextest {NEXTEST} "):
        raise ValueError("Wrong compiler or nextest version")
    for key, expected in {
        "CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "0",
        "MBX_CACHE_LINKS": "1", "MBX_EAGER_INCREMENTAL": "0",
        "MBX_SHARE_OUT_DIR": "1", "MBX_BUILD_SCRIPT_EXECUTION": "1",
        "MBX_REMOTE_TRUST_WRITES": "false", "MBX_GC_MAX_SIZE": "30GiB",
        "MBX_GC_MIN_FREE_SIZE": "10GiB",
    }.items():
        if os.environ.get(key) != expected:
            raise ValueError(f"Wrong fixed control: {key}")
    for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_JOBS", "RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
        if os.environ.get(key):
            raise ValueError(f"Unexpected compiler override: {key}")
    arm = os.environ["CACHE_COMPARISON_ARM"]
    views = "1" if arm == "remote" else "0"
    if os.environ.get("MBX_TARGET_VIEWS") != views:
        raise ValueError("Wrong target ownership")
    remote = os.environ.get("MBX_REMOTE_URL", "")
    if remote != ("" if arm == "target-local" else "https://mbx.kclj.dev"):
        raise ValueError("Wrong remote transport")
    if arm != "target-local" and os.environ.get("MBX_REMOTE_NAMESPACE") != "kclejeune/sparkles":
        raise ValueError("Wrong remote namespace")
    doctor = json.loads(output("mbx", "doctor", "--json"))
    checks = {row["name"]: row for row in doctor["checks"] if row["name"] in ("config", "policy", "remote")}
    if any(row["severity"] == "fail" for row in checks.values()):
        raise ValueError("mbx doctor rejected a control")
    if checks.get("remote", {}).get("severity") != "pass":
        raise ValueError("Remote availability must be confirmed before measuring")
    if arm != "target-local" and checks.get("policy", {}).get("detail") != "configured read-write, effective read-only":
        raise ValueError("Experiment must preserve branch remote read-only policy")
    fixed = {
        "source": source, "tree": output("git", "rev-parse", "HEAD^{tree}"),
        "rustc": rustc, "mbx": mbx, "mbx_sha256": digest(Path(shutil.which("mbx")).resolve()),
        "nextest": nextest, "cargo_lock_sha256": digest("Cargo.lock"),
        "compiler_environment": {key: os.environ.get(key) for key in (
            "CARGO_INCREMENTAL", "CARGO_PROFILE_DEV_DEBUG", "CARGO_BUILD_JOBS", "RUSTFLAGS",
            "MBX_CACHE_LINKS", "MBX_EAGER_INCREMENTAL", "MBX_SHARE_OUT_DIR", "MBX_BUILD_SCRIPT_EXECUTION",
        )},
        "linker": output("cc", "--version"),
    }
    write_json(results() / "controls.json", {
        "fixed": fixed, "fixed_sha256": hashlib.sha256(json.dumps(fixed, sort_keys=True).encode()).hexdigest(),
        "arm": arm, "environment": {key: os.environ.get(key) for key in ENV_KEYS},
        "doctor": checks, "runner": {key: os.environ.get(key) for key in (
            "ImageOS", "ImageVersion", "RUNNER_ARCH", "RUNNER_OS", "GITHUB_RUN_ID", "GITHUB_RUN_ATTEMPT",
        )}, "cpu": output("lscpu"), "memory": Path("/proc/meminfo").read_text(),
    })


def snapshot(label):
    target = Path("target")
    arm = os.environ["CACHE_COMPARISON_ARM"]
    if arm != "remote" and target.is_symlink():
        raise ValueError("Swatinem target unexpectedly became a managed view")
    store = Path(output("mbx", "cache", "dir"))
    if target.exists() and (store.resolve().is_relative_to(target.resolve()) or target.resolve().is_relative_to(store.resolve())):
        raise ValueError("Cargo target and mbx object store overlap")
    sizes = {}
    for name, path in (("target", target), ("mbx_cache_dir", store)):
        sizes[name] = {"path": str(path.absolute()), "resolved": str(path.resolve()), "symlink": path.is_symlink()}
        if path.exists():
            for kind, flag in (("physical", ""), ("logical", "--apparent-size")):
                args = ["du", "-sL", "--block-size=1", *([flag] if flag else []), str(path)]
                sizes[name][kind + "_bytes"] = int(output(*args).split()[0])
    write_json(results() / f"{label}.sizes.json", {"paths": sizes, "disk": output("df", "-B1", ".")})


def archive():
    archive_dir = Path(os.environ["RUNNER_TEMP"]) / "archive"
    archive_dir.mkdir(exist_ok=True)
    raw = Path(os.environ["RUNNER_TEMP"]) / "raw.tar.zst"
    run_phase("archive", ["mbx", "nextest", "archive", "--locked", "--workspace", "--zstd-level", "-7", "--archive-file", str(raw)])
    run_phase("recompress", ["bash", "-euo", "pipefail", "-c", 'zstd -q -d -c "$1" | zstd -q -6 -T0 --long=27 -o "$2"', "recompress", str(raw), str(archive_dir / "tests.tar.zst")])
    raw.unlink()
    subprocess.run(["tar", "-cf", str(archive_dir / "ui-build.tar"), "ui/build"], check=True)
    log = ANSI.sub("", (results() / "archive.log").read_text())
    found = re.search(r"Archiving (\d+) binaries \(including (\d+) non-test binaries\)", log)
    if not found or int(found[1]) != EXPECTED_BINARIES:
        raise ValueError("Unexpected archive binary count")
    write_json(results() / "archive-counts.json", {"binaries": int(found[1]), "non_test_binaries": int(found[2]), "archive_bytes": (archive_dir / "tests.tar.zst").stat().st_size})


def listing_counts(data):
    suites = data["rust-suites"]
    count = sum(len(suite["testcases"]) for suite in suites.values())
    if not suites or not count or count != data["test-count"]:
        raise ValueError("Invalid nextest test listing")
    return {"test_binaries": len(suites), "tests": count, "test_ids_sha256": hashlib.sha256(json.dumps({key: sorted(suite["testcases"]) for key, suite in suites.items()}, sort_keys=True).encode()).hexdigest()}


def finish():
    root = results()
    counts = json.loads((root / "archive-counts.json").read_text())
    if os.environ["CACHE_COMPARISON_PHASE"] == "warm":
        listing = listing_counts(json.loads((root / "verify-list.json").read_text()))
        if listing["test_binaries"] + counts["non_test_binaries"] != counts["binaries"]:
            raise ValueError("Archive and listed binary counts differ")
        counts.update(listing)
    write_json(root / "summary.json", {
        "phase": os.environ["CACHE_COMPARISON_PHASE"],
        "controls": json.loads((root / "controls.json").read_text()), "counts": counts,
        "phases": {path.stem: json.loads(path.read_text()) for path in root.glob("*.phase.json")},
        "cache_summaries": {path.stem: [line for line in ANSI.sub("", path.read_text()).splitlines() if "mbx[cache]:" in line] for path in root.glob("*.log")},
    })


def compare_reports(directory):
    reports = [json.loads(path.read_text()) for path in Path(directory).rglob("summary.json")]
    reports = [report for report in reports if report.get("phase") == "warm"]
    arms = [report["controls"]["arm"] for report in reports]
    if len(arms) != 3 or set(arms) != {"remote", "target-local", "hybrid"}:
        raise ValueError("Exactly one completed warm report per arm is required")
    fixed = {report["controls"]["fixed_sha256"] for report in reports}
    counts = {json.dumps({key: report["counts"][key] for key in (
        "binaries", "non_test_binaries", "test_binaries", "tests", "test_ids_sha256",
    )}, sort_keys=True) for report in reports}
    if len(fixed) != 1 or len(counts) != 1:
        raise ValueError("Source/tool controls or archive/test identities differ")
    if any(phase["exit_code"] for report in reports for phase in report["phases"].values()):
        raise ValueError("A producer command failed")
    return {report["controls"]["arm"]: report for report in reports}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("controls", "repair", "archive", "doctests", "verify", "finish", "snapshot", "compare"))
    parser.add_argument("label", nargs="?")
    args = parser.parse_args()
    if args.operation == "controls":
        assert_controls()
    elif args.operation == "repair":
        started = time.monotonic()
        receipt = repair_launchers("target", Path(shutil.which("mbx")).resolve())
        write_json(results() / "repair.phase.json", {**receipt, "wall_seconds": time.monotonic() - started, "exit_code": 0})
    elif args.operation == "archive":
        archive()
    elif args.operation == "doctests":
        run_phase("doctests", ["mbx", "test", "--locked", "--workspace", "--doc"])
    elif args.operation == "verify":
        run_phase("verify-list", ["mbx", "nextest", "list", "--archive-file", str(Path(os.environ["RUNNER_TEMP"]) / "archive/tests.tar.zst"), "--workspace-remap", ".", "--message-format", "json"], json_stdout=True)
    elif args.operation == "snapshot":
        snapshot(args.label or "snapshot")
    elif args.operation == "compare":
        if not args.label:
            parser.error("compare requires the downloaded receipt directory")
        print(json.dumps(compare_reports(args.label), indent=2, sort_keys=True))
    else:
        finish()


if __name__ == "__main__":
    main()
