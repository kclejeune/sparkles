#!/usr/bin/env python3
"""Preserve actual normal-CI nextest output while ci.sh holds its target lease."""

import argparse
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
from pathlib import Path

VERSION = 1
REUSE_VERSION = 2
ORIGIN = re.compile(
    r"sparkles-gate-junit-v1 ([A-Za-z0-9_-][A-Za-z0-9._-]*) ([0-9a-f]{64})"
)


def atomic_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    temporary.replace(path)


def read_regular(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode):
            raise ValueError(f"not a regular file: {path}")
        data = source.read()
        after = os.fstat(source.fileno())
    identity = lambda s: (s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
    if (
        identity(before) != identity(after)
        or identity(after) != identity(path.lstat())
        or len(data) != before.st_size
    ):
        raise ValueError(f"file changed during read: {path}")
    return data, {
        "device": before.st_dev,
        "inode": before.st_ino,
        "size": len(data),
        "mtime_ns": before.st_mtime_ns,
        "ctime_ns": before.st_ctime_ns,
        "sha256": hashlib.sha256(data).hexdigest(),
    }


def inventory(root):
    target = root / "target"
    result = {
        "target_link": os.readlink(target) if target.is_symlink() else None,
        "target_kind": "unmanaged",
        "resolved_target": None,
        "reports": {},
    }
    if not target.exists():
        return result, {}
    resolved = target.resolve(strict=True)
    info = resolved.stat()
    if not stat.S_ISDIR(info.st_mode):
        raise ValueError("target is not a directory")
    result.update(
        resolved_target=str(resolved),
        target_device=info.st_dev,
        target_inode=info.st_ino,
    )
    if target.is_symlink():
        result["target_kind"] = "linked"
    nextest = resolved / "nextest"
    payload = {}
    if nextest.is_symlink():
        raise ValueError("nextest directory is a symlink")
    if not nextest.exists():
        return result, payload
    for profile in sorted(nextest.iterdir()):
        if profile.is_symlink():
            raise ValueError("nextest profile is a symlink")
        if not profile.is_dir():
            continue
        report = profile / "junit.xml"
        if not report.exists() and not report.is_symlink():
            continue
        if not re.fullmatch(r"[A-Za-z0-9_-]+", profile.name):
            raise ValueError("unsafe nextest profile name")
        data, identity = read_regular(report)
        identity["source_path"] = str(report)
        result["reports"][profile.name] = identity
        payload[profile.name] = data
    return result, payload


def load_manifest(out):
    data, _ = read_regular(out / "manifest.json")
    return parse_manifest(data)


def parse_manifest(data):
    value = json.loads(data)
    if not isinstance(value, dict):
        raise TypeError("JUnit retention manifest is not an object")
    if type(value.get("version")) is not int or value["version"] not in (
        VERSION,
        REUSE_VERSION,
    ):
        raise ValueError("unsupported JUnit retention version")
    return value


def init(root, out, args):
    out.mkdir()
    commit = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=root, text=True
    ).strip()
    dirty = subprocess.check_output(
        ["git", "status", "--porcelain"], cwd=root, text=True
    )
    atomic_json(
        out / "manifest.json",
        {
            "version": VERSION,
            "task": "test",
            "source_commit": commit,
            "source_dirty_status": dirty,
            "selection_base": args.base,
            "selection_packages": args.packages or "all",
            "selected": args.selected,
            "status": "not-run" if args.selected else "not-selected",
            "task_exit": None,
            "reports": [],
            "metadata": None,
        },
    )


def before(root, out):
    manifest = load_manifest(out)
    state, _ = inventory(root)
    atomic_json(out / "before.json", state)
    manifest["status"] = "running"
    atomic_json(out / "manifest.json", manifest)


def after(root, out, task_exit):
    manifest = load_manifest(out)
    manifest["task_exit"] = task_exit
    previous = json.loads((out / "before.json").read_text())
    current, payload = inventory(root)
    manifest["target"] = {
        key: value for key, value in current.items() if key != "reports"
    }
    if manifest["selection_packages"] == "none":
        manifest["status"] = "not-applicable"
        atomic_json(out / "manifest.json", manifest)
        return
    staging = out / ".staging"
    staging.mkdir()
    reports = []
    for profile, data in payload.items():
        relative = f"test/{profile}/junit.xml"
        target = staging / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        if read_regular(target)[0] != data:
            raise ValueError("retained copy differs")
        observed = current["reports"][profile]
        old = previous["reports"].get(profile)
        freshness = (
            "unchanged-generation-unproven"
            if old == observed
            else "new-or-changed-since-task-start"
        )
        reports.append(
            dict(observed, profile=profile, retained_path=relative, freshness=freshness)
        )
    resolved = current["resolved_target"]
    if resolved:
        metadata = Path(resolved + ".json")
        if metadata.exists() or metadata.is_symlink():
            data, identity = read_regular(metadata)
            (staging / "target-metadata.json").write_bytes(data)
            if read_regular(staging / "target-metadata.json")[0] != data:
                raise ValueError("retained metadata differs")
            manifest["metadata"] = dict(
                identity,
                source_path=str(metadata),
                retained_path="target-metadata.json",
            )
        else:
            manifest["metadata_status"] = "absent"
    else:
        manifest["metadata_status"] = "target-absent"
    for child in staging.iterdir():
        child.replace(out / child.name)
    staging.rmdir()
    manifest["reports"] = reports
    manifest["status"] = "retained" if reports else "missing"
    atomic_json(out / "manifest.json", manifest)


