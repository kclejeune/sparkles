#!/usr/bin/env python3
"""Describe qualified full-consumption pairs in common benchmark records.

The CLI validates the existing record format first. Ratios are descriptive sample
medians, not significance tests or evidence that one library leads every workload.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import math
from pathlib import Path
import re
import statistics
import subprocess
import sys

CONTRACT_PATH = Path(__file__).resolve().parents[1] / "bench-record.py"
SPEC = importlib.util.spec_from_file_location("binding_record_contract", CONTRACT_PATH)
CONTRACT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CONTRACT)
LAYERS = {
    "rust-native", "python-native", "python-rdflib", "node-native", "node-rdfjs",
    "jvm-jena", "jvm-bridge", "rust-http", "typescript-http",
}


def sample_median(row):
    samples = row.get("samples_ms")
    if not isinstance(samples, list) or not samples:
        return None
    if any(isinstance(x, bool) or not isinstance(x, (int, float)) for x in samples):
        return None
    try:
        numbers = [float(x) for x in samples]
    except OverflowError:
        return None
    if any(not math.isfinite(x) or x <= 0 for x in numbers):
        return None
    median = statistics.median(numbers)
    return median if math.isfinite(median) and median > 0 else None


def oracle_evidence(answer):
    evidence = answer.get("verification", {})
    if not isinstance(evidence, dict) or evidence.get("method") != "data-oracle":
        return None
    if not isinstance(evidence.get("policy"), str) or not evidence["policy"]:
        return None
    digests = tuple(evidence.get(key) for key in ("source_sha256", "query_sha256"))
    if any(not isinstance(x, str) or re.fullmatch(r"[0-9a-f]{64}", x) is None
           for x in digests):
        return None
    return (*digests, evidence["policy"])


def complete_answer_algorithm(answer):
    """Qualify existing producers' complete digests without rewriting raw evidence."""
    if type(answer.get("rows")) is not int or answer["rows"] < 0:
        return None
    digest = answer.get("hash")
    if not isinstance(digest, str):
        return None
    try:
        algorithm = CONTRACT.answer_algorithm(answer)
    except (ValueError, TypeError, AttributeError):
        return None
    patterns = {
        "canonical-sha256-v1": r"[0-9a-f]{16}",
        "row-sha256-v1": r"row-sha256-v1:[0-9a-f]{64}",
    }
    pattern = patterns.get(algorithm)
    return algorithm if pattern is not None and re.fullmatch(pattern, digest) else None


def build_scorecard(record, candidate, peer):
    """Apply comparison qualifications after ordinary common-record validation."""
    if type(record.get("record")) is not int or record["record"] != CONTRACT.RECORD:
        raise ValueError("scorecards require current common benchmark record version 3")
    if not candidate or not peer or candidate == peer:
        raise ValueError("candidate and peer must be different named engines")
    settings = record.get("settings", {})
    layer = settings.get("api_layer")
    pairs = {}
    seen = set()
    for row in record["results"]:
        clients = row["clients"]
        if isinstance(clients, bool) or not isinstance(clients, int) or clients < 1:
            raise ValueError("clients must be a positive integer")
        key = row["query"], row["mode"], clients, row["durability"]
        if key[1] not in CONTRACT.MODES or key[3] not in {None, "memory", "written", "synced"}:
            raise ValueError("unknown mode or durability")
        identity = row["engine"], key
        if identity in seen:
            raise ValueError("duplicate engine observation for one case")
        seen.add(identity)
        if row["engine"] in {candidate, peer}:
            pairs.setdefault(key, {})[row["engine"]] = row
    cases = []
    for key in sorted(pairs, key=lambda x: (*x[:3], x[3] or "")):
        arms = pairs[key]
        reasons = []
        if layer not in LAYERS:
            reasons.append("missing-or-unknown-api-layer")
        if settings.get("consumption") != "all-terms":
            reasons.append("full-consumption-not-attested")
        if key[1] in {"update", "throughput", "longrun"} and key[3] is None:
            reasons.append("write-durability-not-attested")
        for engine in (candidate, peer):
            row = arms.get(engine)
            if row is None:
                reasons.append(f"{engine}:missing")
                continue
            if row.get("api_layer", layer) != layer:
                reasons.append(f"{engine}:mixed-api-layer")
            if row.get("status") != "ok":
                reasons.append(f"{engine}:status-{row.get('status')}")
            if sample_median(row) is None:
                reasons.append(f"{engine}:invalid-or-missing-samples")
            answer = row.get("answer") or {}
            if answer.get("agrees") is not True or oracle_evidence(answer) is None:
                reasons.append(f"{engine}:independent-answer-not-qualified")
        if candidate in arms and peer in arms:
            a, b = (arms[e].get("answer") or {} for e in (candidate, peer))
            algorithms = (complete_answer_algorithm(a), complete_answer_algorithm(b))
            complete = algorithms[0] is not None and algorithms[0] == algorithms[1]
            if not complete or not a.get("hash") or (a.get("hash"), a.get("rows")) != (b.get("hash"), b.get("rows")):
                reasons.append("complete-fingerprints-do-not-match")
            if oracle_evidence(a) != oracle_evidence(b):
                reasons.append("source-query-oracle-evidence-does-not-match")
        ratio = None
        if not reasons:
            ratio = sample_median(arms[candidate]) / sample_median(arms[peer])
            if not math.isfinite(ratio) or ratio <= 0:
                reasons.append("unrepresentable-ratio")
                ratio = None
        cases.append({
            "query": key[0], "mode": key[1], "clients": key[2], "durability": key[3],
            "observations": arms, "comparable": not reasons, "reasons": reasons,
            "candidate_over_peer": ratio,
        })
    return {
        "scorecard": 1, "suite": record["suite"], "scale": record["scale"],
        "api_layer": layer, "candidate": candidate, "peer": peer, "cases": cases,
        "qualification": "Descriptive ratios only. No significance or universal leadership claim.",
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("record", type=Path)
    parser.add_argument("--candidate", required=True)
    parser.add_argument("--peer", required=True)
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()
    validated = subprocess.run(
        [sys.executable, str(CONTRACT_PATH), "validate", str(args.record)],
        capture_output=True, text=True,
    )
    if validated.returncode:
        parser.error(validated.stderr.strip() or validated.stdout.strip())
    result = build_scorecard(json.loads(args.record.read_text()), args.candidate, args.peer)
    text = json.dumps(result, indent=2, allow_nan=False) + "\n"
    if args.out:
        if args.out.resolve() == args.record.resolve():
            parser.error("output must not overwrite the raw input record")
        args.out.write_text(text)
    else:
        print(text, end="")


if __name__ == "__main__":
    main()
