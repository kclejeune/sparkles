#!/usr/bin/env python3
"""Bind an identified SP2Bench timing record to independent complete source-oracle results.

Raw inputs remain unchanged. The bindings file pins their content hashes, source dataset,
queries, host, binary and measurement contract. Qualified output is a derived record with
explicit independent-data-oracle provenance, not an engine-majority vote.
Only common record versions 1, 2 and 3 are interpreted. Output paths must be distinct
from each other, the bindings file and every input, including filesystem aliases.
"""

import argparse
import copy
import hashlib
import json
import math
import pathlib
import re
import statistics

POLICY = "sp2bench-source-triple-reconstruction-v1"
CANONICALIZATION = {
    "fingerprint": "row-sha256-v1",
    "row": "UTF-8 Python repr of tuples of normalized RDF terms, projected variables sorted by name",
    "multiset": "SHA-256 of sorted lowercase SHA-256 row hashes, each followed by LF; duplicates retained",
    "exact_terms": "RDF term identity; xsd:string is explicit and language tags are lowercase",
    "numeric_values": "12 significant decimal digits; normalized Decimal spelling",
    "source_oracle": "Journal name-pair set unions for q4; earliest numeric year per author for q6; generated string names and integer years are checked explicitly",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def timing_summaries(row, timing):
    times = timing["times"]
    require(times and all(isinstance(value, (int, float)) and not isinstance(value, bool)
                         and math.isfinite(value) and value > 0 for value in times), "invalid raw timing samples")
    ordered = sorted(times)
    stats = {"mean": statistics.mean(times), "median": statistics.median(times),
             "stddev": statistics.stdev(times) if len(times) > 1 else 0.0,
             "min": min(times), "max": max(times)}
    for key, expected in stats.items():
        if key not in timing:
            continue
        actual = timing[key]
        require(isinstance(actual, (int, float)) and not isinstance(actual, bool) and math.isfinite(actual)
                and math.isclose(actual, expected, rel_tol=1e-9, abs_tol=1e-9), f"stale raw timing summary: {key}")
    expected_ms = {"median_ms": round(stats["median"] * 1000, 3),
                   "p95_ms": round(ordered[max(0, math.ceil(.95 * len(times)) - 1)] * 1000, 3)}
    for key, expected in expected_ms.items():
        actual = row.get(key)
        require(isinstance(actual, (int, float)) and not isinstance(actual, bool) and math.isfinite(actual)
                and math.isclose(actual, expected, rel_tol=0, abs_tol=5e-7), f"stale timing summary: {key}")
    for key in ("mean", "stddev", "min", "max"):
        field = key + "_ms"
        if field in row:
            actual = row[field]
            require(isinstance(actual, (int, float)) and not isinstance(actual, bool) and math.isfinite(actual)
                    and math.isclose(actual, stats[key] * 1000, rel_tol=1e-9, abs_tol=.00051),
                    f"stale timing summary: {field}")


def distinct_outputs(binding_path, bindings, outputs):
    inputs = [binding_path, *[binding_path.parent / item["path"] for item in bindings["inputs"].values()]]
    for index, output in enumerate(outputs):
        for other in [*inputs, *outputs[:index]]:
            require(output.resolve() != other.resolve()
                    and not (output.exists() and other.exists() and output.samefile(other)),
                    f"output collision: {output} aliases {other}")


def qualify(directory, bindings):
    directory = pathlib.Path(directory).resolve()
    require(bindings.get("bindings_version") == 1, "unknown bindings version")
    require(bindings.get("queries") and set(bindings["queries"]) <= {"q4", "q6"}, "unknown source-oracle query")
    sources = {}
    for role, item in bindings["inputs"].items():
        path = (directory / item["path"]).resolve()
        require(path.is_relative_to(directory), f"input escapes evidence directory: {role}")
        raw = path.read_bytes()
        require(hashlib.sha256(raw).hexdigest() == item["sha256"], f"changed input: {role}")
        sources[role] = json.loads(raw)
    record, answers, oracle = (sources[role] for role in ("raw_record", "raw_answers", "oracle"))
    require(type(record.get("record")) is int and record["record"] in (1, 2, 3), "unknown or missing record version")
    require(record.get("suite") == "sp2bench" and record.get("scale") == bindings["scale"], "wrong suite or scale")
    manifest = record["manifest"]
    require(manifest["host"]["name"] == bindings["host"], "wrong measurement host")
    require(manifest["sparkles"]["sha256"] == bindings["binary_sha256"], "wrong runtime binary")
    require(manifest["sparkles"]["binary_commit"] == bindings["binary_commit"], "wrong runtime revision")
    require(oracle.get("version") == 1 and oracle.get("method") == "source-triple-reconstruction", "unknown oracle contract")
    require(oracle["source_sha256"] == bindings["source_sha256"], "wrong oracle dataset")
    require(manifest["data"][bindings["dataset"]]["sha256"] == bindings["source_sha256"], "wrong benchmark dataset")
    for key, expected in bindings["settings"].items():
        require(record["settings"].get(key) == expected, f"wrong measurement setting: {key}")
    require(record["settings"].get("resultCache") is False, "result caching is not disabled")
    derived = copy.deepcopy(record)
    checks = {}
    require(len(derived["results"]) == len(bindings["queries"]), "unexpected timing rows")
    for row in derived["results"]:
        query = row["query"]
        require(query in bindings["queries"] and query not in checks, "duplicate or unexpected query")
        expected_query = bindings["queries"][query]
        require(row["engine"] == bindings["engine"] and row["mode"] == "warm" and row["clients"] == 1,
                "wrong engine or timing mode")
        require(row["status"] == "ok", "failed timing cannot be qualified")
        actual, expected = answers[query][bindings["engine"]], oracle["queries"][query]
        require(actual.get("query_sha256") == expected_query["sha256"] == expected.get("query_sha256"), "wrong query source")
        require(all(actual.get(key) == expected.get(key) for key in ("rows", "exact", "value")), "answer differs from source oracle")
        require(isinstance(actual["rows"], int) and not isinstance(actual["rows"], bool) and actual["rows"] >= 0, "invalid row count")
        require(all(re.fullmatch(r"row-sha256-v1:[0-9a-f]{64}", actual[key]) for key in ("exact", "value")), "wrong canonicalization algorithm")
        require(row["answer"]["rows"] == actual["rows"] and row["answer"]["hash"] == actual["value"], "timing answer is not the checked answer")
        timing = sources[expected_query["timing_role"]]["results"]
        require(len(timing) == 1 and timing[0]["command"] == bindings["engine"], "wrong raw timing source")
        timing = timing[0]
        samples = row["samples_ms"]
        require(len(samples) == len(timing["times"]) == expected_query["runs"], "wrong sample count")
        require(len(timing["exit_codes"]) == len(samples) and all(code == 0 for code in timing["exit_codes"]), "failed raw timing samples")
        require(all(isinstance(ms, (int, float)) and not isinstance(ms, bool) and math.isfinite(ms) and ms > 0
                    and isinstance(seconds, (int, float)) and not isinstance(seconds, bool) and math.isfinite(seconds) and seconds > 0
                    and math.isclose(ms, seconds * 1000, rel_tol=1e-9, abs_tol=0.00051)
                    for ms, seconds in zip(samples, timing["times"])), "changed timing samples")
        timing_summaries(row, timing)
        row["answer"]["agrees"] = True
        row["answer"]["verification"] = {"method": "independent-data-oracle", "policy": POLICY,
            "source_sha256": bindings["source_sha256"], "query_sha256": expected_query["sha256"],
            "oracle_sha256": bindings["inputs"]["oracle"]["sha256"],
            "raw_answers_sha256": bindings["inputs"]["raw_answers"]["sha256"]}
        checks[query] = {"rows": actual["rows"], "exact_matches": True, "value_matches": True,
                         "query_sha256": expected_query["sha256"], "timed_samples": len(samples), "all_exit_codes_zero": True}
    correctness = {"method": "independent-data-oracle", "policy": POLICY,
                   "canonicalization": CANONICALIZATION, "bindings": bindings, "queries": checks}
    derived["correctness"] = correctness
    derived["manifest"]["answer_validation"] = correctness
    receipt = {"validation_version": 1, "status": "pass", "correctness": correctness,
               "raw_sources_unchanged": True,
               "qualification": "Unchanged timing samples qualified by complete independent source reconstruction. The raw single-engine record remains unchanged with agrees=null."}
    return derived, receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bindings", type=pathlib.Path)
    parser.add_argument("out", type=pathlib.Path)
    parser.add_argument("receipt", type=pathlib.Path)
    args = parser.parse_args()
    bindings = json.loads(args.bindings.read_text())
    distinct_outputs(args.bindings, bindings, [args.out, args.receipt])
    record, receipt = qualify(args.bindings.parent, bindings)
    args.out.write_text(json.dumps(record, indent=1) + "\n")
    args.receipt.write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