def cache_origin(root, out, run):
    if load_manifest(out)["selection_packages"] == "none":
        return
    # This is the last command of the test task, so the XML is still in its original
    # target and only a successful task can have its origin replayed by mise.
    after(root, out, 0)
    _, identity = read_regular(out / "manifest.json")
    line = f"sparkles-gate-junit-v1 {run} {identity['sha256']}"
    if not ORIGIN.fullmatch(line):
        raise ValueError("invalid gate run name")
    print(line)


def replay_origin(out):
    log, _ = read_regular(out.parent / "logs/test.log")
    origins = [
        match.groups()
        for line in log.decode(errors="replace").splitlines()
        if (match := ORIGIN.fullmatch(line))
    ]
    if len(origins) != 1:
        raise ValueError("test log must name one JUnit origin")
    run, digest = origins[0]
    return {"run": run, "manifest_sha256": digest}


def finish_test(root, out, task_exit, gate_run):
    manifest = load_manifest(out)
    if not gate_run or task_exit != 0 or manifest["selection_packages"] == "none":
        after(root, out, task_exit)
    elif manifest["status"] == "running":
        # A cache hit replays the log without running cache-origin. The current
        # manifest is still in the state written before mise started.
        manifest.update(
            version=REUSE_VERSION,
            status="reused",
            task_exit=0,
            reused_from=replay_origin(out),
        )
        atomic_json(out / "manifest.json", manifest)


def verify_reused(out, manifest, gate_runs):
    if (
        not manifest["selected"]
        or type(manifest["task_exit"]) is not int
        or manifest["task_exit"] != 0
        or manifest["selection_packages"] == "none"
        or manifest["reports"] != []
        or manifest["metadata"] is not None
    ):
        raise ValueError("inconsistent reused task")
    origin = replay_origin(out)
    if manifest["reused_from"] != origin:
        raise ValueError("JUnit origin differs from the test log")
    if gate_runs is None:
        raise ValueError("gate results directory is required for reused JUnit evidence")
    run = origin["run"]
    # A rerun of the same gate name archives the earlier output. The digest identifies
    # the original manifest even when several runs used that name.
    for directory in [gate_runs / run, *sorted(gate_runs.glob(f"{run}.old-[0-9]*"))]:
        receipts = directory / "ci-receipts"
        source = receipts / "junit"
        if any(path.is_symlink() for path in (directory, receipts, source)):
            continue
        try:
            data, identity = read_regular(source / "manifest.json")
        except FileNotFoundError:
            continue
        if identity["sha256"] != origin["manifest_sha256"]:
            continue
        original = parse_manifest(data)
        if (
            original["version"] != VERSION
            or original["status"] != "retained"
            or type(original["task_exit"]) is not int
            or original["task_exit"] != 0
            or original["selection_packages"] != manifest["selection_packages"]
        ):
            raise ValueError(f"gate {run} has no matching passing JUnit evidence")
        verify_manifest(source, original)
        if not any(
            report["freshness"] == "new-or-changed-since-task-start"
            for report in original["reports"]
        ):
            raise ValueError(f"gate {run} has no fresh JUnit report")
        return
    raise ValueError(f"JUnit origin manifest is missing or differs for gate {run}")


def verify(out, gate_runs=None):
    manifest = load_manifest(out)
    verify_manifest(out, manifest, gate_runs)


