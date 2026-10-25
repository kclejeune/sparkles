#!/usr/bin/env python3
"""Verify retained benchmark evidence and render explicitly selected query records.

This is a documentation renderer, not a benchmark runner. It never chooses a newest run,
changes raw evidence, pools runs or promotes a historical build to a current-build claim.
"""

import argparse
import hashlib
import html
import json
import math
from pathlib import Path
import statistics
import sys


ROOT = Path(__file__).resolve().parent.parent
DOCS = Path("docs/benchmarks")
ENGINE_COLORS = {"sparkles": "#1254a1", "jena": "#a84300", "qlever": "#21734b",
                 "fluree": "#8c3789", "oxigraph": "#555555"}


def read_json(path):
    return json.loads(path.read_text())


def require(condition, message):
    if not condition:
        raise ValueError(message)


def source(root, directory, entry):
    path = (directory / entry["path"]).resolve()
    require(path.is_relative_to((root / DOCS).resolve()), f"source escapes benchmark tree: {path}")
    raw = path.read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    require(digest == entry["sha256"], f"source hash mismatch: {path}")
    require(entry["run_id"] == "sha256-" + digest, f"run ID mismatch: {path}")
    return path, json.loads(raw)


def context_of(record):
    """Copy source metadata without inventing metadata for legacy assessment formats."""
    manifest = record.get("manifest")
    if isinstance(manifest, dict):
        manifest = {key: value for key, value in manifest.items() if not key.startswith("raw_")}
    results = record.get("results", [])
    context = {"suite": record.get("suite"), "scale": record.get("scale"),
               "manifest": manifest, "settings": record.get("settings"),
               "durability_classes": sorted({row["durability"] for row in results
                                              if isinstance(row, dict) and row.get("durability")})}
    if "record" not in record:
        fields = ("provenance", "measurement_runtime", "host", "source", "measurement_contract",
                  "baseline_runtime_commit", "base_commit", "candidate_commit", "native_compiler",
                  "native_sha256", "method", "measurement", "kind", "scope", "schema",
                  "assessment_version")
        context["legacy_metadata"] = {key: record[key] for key in fields if key in record}
    return context


def verify_catalogs(root):
    count = 0
    for path in sorted((root / DOCS).glob("*/RUNS.json")):
        catalog = read_json(path)
        require(catalog.get("catalog_version") == 1, f"unknown catalog version: {path}")
        seen = set()
        for entry in catalog["sources"]:
            _, record = source(root, path.parent, entry)
            require(entry["context"] == context_of(record), f"catalog context does not match source: {path}")
            expected_format = ("bench-record-" + str(record["record"]) if "record" in record
                               else "legacy-assessment")
            require(entry["format"] == expected_format, f"catalog format does not match source: {path}")
            require(entry["run_id"] not in seen, f"duplicate run ID: {path}")
            seen.add(entry["run_id"])
            count += 1
    return count


