#!/usr/bin/env python3
"""Qualify and check runs of the host-timing test group (`mise run test:timing-cohort`).

The batch gate leaves the tests of the host-timing group of .config/nextest.toml out of
its pass/fail run, since they assert wall-clock bounds that other work on a host can
break. They run after the gate, alone, on one dedicated worker. A regression there opens
a fix-forward roadmap item rather than blocking the push, and this script decides what a
regression is.

Usage:
  timing-cohort.py record RUN --nextest-exit N --list LIST.json --worker ID
      Write RUN/run.json, the run's identity: a unique run ID, the source commit, the
      worker, the compiler and build configuration, nextest's exit status and the tests
      that the group selected, from `nextest list --message-format json`. The build
      configuration holds the Boxington version, every environment override of Cargo's
      and the compiler's flags and profiles, the manifest's profiles and a hash of each
      Cargo configuration file that applies.
  timing-cohort.py qualify RUN RUN RUN... --out THRESHOLDS
      From three or more distinct runs at one commit on one worker with one build
      configuration, write each test's duration threshold: the slowest run plus three
      times the spread between the fastest and the slowest, and at least 1.5 times the
      slowest run. Each run must have run to its end and report every selected test exactly
      once with a pass. Anything else makes the qualification fail.
  timing-cohort.py check RUN [--thresholds THRESHOLDS] [--commit REV]
      Report each test that failed, timed out, was skipped, is missing or listed twice, or
      ran longer than its threshold, and each test that the group lists without a
      threshold or that has a threshold and was not selected. Exits 1 when there is one
      and writes RUN/regression.md with a roadmap row to add, and 0 otherwise. A run from
      another worker or build configuration than the thresholds' exits 2.

A RUN is a directory with the run's junit.xml and run.json, as test:timing-cohort writes
them. Both run.json and the thresholds file carry a version, and a version this script
does not know is refused.
"""

import argparse
import json
import hashlib
import os
import re
import subprocess
import uuid
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


RUN_VERSION = 1
THRESHOLDS_VERSION = 1
# The parts of a run's identity that every run of one qualification shares, and that a
# checked run shares with its thresholds (the commit only within a qualification).
BUILD_KEYS = ("worker", "rustc", "build")


class Refused(Exception):
    """A run or thresholds file that cannot be compared."""


def results(run):
    """{"binary test": [(seconds, outcome), ...]} for the run's JUnit report, where outcome
    is pass, fail or skipped. A test listed twice has two entries."""
    root = ET.parse(Path(run) / "junit.xml").getroot()
    out = {}
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            if case.find("failure") is not None or case.find("error") is not None:
                outcome = "fail"
            elif case.find("skipped") is not None:
                outcome = "skipped"
            else:
                outcome = "pass"
            out.setdefault(f"{suite.get('name')} {case.get('name')}", []).append(
                (float(case.get("time") or 0), outcome))
    return out


def identity(run):
    """RUN/run.json, refused when it is missing or of an unknown version."""
    p = Path(run) / "run.json"
    try:
        ident = json.loads(p.read_text())
    except (OSError, ValueError) as e:
        raise Refused(f"{run} has no readable run.json ({e})")
    if ident.get("version") != RUN_VERSION:
        raise Refused(f"{p} is version {ident.get('version')!r}, and this script reads version {RUN_VERSION}")
    return ident


def selected(listing):
    """The tests that a `nextest list --message-format json` report selects, as
    "binary test" names like the JUnit report's."""
    names = set()
    for binary, suite in listing.get("rust-suites", {}).items():
        for name, case in suite.get("testcases", {}).items():
            if case.get("filter-match", {}).get("status") == "matches":
                names.add(f"{suite.get('binary-id', binary)} {name}")
    return sorted(names)


def coverage(run, ident):
    """Lines for each way RUN's report fails to cover its selected tests exactly once with
    a terminal outcome, and for a nextest run that did not end cleanly with no failing
    test to account for it."""
    got = results(run)
    want = set(ident["selected"])
    lines = []
    for n in sorted(want - set(got)):
        lines.append(f"MISSING {n} (selected, not in the report)")
    for n, rs in sorted(got.items()):
        if len(rs) > 1:
            lines.append(f"DUPLICATE {n} (listed {len(rs)} times)")
        elif rs[0][1] == "skipped":
            lines.append(f"SKIPPED {n}")
        if n not in want:
            lines.append(f"UNSELECTED {n} (in the report, not selected)")
    if not want:
        lines.append("EMPTY the run selected no test of the host-timing group")
    failed = any(o == "fail" for rs in got.values() for _, o in rs)
    if ident.get("nextest_exit") != 0 and not failed:
        lines.append(f"INCOMPLETE nextest exited {ident.get('nextest_exit')!r} with no failing test")
    return got, lines


