#!/usr/bin/env python3
"""Read hyperfine's JSON export in the format of hyperfine 1 or hyperfine 2.

    bench_hyperfine.py merge NEW DEST     replace or add NEW's results in DEST by command name
    bench_hyperfine.py normalize FILE...  rewrite each FILE in the hyperfine 1 layout
    bench_hyperfine.py has FILE NAME...   exit 0 when FILE has a result for every NAME

Hyperfine 1 writes one result per command with the fields `command`, `mean`, `stddev`,
`median`, `min`, `max`, `user`, `system`, `times`, `memory_usage_byte`, `exit_codes` and,
for a parameter scan, `parameters`. Its `command` is the `--command-name` when one is given.
Hyperfine 2 writes `schema_version` 2 with `metadata` and `primary_metric` at the top, and
each result keeps the command line in `command`, the `--command-name` in `name`, the runs in
`measurements` and the statistics per metric in `summary`.

The harness reads every file through `normalize`, which gives the hyperfine 1 layout for
either format, and never reads the hyperfine 2 fields directly. A result converted from
hyperfine 2 also gets `hyperfine_version`. `merge` normalizes both files and writes the
hyperfine 1 layout, so a merged results file never mixes the two formats. A file with an
unknown `schema_version` is refused.
"""

import json
import os
import statistics
import sys

SCHEMAS = (1, 2)


class FormatError(ValueError):
    pass


def _values(measurements, metric):
    out = []
    for m in measurements:
        v = m.get(metric)
        out.append(v.get("value") if isinstance(v, dict) else v)
    return out


def _stat(summary, metric, key, values):
    """A summary statistic, computed from the runs when the summary lacks it."""
    s = summary.get(metric)
    if s is not None and key in s:
        return s[key]
    xs = [v for v in values if isinstance(v, (int, float))]
    if not xs:
        return None
    if key == "stddev":
        return statistics.stdev(xs) if len(xs) > 1 else None
    return {"mean": statistics.mean, "median": statistics.median, "min": min, "max": max}[key](xs)


def normalize_result(r, version=None):
    """One result in the hyperfine 1 layout. A result already in that layout is returned as is."""
    if "measurements" not in r:
        return r
    ms, summary = r["measurements"], r.get("summary") or {}
    if any("time_wall_clock" not in m for m in ms):
        raise FormatError(f"hyperfine result {r.get('name') or r.get('command')!r} has a run without time_wall_clock")
    times = _values(ms, "time_wall_clock")
    wall = lambda key: _stat(summary, "time_wall_clock", key, times)
    out = {
        "command": r.get("name") or r["command"],
        "mean": wall("mean"),
        "stddev": wall("stddev"),
        "median": wall("median"),
        "user": _stat(summary, "time_user", "mean", _values(ms, "time_user")),
        "system": _stat(summary, "time_system", "mean", _values(ms, "time_system")),
        "min": wall("min"),
        "max": wall("max"),
        "times": times,
    }
    if all("memory_peak_resident" in m for m in ms):
        out["memory_usage_byte"] = [int(v) for v in _values(ms, "memory_peak_resident")]
    out["exit_codes"] = [m.get("exit_code") for m in ms]
    if "parameters" in r:
        out["parameters"] = {k: v.get("value") if isinstance(v, dict) else v for k, v in r["parameters"].items()}
    if version:
        out["hyperfine_version"] = version
    return out


def normalize(doc):
    """The document in the hyperfine 1 layout: `{"results": [...]}` with 1.x fields."""
    if doc is None:
        return None
    if not isinstance(doc, dict) or not isinstance(doc.get("results", []), list):
        raise FormatError("not a hyperfine JSON export")
    schema = doc.get("schema_version", 1)
    if schema not in SCHEMAS:
        raise FormatError(f"unknown hyperfine JSON schema_version {schema!r}; this harness reads {SCHEMAS}")
    version = (doc.get("metadata") or {}).get("hyperfine_version") if schema == 2 else None
    out = {k: v for k, v in doc.items() if k not in ("schema_version", "metadata", "primary_metric", "results")}
    out["results"] = [normalize_result(r, version) for r in doc.get("results", [])]
    return out


def load(path):
    """The normalized export at `path`, or None when the file does not exist."""
    if not os.path.exists(path):
        return None
    with open(path) as f:
        return normalize(json.load(f))


def by_command(path):
    """{command: result} of the normalized export at `path`, empty when it does not exist."""
    return {r["command"]: r for r in (load(path) or {"results": []})["results"]}


def merge(new, dest):
    """Replace or add the results of `new` in `dest` by command name, in the hyperfine 1 layout."""
    n = load(new)
    if n is None:
        raise FileNotFoundError(new)
    old = load(dest) or {"results": []}
    cmds = {r["command"] for r in n["results"]}
    old["results"] = [r for r in old["results"] if r["command"] not in cmds] + n["results"]
    with open(dest, "w") as f:
        json.dump(old, f, indent=1)


def main(argv):
    if len(argv) == 3 and argv[0] == "merge":
        merge(argv[1], argv[2])
    elif len(argv) >= 2 and argv[0] == "normalize":
        for p in argv[1:]:
            d = load(p)
            if d is None:
                raise FileNotFoundError(p)
            with open(p, "w") as f:
                json.dump(d, f, indent=1)
    elif len(argv) >= 2 and argv[0] == "has":
        return 0 if set(argv[2:]) <= set(by_command(argv[1])) else 1
    else:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
