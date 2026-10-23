#!/usr/bin/env python3
"""Summarize completed bench-indexed-commit.py campaigns with medians, spreads and the
storage synchronization time of every process.

Each argument is the output directory of one campaign, the directory that holds
results.json. The report prints, for every people, index and scenario, the median and the
range of each metric with the ordinary writer and with grouping, and the ratio of the two
medians. For the serial scenarios it also prints the mean time of a commit
synchronization, read from the server's counters of each process, and the engine
overhead, which is the mean request latency minus that synchronization time. A
campaign on a drive whose flush latency moves between processes shows the move in the
synchronization column, and the overhead column shows whether the two writers differ
once the move is taken out. The report never changes a gate, and the pass or fail marks
come from gate-summary.json as the harness wrote them.
"""

import argparse
import json
import pathlib
import re
import statistics
import sys

SYNC = re.compile(r"^sparkles_commit_sync_seconds_(sum|count)\{[^}]*\} ([0-9.eE+-]+)$")


def sync_totals(path):
    """Seconds and count of commit synchronizations in one metrics scrape."""
    seconds = count = 0.0
    try:
        text = pathlib.Path(path).read_text()
    except FileNotFoundError:
        return None
    for line in text.splitlines():
        match = SYNC.match(line)
        if match:
            if match.group(1) == "sum":
                seconds += float(match.group(2))
            else:
                count += float(match.group(2))
    return seconds, count


def sync_mean_ms(run):
    """Mean milliseconds per commit synchronization during one process, or None."""
    after = sync_totals(run / "metrics.txt")
    before = sync_totals(run / "before" / "metrics.txt") or (0.0, 0.0)
    if after is None or after[1] <= before[1]:
        return None
    return (after[0] - before[0]) / (after[1] - before[1]) * 1000


def cell_directory(root):
    cells = [
        p
        for p in root.iterdir()
        if p.is_dir() and re.fullmatch(r"[a-z]+-[0-9]+", p.name)
    ]
    assert len(cells) == 1, f"{root} holds {len(cells)} cell directories"
    return cells[0]


def load(root):
    """Rows of one campaign with the synchronization mean of each process."""
    root = pathlib.Path(root)
    results = json.loads((root / "results.json").read_text())
    cell = cell_directory(root)
    seen = {}
    rows = []
    for result in results:
        if result.get("calibration"):
            continue
        key = (result["block"], result["scenario"])
        position = seen.get(key, 0)
        seen[key] = position + 1
        run = cell / f"b{result['block']}-{result['scenario']}-{position}"
        rows.append(dict(result, sync_mean_ms=sync_mean_ms(run), position=position))
    return rows


def spread(values):
    return [min(values), max(values)]


def paired(group, getter):
    """Ratios of grouped to ordinary between the two neighbours of each ABBA block.

    A block runs the ordinary writer, grouping, grouping and the ordinary writer again.
    Pairing positions 0 with 1 and 3 with 2 compares processes that ran next to each
    other, so a drive that changes state during a block moves both sides of a pair.
    """
    by_block = {}
    for r in group:
        by_block.setdefault(r["block"], {})[r["position"]] = r
    ratios = []
    for block in by_block.values():
        for off, on in ((0, 1), (3, 2)):
            if off in block and on in block:
                a, b = getter(block[off]), getter(block[on])
                if a is None or b is None or not a:
                    return None
                ratios.append(b / a)
    if not ratios:
        return None
    return {"median": statistics.median(ratios), "range": spread(ratios)}


def summarize(rows):
    """One entry per people, index and scenario."""
    out = []
    keys = sorted({(r["people"], r["index"], r["scenario"]) for r in rows})
    for people, index, scenario in keys:
        group = [
            r
            for r in rows
            if (r["people"], r["index"], r["scenario"]) == (people, index, scenario)
        ]
        variants = {
            name: [r for r in group if r["grouped"] == grouped]
            for name, grouped in (("off", False), ("on", True))
        }
        entry = {
            "people": people,
            "index": index,
            "scenario": scenario,
            "processes": {k: len(v) for k, v in variants.items()},
            "metrics": {},
        }

        def add(name, getter, variants=variants, entry=entry, group=group):
            sides = {}
            for side, rs in variants.items():
                values = [getter(r) for r in rs]
                if not values or any(v is None for v in values):
                    return
                sides[side] = {
                    "median": statistics.median(values),
                    "range": spread(values),
                }
            off = sides["off"]["median"]
            sides["ratio"] = sides["on"]["median"] / off if off else None
            sides["paired"] = paired(group, getter)
            entry["metrics"][name] = sides

        if scenario.startswith("serial-"):
            add("p50_ms", lambda r: r["writers"]["p50_ms"])
            add("p90_ms", lambda r: r["writers"]["p90_ms"])
            add("sync_ms", lambda r: r["sync_mean_ms"])
            add(
                "overhead_ms",
                lambda r: (
                    None
                    if r["sync_mean_ms"] is None
                    else r["writers"]["mean_ms"] - r["sync_mean_ms"]
                ),
            )
        else:
            add("tps", lambda r: r["tps"])
            add("peak_rss_mib", lambda r: r["peak_rss_kib"] / 1024)
            add("sync_ms", lambda r: r["sync_mean_ms"])
            if scenario == "mixed":
                add("reader_p50_ms", lambda r: r["readers"]["p50_ms"])
                add("reader_p99_ms", lambda r: r["readers"]["p99_ms"])
                add("max_schedule_lag_ms", lambda r: r["max_schedule_lag_ms"])
                add("write_p99_ms", lambda r: r["writers"]["p99_ms"])
        out.append(entry)
    return out


def marks(root):
    """Pass or fail of each check, as the harness wrote them."""
    path = pathlib.Path(root) / "gate-summary.json"
    if not path.exists():
        return {}
    return {
        (g["people"], g["index"], g["scenario"]): {
            name: check["passed"] for name, check in g["checks"].items()
        }
        for g in json.loads(path.read_text())["gates"]
    }


def render(entries, gate_marks):
    lines = []
    for e in entries:
        key = (e["people"], e["index"], e["scenario"])
        passed = gate_marks.get(key, {})
        label = " ".join(f"{k}={'ok' if v else 'FAIL'}" for k, v in passed.items())
        lines.append(
            f"{e['people']} {e['index']} {e['scenario']} "
            f"({e['processes']['off']}/{e['processes']['on']} processes) {label}".rstrip()
        )
        for name, m in e["metrics"].items():
            lines.append(
                f"    {name}: off {m['off']['median']:.3f} "
                f"[{m['off']['range'][0]:.3f}-{m['off']['range'][1]:.3f}], "
                f"on {m['on']['median']:.3f} "
                f"[{m['on']['range'][0]:.3f}-{m['on']['range'][1]:.3f}], "
                f"ratio {'n/a' if m['ratio'] is None else format(m['ratio'], '.3f')}"
                + (
                    ""
                    if not m.get("paired")
                    else f", paired {m['paired']['median']:.3f} "
                    f"[{m['paired']['range'][0]:.3f}-{m['paired']['range'][1]:.3f}]"
                )
            )
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("roots", nargs="+", help="campaign output directories")
    parser.add_argument("--json", action="store_true", help="print JSON, not text")
    args = parser.parse_args(argv)
    entries = []
    gate_marks = {}
    for root in args.roots:
        entries += summarize(load(root))
        gate_marks.update(marks(root))
    if args.json:
        print(json.dumps(entries, indent=2))
    else:
        print(render(entries, gate_marks))


if __name__ == "__main__":
    sys.exit(main())