def qualify(runs):
    """Thresholds and the accepted runs' identities from the qualification runs, or the
    reason they do not qualify."""
    if len(set(os.path.realpath(r) for r in runs)) != len(runs):
        return None, "a run directory is given twice"
    if len(runs) < 3:
        return None, "qualification needs at least three runs"
    try:
        idents = [identity(r) for r in runs]
    except Refused as e:
        return None, str(e)
    if len({i.get("run_id") for i in idents}) != len(runs):
        return None, "two runs share a run ID"
    for key in ("commit",) + BUILD_KEYS:
        values = {json.dumps(i.get(key), sort_keys=True) for i in idents}
        if len(values) != 1 or idents[0].get(key) in (None, ""):
            return None, f"the runs do not share one {key}"
    names = set(idents[0]["selected"])
    if any(set(i["selected"]) != names for i in idents):
        return None, "the runs do not select the same tests"
    per = []
    for r, i in zip(runs, idents):
        got, lines = coverage(r, i)
        if lines:
            return None, f"{r} does not cover its tests: " + "; ".join(lines)
        per.append(got)
    thresholds = {}
    for n in sorted(names):
        if any(p[n][0][1] != "pass" for p in per):
            return None, f"{n} failed in a qualification run"
        secs = [p[n][0][0] for p in per]
        lo, hi = min(secs), max(secs)
        thresholds[n] = {"min": lo, "max": hi, "threshold": round(max(hi + 3 * (hi - lo), 1.5 * hi), 3)}
    return {"tests": thresholds, "identities": idents}, None


def load_thresholds(path):
    t = json.loads(Path(path).read_text())
    if t.get("version") != THRESHOLDS_VERSION:
        raise Refused(f"{path} is version {t.get('version')!r}, and this script reads version {THRESHOLDS_VERSION}")
    return t


def check(run, thresholds):
    """The regressions of RUN against THRESHOLDS (a loaded thresholds file or None), as
    lines. A run from another worker or build configuration is refused."""
    ident = identity(run)
    if thresholds is not None:
        for key in BUILD_KEYS:
            if ident.get(key) != thresholds.get(key):
                raise Refused(f"the run's {key} {ident.get(key)!r} is not the thresholds' "
                              f"{thresholds.get(key)!r}; qualify this worker and build again")
    tests = thresholds["tests"] if thresholds is not None else None
    got, lines = coverage(run, ident)
    for n, rs in sorted(got.items()):
        if len(rs) != 1:
            continue
        secs, outcome = rs[0]
        t = tests.get(n) if tests is not None else None
        if outcome == "fail":
            lines.append(f"FAIL {n} ({secs:.2f} s)")
        elif outcome != "pass":
            continue
        elif tests is not None and t is None:
            lines.append(f"UNQUALIFIED {n} ({secs:.2f} s, no threshold)")
        elif t is not None and secs > t["threshold"]:
            lines.append(f"SLOW {n} ({secs:.2f} s, threshold {t['threshold']:.2f} s from {t['min']:.2f} to {t['max']:.2f} s)")
    if tests is not None:
        for n in sorted(set(tests) - set(ident["selected"])):
            lines.append(f"UNSELECTED-QUALIFIED {n} (has a threshold, not selected)")
    return lines


# Environment variables that change how Cargo and the compiler build the tests. Each one
# that is set goes into the build identity by name and value, and the listed names that
# are unset go in as unset, so a run with an override never compares with one without.
BUILD_ENV = re.compile(r"^(CARGO_ENCODED_RUSTFLAGS|CARGO_ENCODED_RUSTDOCFLAGS|RUSTFLAGS|RUSTDOCFLAGS"
                       r"|RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|RUSTC_BOOTSTRAP|CARGO_INCREMENTAL"
                       r"|CARGO_BUILD_[A-Z0-9_]+|CARGO_PROFILE_[A-Z0-9_]+|CARGO_TARGET_[A-Z0-9_]+"
                       r"|CARGO_UNSTABLE_[A-Z0-9_]+|MBX_LINKER|MBX_[A-Z_]*INCREMENTAL"
                       r"|CC|CXX|CFLAGS|CXXFLAGS|LDFLAGS)$")
LISTED_ENV = ("CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER",
              "CARGO_INCREMENTAL", "MBX_LINKER")
