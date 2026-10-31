#!/usr/bin/env python3
"""List the tests in a nextest JUnit report that ran longer than a threshold.

GitHub's runners are several times slower than the hosts the local gate runs on, so a
test that takes a minute and a half locally can reach nextest's ten-minute limit there. The
`test` task runs this after nextest, and `scripts/ci.sh` repeats the list in its
summary so the warning is seen before a push.

Usage: nextest-slow.py JUNIT OUT [SECONDS]

Writes one `seconds binary test` line per slow test to OUT, slowest first, and prints
them. OUT is removed when no test is slow. The threshold defaults to 100 seconds, or
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
    if os.path.exists(out):
        os.remove(out)
    try:
        root = ET.parse(junit).getroot()
    except (OSError, ET.ParseError) as e:
        print(f"nextest-slow: cannot read {junit}: {e}", file=sys.stderr)
        return
    slow = []
    for suite in root.iter("testsuite"):
        for case in suite.iter("testcase"):
            secs = float(case.get("time") or 0)
            if secs >= limit:
                slow.append((secs, suite.get("name", "?"), case.get("name", "?")))
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
