#!/usr/bin/env python3
"""Qualify and check runs of the host-timing test group (`mise run test:timing-cohort`).

The batch gate leaves the tests of the host-timing group of .config/nextest.toml out of
its pass/fail run, since they assert wall-clock bounds that other work on a host can
break. They run after the gate, alone, on one dedicated worker. A regression there opens
a fix-forward roadmap item rather than blocking the push, and this script decides what a
regression is.

Usage:
  timing-cohort.py qualify RUN RUN RUN... --out THRESHOLDS
      From three or more runs on the idle worker at origin/main, write each test's
      duration threshold: the slowest run plus three times the spread between the fastest
      and the slowest, and at least 1.5 times the slowest run. A test that failed in any
      qualification run makes the qualification fail.
  timing-cohort.py check RUN [--thresholds THRESHOLDS] [--commit REV]
      Report each test that failed, timed out or ran longer than its threshold, and each
      test that the group lists without a threshold. Exits 1 when there is one and writes
      RUN/regression.md with a roadmap row to add, and 0 otherwise.

A RUN is a directory with the run's junit.xml, as test:timing-cohort writes it.
"""

import argparse
import json
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


def results(run):
    """{"binary test": (seconds, outcome)} for the run's JUnit report, where outcome is
    pass or fail."""
    root = ET.parse(Path(run) / "junit.xml").getroot()
    out = {}
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            failed = case.find("failure") is not None or case.find("error") is not None
            out[f"{suite.get('name')} {case.get('name')}"] = (float(case.get("time") or 0), "fail" if failed else "pass")
    return out


def qualify(runs):
    """Thresholds from the qualification runs, or the reason they do not qualify."""
    if len(runs) < 3:
        return None, "qualification needs at least three runs"
    per = [results(r) for r in runs]
    names = set(per[0])
    if not names or any(set(p) != names for p in per):
        return None, "the runs do not list the same tests"
    thresholds = {}
    for n in sorted(names):
        if any(p[n][1] != "pass" for p in per):
            return None, f"{n} failed in a qualification run"
        secs = [p[n][0] for p in per]
        lo, hi = min(secs), max(secs)
        thresholds[n] = {"min": lo, "max": hi, "threshold": round(max(hi + 3 * (hi - lo), 1.5 * hi), 3)}
    return thresholds, None


def check(run, thresholds):
    """The regressions of RUN against THRESHOLDS, as lines."""
    got = results(run)
    lines = []
    for n, (secs, outcome) in sorted(got.items()):
        t = thresholds.get(n) if thresholds is not None else None
        if outcome != "pass":
            lines.append(f"FAIL {n} ({secs:.2f} s)")
        elif thresholds is not None and t is None:
            lines.append(f"UNQUALIFIED {n} ({secs:.2f} s, no threshold)")
        elif t is not None and secs > t["threshold"]:
            lines.append(f"SLOW {n} ({secs:.2f} s, threshold {t['threshold']:.2f} s from {t['min']:.2f} to {t['max']:.2f} s)")
    if not got:
        lines.append("EMPTY the run lists no test of the host-timing group")
    return lines


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
    q = sub.add_parser("qualify")
    q.add_argument("runs", nargs="+")
    q.add_argument("--out", required=True)
    c = sub.add_parser("check")
    c.add_argument("run")
    c.add_argument("--thresholds")
    c.add_argument("--commit", default="HEAD")
    a = ap.parse_args(argv)
    if a.cmd == "qualify":
        t, why = qualify(a.runs)
        if t is None:
            print(f"timing-cohort: not qualified: {why}", file=sys.stderr)
            return 1
        Path(a.out).write_text(json.dumps({"runs": a.runs, "tests": t}, indent=2) + "\n")
        for n, v in t.items():
            print(f"{v['threshold']:8.2f} s  {n} ({v['min']:.2f} to {v['max']:.2f} s)")
        return 0
    thresholds = json.loads(Path(a.thresholds).read_text())["tests"] if a.thresholds else None
    lines = check(a.run, thresholds)
    if thresholds is None:
        print("timing-cohort: no thresholds given, so only failures count")
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