def verify_manifest(out, manifest, gate_runs=None):
    if manifest["task"] != "test" or not re.fullmatch(
        r"[0-9a-f]{40,64}", manifest["source_commit"]
    ):
        raise ValueError("invalid source/task provenance")
    if not isinstance(manifest["source_dirty_status"], str) or not isinstance(
        manifest["selected"], bool
    ):
        raise TypeError("invalid source/task context")
    if not all(
        isinstance(manifest[key], str)
        for key in ("selection_base", "selection_packages")
    ):
        raise ValueError("invalid package selection context")
    status = manifest["status"]
    if manifest["version"] == REUSE_VERSION:
        if status != "reused":
            raise ValueError("invalid status for reused JUnit manifest version")
        verify_reused(out, manifest, gate_runs)
        return
    if status == "not-selected" and (
        manifest["selected"] or manifest["task_exit"] is not None
    ):
        raise ValueError("inconsistent unselected task")
    if status == "not-run" and (
        not manifest["selected"] or manifest["task_exit"] is not None
    ):
        raise ValueError("inconsistent unstarted task")
    if status == "not-applicable" and (
        not manifest["selected"]
        or manifest["selection_packages"] != "none"
        or type(manifest["task_exit"]) is not int
    ):
        raise ValueError("inconsistent inapplicable task")
    if status in ("not-selected", "not-run", "not-applicable"):
        if manifest["reports"] or manifest["metadata"] is not None:
            raise ValueError("files in a non-executed state")
        return
    if status != "retained" or not manifest["reports"]:
        raise ValueError(f"JUnit evidence is {status}")
    if not manifest["selected"] or type(manifest["task_exit"]) is not int:
        raise ValueError("missing original task exit")

    def provenance(item):
        if not isinstance(item, dict):
            raise TypeError("file provenance is not an object")
        if (
            not isinstance(item["source_path"], str)
            or not Path(item["source_path"]).is_absolute()
        ):
            raise ValueError("invalid source file path")
        for key in ("device", "inode", "size", "mtime_ns", "ctime_ns"):
            if type(item[key]) is not int:
                raise TypeError(f"invalid file provenance type: {key}")
        if any(item[key] < 0 for key in ("device", "inode", "size")):
            raise ValueError("negative file identity or size")
        if not isinstance(item["sha256"], str) or not re.fullmatch(
            r"[0-9a-f]{64}", item["sha256"]
        ):
            raise ValueError("invalid file hash")

    resolved = manifest["target"]["resolved_target"]
    if not isinstance(resolved, str) or not Path(resolved).is_absolute():
        raise ValueError("invalid original target path")
    profiles = set()
    for report in manifest["reports"]:
        provenance(report)
        if report["freshness"] not in (
            "unchanged-generation-unproven",
            "new-or-changed-since-task-start",
        ):
            raise ValueError("invalid report freshness")
        profile = report["profile"]
        if not re.fullmatch(r"[A-Za-z0-9_-]+", profile) or profile in profiles:
            raise ValueError("invalid or duplicate profile")
        profiles.add(profile)
        if report["retained_path"] != f"test/{profile}/junit.xml":
            raise ValueError("wrong retained report path")
        if report["source_path"] != str(
            Path(resolved) / "nextest" / profile / "junit.xml"
        ):
            raise ValueError("wrong original report path")
    metadata = manifest["metadata"]
    if metadata is not None:
        provenance(metadata)
        if metadata["retained_path"] != "target-metadata.json":
            raise ValueError("wrong retained metadata path")
        if metadata["source_path"] != resolved + ".json":
            raise ValueError("wrong original metadata path")
    for item in [*manifest["reports"], *([metadata] if metadata is not None else [])]:
        relative = item["retained_path"]
        path = out / relative
        for parent in path.parents:
            if parent == out:
                break
            if parent.is_symlink():
                raise ValueError("retained parent is a symlink")
        data, identity = read_regular(path)
        if identity["sha256"] != item["sha256"] or len(data) != item["size"]:
            raise ValueError(f"retained bytes differ: {relative}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mode", choices=("init", "before", "after", "cache-origin", "verify")
    )
    parser.add_argument("--directory", type=Path, default=Path(".artifacts/ci/junit"))
    parser.add_argument("--selected", action="store_true")
    parser.add_argument("--base", default="")
    parser.add_argument("--packages", default="all")
    parser.add_argument("--task-exit", type=int)
    args = parser.parse_args()
    out = args.directory
    try:
        if args.mode == "init":
            init(Path.cwd(), out, args)
        elif args.mode == "before":
            before(Path.cwd(), out)
        elif args.mode == "after":
            if args.task_exit is None:
                raise ValueError("after requires original task exit")
            finish_test(
                Path.cwd(), out, args.task_exit, os.environ.get("SPARKLES_CI_GATE_RUN")
            )
        elif args.mode == "cache-origin":
            cache_origin(Path.cwd(), out, os.environ["SPARKLES_CI_GATE_RUN"])
        else:
            gate_runs = os.environ.get("SPARKLES_CI_GATE_RUNS")
            verify(out, Path(gate_runs) if gate_runs else None)
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        subprocess.SubprocessError,
    ) as error:
        print(f"JUnit retention {args.mode}: {error}", file=sys.stderr)
        if args.mode != "verify" and out.is_dir():
            try:
                manifest = load_manifest(out)
                manifest.update(status="error", error=str(error))
                if args.mode == "after":
                    manifest["task_exit"] = args.task_exit
                elif args.mode == "cache-origin":
                    manifest["task_exit"] = 0
                atomic_json(out / "manifest.json", manifest)
            except (OSError, ValueError, KeyError, TypeError):
                pass
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