SECRET = re.compile(r"TOKEN|SECRET|PASSWORD|KEY", re.I)


def build_identity(root, env):
    """The build configuration of a run from ROOT under ENV: the Boxington version, the
    build environment overrides, the manifest's profiles and the Cargo configuration
    files that apply, as one stable string."""
    import tomllib
    lines = []
    try:
        lines.append(subprocess.run(["mbx", "--version"], capture_output=True, text=True, check=True).stdout.strip())
    except (OSError, subprocess.CalledProcessError):
        lines.append("mbx unknown")
    for name in sorted(set(LISTED_ENV) | {n for n in env if BUILD_ENV.match(n)}):
        if SECRET.search(name):
            continue
        lines.append(f"{name}={env[name]}" if name in env else f"{name} unset")
    root = Path(root)
    profiles = tomllib.loads((root / "Cargo.toml").read_text()).get("profile", {})
    lines.append("profiles " + json.dumps(profiles, sort_keys=True))
    home = Path(env.get("CARGO_HOME") or Path(env.get("HOME", "/")) / ".cargo")
    configs = [d / ".cargo" / n for d in [root, *root.parents] for n in ("config.toml", "config")]
    configs += [home / "config.toml", home / "config"]
    for c in configs:
        if c.is_file():
            lines.append(f"config {c} {hashlib.sha256(c.read_bytes()).hexdigest()}")
    return "\n".join(lines)


def record(run, nextest_exit, listing, worker):
    """Write RUN/run.json."""
    run = Path(run)
    ident = {
        "version": RUN_VERSION,
        "run_id": str(uuid.uuid4()),
        "commit": (run / "commit").read_text().strip(),
        "worker": worker,
        "rustc": (run / "rustc.txt").read_text().strip(),
        "build": build_identity(Path(__file__).resolve().parents[1], os.environ),
        "nextest_exit": nextest_exit,
        "selected": selected(json.loads(Path(listing).read_text())),
    }
    (run / "run.json").write_text(json.dumps(ident, indent=2) + "\n")
    return ident


def roadmap_row(lines, commit):
    tests = "; ".join(lines)
    return (
        f"| NNN | **A host-timing test regressed after the push of {commit}.** The dedicated timing "
        f"worker reported {tests}. Find the change in the batch that slowed it or broke its bound, "
        f"and fix it forward or make the test count its work in place of the wall clock. "
        f"| [DEVELOPMENT](DEVELOPMENT.md#timing-cohort) | S |"
    )


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("record")
    r.add_argument("run")
    r.add_argument("--nextest-exit", type=int, required=True)
    r.add_argument("--list", required=True)
    r.add_argument("--worker", required=True)
    q = sub.add_parser("qualify")
    q.add_argument("runs", nargs="+")
    q.add_argument("--out", required=True)
    c = sub.add_parser("check")
    c.add_argument("run")
    c.add_argument("--thresholds")
    c.add_argument("--commit", default="HEAD")
    a = ap.parse_args(argv)
    if a.cmd == "record":
        record(a.run, a.nextest_exit, a.list, a.worker)
        return 0
    if a.cmd == "qualify":
        q, why = qualify(a.runs)
        if q is None:
            print(f"timing-cohort: not qualified: {why}", file=sys.stderr)
            return 1
        first = q["identities"][0]
        out = {"version": THRESHOLDS_VERSION, "commit": first["commit"],
               **{k: first[k] for k in BUILD_KEYS},
               "runs": [{"dir": d, "run_id": i["run_id"]} for d, i in zip(a.runs, q["identities"])],
               "tests": q["tests"]}
        Path(a.out).write_text(json.dumps(out, indent=2) + "\n")
        for n, v in q["tests"].items():
            print(f"{v['threshold']:8.2f} s  {n} ({v['min']:.2f} to {v['max']:.2f} s)")
        return 0
    try:
        thresholds = load_thresholds(a.thresholds) if a.thresholds else None
        lines = check(a.run, thresholds)
    except Refused as e:
        print(f"timing-cohort: refused: {e}", file=sys.stderr)
        return 2
    if thresholds is None:
        print("timing-cohort: no thresholds given, so only failures and coverage count")
    if not lines:
        print("timing-cohort: no regression")
        return 0
    print("\n".join(lines))
    (Path(a.run) / "regression.md").write_text(
        "Add this row to the roadmap's fix-forward items with the next free number:\n\n"
        + roadmap_row(lines, a.commit) + "\n")
    print(f"timing-cohort: regression; roadmap row in {Path(a.run) / 'regression.md'}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