def prepare_record(record, selection):
    """Enforce the selected contract and expose excluded answers without ranking them."""
    require(selection["mode"] == "warm" and selection["clients"] == 1,
            "this renderer supports only single-client warm query records")
    require(record.get("record") in (1, 2, 3), "unknown common record version")
    require(record["suite"] == selection["suite"] and record["scale"] == selection["scale"],
            "selected suite or scale does not match source")
    manifest = record["manifest"]
    require(manifest["host"]["name"] == selection["host"], "selected host does not match source")
    require(manifest["sparkles"]["binary_commit"] == selection["binary_commit"],
            "selected binary revision does not match source")
    for key, value in selection["settings"].items():
        require(record["settings"].get(key) == value, f"selected setting does not match source: {key}")
    require(record["settings"].get("resultCache") is False, "result cache contract is unknown")
    require(manifest.get("data") and all(d.get("sha256") for d in manifest["data"].values()),
            "data checksums are missing")
    require(manifest["sparkles"].get("sha256") and manifest["sparkles"].get("scripts_commit"),
            "Sparkles binary or harness identity is missing")
    engines = selection["engines"]
    require(engines and len(set(engines)) == len(engines) and set(engines) <= ENGINE_COLORS.keys(),
            "invalid engine selection")
    for engine in engines:
        provenance = manifest["sparkles"] if engine == "sparkles" else manifest["engines"].get(engine, {})
        require(provenance.get("version") and provenance.get("sha256"),
                f"engine version or binary identity is missing: {engine}")
    rows = {}
    for row in record["results"]:
        if row["mode"] != selection["mode"] or row["clients"] != selection["clients"]:
            continue
        if row["engine"] not in engines:
            continue
        query = row["query"]
        require(row["engine"] not in rows.setdefault(query, {}), f"duplicate query/engine: {query}")
        if row.get("reused"):
            prior = manifest.get("reused", {}).get(row["engine"], {})
            current = manifest["engines"].get(row["engine"], {})
            require(prior.get("host") == manifest["host"]["name"], "carried-over host mismatch")
            require(prior.get("engine", {}).get("sha256") == current.get("sha256"),
                    "carried-over binary mismatch")
            require(prior.get("engine", {}).get("version") == current.get("version"),
                    "carried-over version mismatch")
        if row["status"] != "ok":
            cell = (None, row["status"])
        elif not row.get("answer") or row["answer"].get("agrees") is not True:
            cell = (None, "answer unqualified")
        elif not row["answer"].get("hash") or row["answer"].get("rows") is None:
            cell = (None, "answer evidence missing")
        else:
            samples = row.get("samples_ms", [])
            require(samples and all(isinstance(x, (int, float)) and not isinstance(x, bool)
                                    and math.isfinite(x) and x > 0 for x in samples),
                    f"invalid latency samples: {query}/{row['engine']}")
            median = statistics.median(samples)
            require(math.isclose(median, row["median_ms"], rel_tol=1e-6, abs_tol=0.00051),
                    f"recorded median disagrees with samples: {query}/{row['engine']}")
            cell = (median, "qualified")
        rows[query][row["engine"]] = (row, cell)
    require(rows, "selected contract has no results")
    for query, values in rows.items():
        identities = {(row["answer"]["hash"], row["answer"]["rows"])
                      for row, (median, _) in values.values() if median is not None}
        # A source claiming agreement for different answers cannot support this report.
        require(len(identities) <= 1, f"conflicting qualified answers: {query}")
        for engine in engines:
            if engine not in values:
                values[engine] = ({}, (None, "not recorded"))
    return rows


def chart(rows, engines, label):
    """Render a dependency-free SVG dot plot, with one measured median per point."""
    values = [cell[0] for query in rows.values() for _, cell in query.values() if cell[0] is not None]
    lo = math.floor(math.log10(min(values)))
    hi = max(lo + 1, math.ceil(math.log10(max(values))))
    left, right, top, step = 210, 940, 110, 34
    height = top + step * len(rows) + 55
    scale = lambda value: left + (math.log10(value) - lo) / (hi - lo) * (right - left)
    escape = html.escape
    svg = [f'<svg xmlns="http://www.w3.org/2000/svg" width="1000" height="{height}" viewBox="0 0 1000 {height}" role="img" aria-labelledby="title desc">',
           f'<title id="title">{escape(label)}</title>',
           '<desc id="desc">Warm query medians in milliseconds on a logarithmic horizontal axis. Lower is faster. Unqualified or failed results have no point. Dots on the same row represent the same answer fingerprint.</desc>',
           '<rect width="100%" height="100%" fill="white"/>',
           '<g font-family="sans-serif" font-size="13" fill="#222">',
           f'<text x="24" y="28" font-size="20">{escape(label)}</text>',
           '<text x="24" y="50">Measured medians in ms · logarithmic axis · lower is faster</text>']
    for i, engine in enumerate(engines):
        x = 24 + i * 185
        svg += [f'<circle cx="{x}" cy="76" r="4" fill="{ENGINE_COLORS[engine]}"/>',
                f'<text x="{x + 10}" y="80">{escape(engine)}</text>']
    for power in range(lo, hi + 1):
        x = scale(10 ** power)
        svg += [f'<line x1="{x:.2f}" x2="{x:.2f}" y1="96" y2="{height - 42}" stroke="#ddd"/>',
                f'<text x="{x:.2f}" y="{height - 20}" text-anchor="middle">{10 ** power:g}</text>']
    for i, (query, entries) in enumerate(sorted(rows.items())):
        y = top + i * step
        svg += [f'<text x="{left - 12}" y="{y + 4}" text-anchor="end">{escape(query)}</text>',
                f'<line x1="{left}" x2="{right}" y1="{y}" y2="{y}" stroke="#eee"/>']
        for n, engine in enumerate(engines):
            value, _ = entries[engine][1]
            if value is not None:
                svg.append(f'<circle cx="{scale(value):.2f}" cy="{y + (n - 2) * 4}" r="3.5" fill="{ENGINE_COLORS[engine]}"><title>{escape(query)} / {escape(engine)}: {value:.3f} ms</title></circle>')
    return "\n".join(svg + ['</g>', '</svg>', ''])


