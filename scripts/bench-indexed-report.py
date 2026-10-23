#!/usr/bin/env python3
"""Summarize completed bench-indexed-commit.py campaigns with medians, spreads and the
storage synchronization time of every process.

Each argument is the output directory of one campaign, the directory that holds
results.json. The report prints, for every people, index and scenario, the median and the
range of each metric with the ordinary writer and with grouping, the ratio of the two
medians, and the ratio between neighbouring processes of each ABBA block. It also prints
the mean time of a commit synchronization, read from the server's counters of each
process. For a serial scenario it prints the engine overhead as well, which is the mean
request latency less the mean synchronization time, when the two cover the same commits.
The latency comes from the request timings, which include the warmup requests, and so do
the counters. A scenario whose counters hold other commits, such as the seeding writes of
the delete scenario or the embedding worker's own commits, has no overhead row. A campaign
on a drive whose flush latency moves between processes shows the move in the
synchronization row, and the overhead row shows whether the two writers still differ once
the move is taken out. The report never changes a gate, and the pass or fail marks come
from the gate-summary.json of each campaign as the harness wrote it.
"""

import argparse
import json
import pathlib
import re
import statistics
import sys

# The gates of X09 section 17.9 as amended on 2026-10-23. A serial process may be at most
# 5% slower than its ordinary neighbour. Four writers must gain the ratio that
# required_gain gives for the mean commit synchronization time of the ordinary processes.
SERIAL_LIMIT = 1.05
THROUGHPUT_BASE = 1.5
SYNC_FULL_MS = 1.2

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


def sync_delta(run):
    """Seconds and count of commit synchronizations during one process, or None."""
    after = sync_totals(run / "metrics.txt")
    before = sync_totals(run / "before" / "metrics.txt") or (0.0, 0.0)
    if after is None or after[1] <= before[1]:
        return None
    return after[0] - before[0], after[1] - before[1]


def sync_mean_ms(run):
    """Mean milliseconds per commit synchronization during one process, or None."""
    delta = sync_delta(run)
    return None if delta is None else delta[0] / delta[1] * 1000


def attempt_latencies(run):
    """Latencies in milliseconds of every request the client sent, warmups included."""
    path = run / "request-timings.tsv"
    if not path.exists():
        return []
    lines = path.read_text().splitlines()
    column = lines[0].split("\t").index("latency_ms")
    return [float(line.split("\t")[column]) for line in lines[1:]]


def overhead_ms(run):
    """Mean request latency less the mean synchronization time, over the same commits.

    The two cover the same commits only when the server counted one synchronization for
    each request the client sent. Otherwise there is no overhead.
    """
    delta = sync_delta(run)
    latencies = attempt_latencies(run)
    if delta is None or not latencies or round(delta[1]) != len(latencies):
        return None
    return statistics.fmean(latencies) - delta[0] / len(latencies) * 1000


def load(root):
    """Rows of one campaign with the synchronization figures of each process."""
    root = pathlib.Path(root)
    results = json.loads((root / "results.json").read_text())
    seen = {}
    rows = []
    for result in results:
        if result.get("calibration"):
            continue
        key = (result["index"], result["people"], result["block"], result["scenario"])
        position = seen.get(key, 0)
        seen[key] = position + 1
        run = (
            root
            / f"{result['index']}-{result['people']}"
            / f"b{result['block']}-{result['scenario']}-{position}"
        )
        rows.append(
            dict(
                result,
                sync_mean_ms=sync_mean_ms(run),
                overhead_ms=overhead_ms(run),
                position=position,
            )
        )
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


def required_gain(sync_ms):
    """Gain in transactions per second that four writers must reach with grouping.

    Grouping saves synchronization time and nothing else, so the requirement is the
    original 1.5 only when the ordinary path's mean commit synchronization takes
    SYNC_FULL_MS or longer. For a shorter synchronization it falls in proportion, to 1.0
    for a synchronization that takes no time.
    """
    return 1 + (THROUGHPUT_BASE - 1) * min(1.0, sync_ms / SYNC_FULL_MS)


def amended(scenario, metrics, ordinary):
    """Verdicts of the amended gates for one entry, or None for other scenarios."""
    if scenario.startswith("serial-"):
        verdicts = {}
        for name in ("p50_ms", "p90_ms"):
            pair = metrics.get(name, {}).get("paired")
            verdicts[name] = {
                "paired": pair and pair["median"],
                "limit": SERIAL_LIMIT,
                "passed": None if pair is None else pair["median"] <= SERIAL_LIMIT,
            }
        return verdicts
    if scenario == "writers":
        pair = metrics.get("tps", {}).get("paired")
        syncs = [r["sync_mean_ms"] for r in ordinary]
        if pair is None or not syncs or any(v is None for v in syncs):
            return {"throughput": {"passed": None}}
        sync_ms = statistics.median(syncs)
        required = required_gain(sync_ms)
        return {
            "throughput": {
                "paired": pair["median"],
                "ordinary_sync_ms": sync_ms,
                "required": required,
                "passed": pair["median"] >= required,
            }
        }
    return None


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
            add("overhead_ms", lambda r: r["overhead_ms"])
        else:
            add("tps", lambda r: r["tps"])
            add("peak_rss_mib", lambda r: r["peak_rss_kib"] / 1024)
            add("sync_ms", lambda r: r["sync_mean_ms"])
            if scenario == "mixed":
                add("reader_p50_ms", lambda r: r["readers"]["p50_ms"])
                add("reader_p99_ms", lambda r: r["readers"]["p99_ms"])
                add("max_schedule_lag_ms", lambda r: r["max_schedule_lag_ms"])
                add("write_p99_ms", lambda r: r["writers"]["p99_ms"])
        entry["amended"] = amended(scenario, entry["metrics"], variants["off"])
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
        for name, v in (e.get("amended") or {}).items():
            verdict = {True: "ok", False: "FAIL", None: "n/a"}[v["passed"]]
            detail = ""
            if "required" in v:
                detail = (
                    f" paired {v['paired']:.3f}, required {v['required']:.3f}"
                    f" at {v['ordinary_sync_ms']:.2f} ms"
                )
            elif v.get("paired") is not None:
                detail = f" paired {v['paired']:.3f}, limit {v['limit']:.2f}"
            lines.append(f"    amended gate {name}: {verdict}{detail}")
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
    campaigns = []
    for root in args.roots:
        entries = summarize(load(root))
        campaigns.append({"campaign": str(root), "entries": entries})
    if args.json:
        print(json.dumps(campaigns, indent=2))
    else:
        blocks = []
        for campaign, root in zip(campaigns, args.roots, strict=True):
            body = render(campaign["entries"], marks(root))
            blocks.append(f"campaign {campaign['campaign']}\n{body}")
        print("\n".join(blocks))


if __name__ == "__main__":
    sys.exit(main())
