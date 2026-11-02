#!/usr/bin/env python3
"""List the tests in a nextest JUnit report that ran longer than a threshold.

GitHub's runners are several times slower than the hosts the local gate runs on, so a
test that takes a minute and a half locally can reach nextest's ten-minute limit there. The
`test` task runs this after nextest, and `scripts/ci.sh` repeats the list in its
summary so the warning is seen before a push.

Usage: nextest-slow.py JUNIT OUT [SECONDS]

Writes one `seconds binary test` line per slow test to OUT, slowest first, and prints
them. OUT is removed when no test is slow. It also writes one `attempts binary test` line
per test that passed only on a retry to flaky-tests.txt beside OUT, and prints them. Only
the tests that .config/nextest.toml lists as flaky get retries. The threshold defaults to 100 seconds, or
NEXTEST_SLOW_WARN_SECS when set. Always exits 0, since this is a warning.
"""

import os
import sys
import xml.etree.ElementTree as ET


def main():
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    junit, out = sys.argv[1], sys.argv[2]
    limit = float(sys.argv[3] if len(sys.argv) > 3 else os.environ.get("NEXTEST_SLOW_WARN_SECS", "100"))
    flaky_out = os.path.join(os.path.dirname(out), "flaky-tests.txt")
    for f in (out, flaky_out):
        if os.path.exists(f):
            os.remove(f)
    try:
        root = ET.parse(junit).getroot()
    except (OSError, ET.ParseError) as e:
        print(f"nextest-slow: cannot read {junit}: {e}", file=sys.stderr)
        return
    slow = []
    flaky = []
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            secs = float(case.get("time") or 0)
            if secs >= limit:
                slow.append((secs, suite.get("name", "?"), case.get("name", "?")))
            retried = len(case.findall("flakyFailure")) + len(case.findall("flakyError"))
            if retried and case.find("failure") is None and case.find("error") is None:
                flaky.append((retried + 1, suite.get("name", "?"), case.get("name", "?")))
    if flaky:
        lines = [f"{n} attempts {binary} {name}" for n, binary, name in flaky]
        os.makedirs(os.path.dirname(flaky_out) or ".", exist_ok=True)
        with open(flaky_out, "w") as f:
            f.write("\n".join(lines) + "\n")
        print(f"warning: {len(flaky)} test{'s' if len(flaky) != 1 else ''} passed only on a retry:")
        print("\n".join(lines))
    if not slow:
        return
    slow.sort(reverse=True)
    lines = [f"{secs:7.1f}s {binary} {name}" for secs, binary, name in slow]
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    with open(out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"warning: {len(slow)} test{'s' if len(slow) != 1 else ''} took {limit:g}s or longer, which may time out on GitHub runners:")
    print("\n".join(lines))


if __name__ == "__main__":
    main()