def render(root):
    verify_catalogs(root)
    directory = root / DOCS / "synthetic"
    selected = read_json(directory / "report.json")
    require(selected.get("report_version") == 1, "unknown report version")
    text = ['# Synthetic query records', '',
            'This report and its charts are generated by `scripts/bench-docs.py` from the',
            'explicit selections in [report.json](report.json). It supplements the imported',
            '[manual results](RESULTS.md). It reports medians from retained samples, while the',
            'manual report uses means and other statistics under its stated contracts.', '',
            'Every table uses one record and one warm-query contract. No samples are pooled',
            'across records, and these historical builds are not measurements of the current',
            'checkout. Only successful results with an agreeing answer fingerprint are plotted.',
            'Agreement is a majority screening rule, not an independent correctness proof.',
            'Failed, missing and unqualified results remain visible without a latency value.',
            'For `export-500k`, LIMIT without ORDER BY permits different rows, so agreement',
            'checks only the row count. Other query checks use the retained value fingerprints.',
            'No speedup, winner count or significance claim is inferred from these tables.', '']
    outputs = {}
    for selection in selected["records"]:
        path, record = source(root, directory, selection)
        rows = prepare_record(record, selection)
        engines = selection["engines"]
        manifest = record["manifest"]
        sparkles = manifest["sparkles"]
        label = f'Synthetic {record["scale"]} warm queries'
        name = selection["chart"]
        require(Path(name).name == name and name.endswith(".svg"), "invalid chart output name")
        outputs[directory / "figures" / name] = chart(rows, engines, label)
        text += [f'## {record["scale"]}', '', f'![{label}](figures/{name})', '',
                 f'Raw record: [{path.name}]({selection["path"]}).', '',
                 f'Run ID: `{selection["run_id"]}`.', '',
                 f'Host `{manifest["host"]["name"]}`, {manifest["host"]["cpu"]}, kernel',
                 f'`{manifest["host"]["kernel"]}`. Memory limit `{manifest.get("memory_limit", "unknown")}`.',
                 f'Sparkles binary revision `{sparkles["binary_commit"]}`, harness revision',
                 f'`{sparkles["scripts_commit"]}`. Timer `{record["settings"]["timer"]}`,',
                 f'{record["settings"]["warmup"]} warm-ups and {record["settings"]["runs"]} measured requests,',
                 f'{selection["clients"]} client, result cache off. The source manifest carries',
                 'the dataset checksums and full binary identities.', '',
                 '| Engine | Version | Measurement origin |', '|---|---|---|']
        for engine in engines:
            provenance = sparkles if engine == "sparkles" else manifest["engines"][engine]
            reuse = manifest.get("reused", {}).get(engine)
            origin = (f'Carried over from `{reuse["started"]}` to `{reuse["ended"]}`.' if reuse
                      else f'Measured from `{manifest["started"]}` to `{manifest["ended"]}`.')
            text.append(f'| {engine} | {provenance["version"]} | {origin} |')
        text += ['', 'The latency values below are recomputed medians in milliseconds. Warm read',
                 'results carry no write durability claim. Write comparisons require their own',
                 'synchronization contract.', '', '| Query | ' + ' | '.join(engines) + ' |',
                 '|---|' + '---:|' * len(engines)]
        for query, entries in sorted(rows.items()):
            cells = [f'{entries[e][1][0]:.3f}' if entries[e][1][0] is not None else entries[e][1][1]
                     for e in engines]
            text.append('| ' + query + ' | ' + ' | '.join(cells) + ' |')
        text.append('')
    outputs[directory / "RECORDS.md"] = "\n".join(text)
    return outputs


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="refuse stale generated files without writing")
    args = parser.parse_args()
    try:
        outputs = render(ROOT)
        stale = [p for p, data in outputs.items() if not p.exists() or p.read_text() != data]
        if args.check:
            require(not stale, "stale generated output: " + ", ".join(str(p.relative_to(ROOT)) for p in stale))
        else:
            for path, data in outputs.items():
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(data)
        print(f"Verified benchmark source hashes and {len(outputs)} generated files.")
        return 0
    except (KeyError, ValueError, OSError, TypeError) as error:
        print(f"bench-docs: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
