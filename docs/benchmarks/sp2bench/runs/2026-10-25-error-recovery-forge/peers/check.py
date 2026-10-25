"""Read-only verification of fresh peer/reference screening and retained timing files.

Run from any directory with Python3. This checks majority screening, not an independent
semantic oracle. Sparkles q4/q6 use the separate reviewed source-oracle qualifier.
"""
import hashlib
import json
import math
import pathlib
import statistics

HERE = pathlib.Path(__file__).resolve().parent
receipt = json.loads((HERE / "validation.json").read_text())
ref_path = (HERE / receipt["retained_reference"]["path"]).resolve()
assert hashlib.sha256(ref_path.read_bytes()).hexdigest() == receipt["retained_reference"]["sha256"]
reference = json.loads(ref_path.read_text())
expected_source = "4486653f3ea16825923eb8a5410137df7ba712aa918442531c33cbeb854eb814"
assert receipt["source_sha256"] == expected_source
assert reference["manifest"]["data"]["sp2bench-10m"]["sha256"] == expected_source
for name in ("qlever", "fluree"):
    record = json.loads((HERE / (name + "-record.json")).read_text())
    answers = json.loads((HERE / (name + "-answers.json")).read_text())
    manifest = record["manifest"]
    assert record["record"] == 3 and record["suite"] == "sp2bench" and record["scale"] == "10m"
    assert manifest["host"]["name"] == "forge" and manifest["memory_limit"] == "12G"
    assert manifest["data"]["sp2bench-10m"]["sha256"] == expected_source
    assert record["settings"]["warmup"] == 2 and record["settings"]["runs"] == 10
    for query, check in receipt["checks"].items():
        answer = answers.get(query, {}).get(name, {})
        if not isinstance(answer.get("rows"), int):
            continue
        full_hash = answer["value"] + ("/" + answer["order"] if answer.get("order") else "")
        candidates = [r for r in reference["results"] if r["query"] == query and r["engine"] in ("sparkles", "jena", "oxigraph") and r["status"] == "ok" and r.get("answer") and r["answer"].get("agrees") is not False]
        matches = [r["engine"] for r in candidates if r["answer"]["hash"] == full_hash and r["answer"]["rows"] == answer["rows"]]
        assert matches == check["retained_reference_matches"][name]
        assert answer["query_sha256"] == check["query_sha256"]
        timing = json.loads((HERE / (name + "-" + query + ".json")).read_text())
        timing = next(t for t in timing["results"] if t["command"] == name)
        row = next(r for r in record["results"] if r["engine"] == name and r["query"] == query)
        times = timing["times"]
        assert len(times) == 10 and all(t > 0 for t in times)
        scaled = [t * 1000 for t in times]
        assert row["samples_ms"] == [round(t, 3) for t in scaled]
        assert row["median_ms"] == round(statistics.median(scaled), 3)
        assert row["p95_ms"] == round(sorted(scaled)[math.ceil(.95 * len(times)) - 1], 3)
        assert math.isclose(timing["mean"], statistics.mean(times), rel_tol=1e-9)
        assert math.isclose(timing["stddev"], statistics.stdev(times), rel_tol=1e-9)
print("Fresh peer samples and full value/order/row reference screening verified.")
