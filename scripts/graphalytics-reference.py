#!/usr/bin/env python3
"""Prepare small Graphalytics references and compare saved Sparkles SPARQL JSON.

No network, engine execution or timing is performed. See testsuite/graphalytics/README.md.
"""

import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import sys


FORMAT = {"kind": "sparkles-graphalytics-reference", "version": 1}
ROOT = Path(__file__).resolve().parents[1]
DEFAULT_MANIFEST = ROOT / "testsuite/graphalytics/manifest.json"
VERTEX = "urn:x-sparkles:graphalytics:vertex:"
VOCAB = "urn:x-sparkles:graphalytics:"
XSD = "http://www.w3.org/2001/XMLSchema#"
INFINITY = (1 << 63) - 1
MAX_BYTES = 16 * 1024 * 1024
MAX_VERTICES = 100_000
MAX_EDGES = 1_000_000
UINT = re.compile(r"0|[1-9][0-9]*")
NUMBER = re.compile(r"[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?")


class Invalid(ValueError):
    pass


def require(ok, message):
    if not ok:
        raise Invalid(message)


def fields(value, names, label):
    require(isinstance(value, dict) and set(value) == set(names), f"{label}: unsupported fields")


def uint(value, label, maximum=(1 << 64) - 1):
    require(isinstance(value, str) and UINT.fullmatch(value) is not None, f"{label}: invalid integer")
    n = int(value)
    require(n <= maximum, f"{label}: integer out of range")
    return n


def finite(value, label):
    require(isinstance(value, str) and NUMBER.fullmatch(value), f"{label}: expected numeric text")
    try:
        n = float(value)
    except ValueError as error:
        raise Invalid(f"{label}: invalid number") from error
    require(math.isfinite(n) and n >= 0, f"{label}: expected finite nonnegative number")
    return n


def read_bytes(path):
    require(path.is_file() and path.stat().st_size <= MAX_BYTES, f"{path}: missing or over 16 MiB")
    data = path.read_bytes()
    require(len(data) <= MAX_BYTES, f"{path}: over 16 MiB")
    return data


def unique_object(pairs):
    out = {}
    for key, value in pairs:
        require(key not in out, f"duplicate JSON key {key!r}")
        out[key] = value
    return out


def read_json(path):
    return json.loads(read_bytes(path), object_pairs_hook=unique_object,
                      parse_constant=lambda s: (_ for _ in ()).throw(Invalid(f"invalid JSON number {s}")))


def checked_file(root, spec):
    fields(spec, ["path", "sha256"], "file reference")
    name, digest = spec["path"], spec["sha256"]
    require(isinstance(name, str) and not Path(name).is_absolute(), "file path must be relative")
    path = (root / name).resolve()
    require(path.is_relative_to(root.resolve()), "file path escapes manifest directory")
    require(isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest), "invalid SHA256")
    data = read_bytes(path)
    require(hashlib.sha256(data).hexdigest() == digest, f"{name}: SHA256 mismatch")
    try:
        return data.decode("ascii")
    except UnicodeDecodeError as error:
        raise Invalid(f"{name}: expected ASCII") from error


def rows(text, count, label):
    for line, raw in enumerate(text.splitlines(), 1):
        values = raw.split()
        require(len(values) == count, f"{label}:{line}: expected {count} columns")
        yield values


def reference(text, algorithm):
    out = {}
    for key, value in rows(text, 2, algorithm):
        node = uint(key, "reference vertex")
        require(node not in out, f"{algorithm}: duplicate reference vertex {node}")
        out[node] = finite(value, "reference score") if algorithm == "pr" else uint(value, "reference value")
        require(len(out) <= MAX_VERTICES, "reference exceeds vertex bound")
        if algorithm == "bfs":
            require(out[node] <= INFINITY, "BFS reference value exceeds infinity sentinel")
    return out


def properties_agree(text, case):
    """Verify the adapter's parameters against the pinned original graph config."""
    values = {}
    for line in text.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        require("=" in line and "\\" not in line, "unsupported properties syntax")
        key, value = (s.strip() for s in line.split("=", 1))
        require(key not in values, "duplicate properties key")
        values[key] = value
    prefix = "graph." + case["name"] + "."
    want = {"vertex-file": Path(case["vertices"]["path"]).name,
            "edge-file": Path(case["edges"]["path"]).name,
            "directed": str(case["directed"]).lower(),
            "bfs.source-vertex": str(case["bfs_source"]),
            "pr.num-iterations": str(case["pagerank"]["iterations"])}
    require(all(values.get(prefix + k) == v for k, v in want.items()), "properties disagree with manifest")
    require(finite(values.get(prefix + "pr.damping-factor"), "properties damping") == case["pagerank"]["damping"], "properties damping disagrees")
    algorithms = {s.strip() for s in values.get(prefix + "algorithms", "").split(",")}
    require({"bfs", "wcc", "pr"} <= algorithms, "properties do not enable all three algorithms")
    require(not values.get(prefix + "vertex-properties.names") and not values.get(prefix + "vertex-properties.types"), "vertex properties unsupported")
    edge_properties = (values.get(prefix + "edge-properties.names", ""), values.get(prefix + "edge-properties.types", ""))
    require(edge_properties == (("weight", "real") if case["edge_columns"] == 3 else ("", "")), "unsupported edge properties")
    return values


def load_case(manifest, name):
    data = read_json(manifest)
    fields(data, ["format", "source", "cases"], "manifest")
    require(data["format"] == FORMAT and type(data["format"]["version"]) is int, "unsupported manifest format/version")
    fields(data["source"], ["repository", "commit", "license"], "source")
    require(all(isinstance(v, str) and v for v in data["source"].values()), "incomplete source provenance")
    require(re.fullmatch(r"[0-9a-f]{40}", data["source"]["commit"]), "invalid source commit")
    require(isinstance(data["cases"], list) and data["cases"], "cases must be a nonempty list")
    names = set()
    selected = None
    for case in data["cases"]:
        fields(case, ["name", "directed", "edge_columns", "vertices", "edges", "properties",
                      "bfs_source", "pagerank", "expected"], "case")
        require(isinstance(case["name"], str) and re.fullmatch(r"[a-z0-9][a-z0-9_-]*", case["name"]), "invalid case name")
        require(case["name"] not in names, "duplicate case name")
        names.add(case["name"])
        if case["name"] == name:
            selected = case
    require(selected is not None, f"unknown case {name!r}")
    case = selected
    require(type(case["directed"]) is bool, "directed must be boolean")
    require(type(case["edge_columns"]) is int and case["edge_columns"] in (2, 3), "only 2 or 3 edge columns supported")
    require(type(case["bfs_source"]) is int and case["bfs_source"] >= 0, "invalid BFS source")
    fields(case["pagerank"], ["iterations", "damping"], "pagerank")
    iterations, damping = case["pagerank"]["iterations"], case["pagerank"]["damping"]
    require(type(iterations) is int and 1 <= iterations <= 10_000, "unsupported PR iteration count")
    require(type(damping) in (int, float) and math.isfinite(damping) and 0 <= damping < 1, "unsupported PR damping")
    base = manifest.parent
    # Retain the original parameter file as part of the pinned input provenance.
    config = properties_agree(checked_file(base, case["properties"]), case)
    vertices = []
    for (value,) in rows(checked_file(base, case["vertices"]), 1, "vertices"):
        node = uint(value, "vertex")
        require(not vertices or node > vertices[-1], "vertices must be strictly increasing")
        vertices.append(node)
        require(len(vertices) <= MAX_VERTICES, "too many vertices")
    require(vertices and case["bfs_source"] in vertices, "empty graph or BFS source absent")
    nodes = set(vertices)
    edges = []
    for values in rows(checked_file(base, case["edges"]), case["edge_columns"], "edges"):
        u, v = (uint(x, "edge endpoint") for x in values[:2])
        require(u in nodes and v in nodes, "edge endpoint absent from vertex file")
        require(u != v, "self-loops unsupported by Graphalytics")
        require(case["directed"] or u < v, "undirected edge must list lower endpoint first")
        edge = (u, v)
        require(not edges or edge > edges[-1], "edges must be unique and lexicographically increasing")
        if len(values) == 3:
            finite(values[2], "edge weight")  # Checked but deliberately not used by these unweighted algorithms.
        edges.append(edge)
        require(len(edges) <= MAX_EDGES, "too many edges")
    fields(case["expected"], ["bfs", "wcc", "pr"], "expected algorithms")
    expected = {alg: reference(checked_file(base, ref), alg) for alg, ref in case["expected"].items()}
    require(all(set(values) == nodes for values in expected.values()), "reference vertex set differs from input")
    for field, count in [("vertices", len(vertices)), ("edges", len(edges))]:
        require(config.get(f"graph.{name}.meta.{field}") == str(count), f"properties {field} count disagrees")
    return data, case, vertices, edges, expected


def queries(case):
    prefix = f"PREFIX algo: <urn:x-sparkles:algo#>\nPREFIX ref: <{VOCAB}>\n"
    direction = "forward" if case["directed"] else "undirected"
    common = "algo:predicate ref:edge ; algo:nodeClass ref:Vertex ; algo:concurrency 1 ; algo:node ?node"
    bodies = {
        "bfs": f"algo:direction algo:{direction} ; algo:graphalytics true ; algo:source <{VERTEX}{case['bfs_source']}> ; algo:depth ?value",
        # WCC has no graphalytics parameter in the native API. Its labels need only preserve the partition.
        "wcc": "algo:direction algo:undirected ; algo:component ?value",
        "pr": f"algo:direction algo:{direction} ; algo:graphalytics true ; algo:dampingFactor {case['pagerank']['damping']!r} ; algo:maxIterations {case['pagerank']['iterations']} ; algo:score ?value",
    }
    services = {"bfs": "bfs", "wcc": "wcc", "pr": "pageRank"}
    return {alg: prefix + f"SELECT ?node ?value WHERE {{ SERVICE algo:{services[alg]} {{ [] {common} ; {body} . }} }}\n"
            for alg, body in bodies.items()}


def prepare(manifest, name, output):
    data, case, vertices, edges, _ = load_case(manifest, name)
    output.mkdir()  # An existing path is always refused. Never overwrite a store or input.
    rdf = output / "graph.nt"
    with rdf.open("w", encoding="ascii") as stream:
        for v in vertices:
            stream.write(f"<{VERTEX}{v}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{VOCAB}Vertex> .\n")
        for u, v in edges:
            stream.write(f"<{VERTEX}{u}> <{VOCAB}edge> <{VERTEX}{v}> .\n")
    for alg, query in queries(case).items():
        (output / f"{alg}.rq").write_text(query)
    plan = {"format": {"kind": "sparkles-graphalytics-preparation", "version": 1},
            "case": case, "source": data["source"], "manifest_sha256": hashlib.sha256(read_bytes(manifest)).hexdigest(),
            "vertices": len(vertices), "edges": len(edges), "unweighted": True,
            "engine_executed": False, "files": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(output.iterdir())}}
    (output / "preparation.json").write_text(json.dumps(plan, indent=2) + "\n")
    return plan


def actual_rows(path, algorithm):
    data = read_json(path)
    require(isinstance(data, dict) and set(data) <= {"head", "results"} and set(data) == {"head", "results"}, "expected SPARQL SELECT JSON")
    head = data["head"]
    require(isinstance(head, dict) and set(head) <= {"vars", "link"}, "invalid SELECT head")
    require(isinstance(head.get("vars"), list) and sorted(head["vars"]) == ["node", "value"], "expected node/value variables")
    fields(data["results"], ["bindings"], "results")
    bindings = data["results"]["bindings"]
    require(isinstance(bindings, list) and len(bindings) <= MAX_VERTICES, "invalid/excessive bindings")
    out = {}
    for binding in bindings:
        fields(binding, ["node", "value"], "binding")
        fields(binding["node"], ["type", "value"], "node binding")
        node, value = binding["node"], binding["value"]
        require(node["type"] == "uri" and isinstance(node["value"], str) and node["value"].startswith(VERTEX), "unexpected vertex IRI")
        key = uint(node["value"][len(VERTEX):], "actual vertex")
        require(key not in out, f"duplicate actual vertex {key}")
        require(isinstance(value, dict) and set(value) <= {"type", "value", "datatype"}, "invalid value binding")
        require(value.get("type") == "literal" and isinstance(value.get("value"), str), "expected literal value")
        lexical, datatype = value["value"], value.get("datatype")
        if algorithm == "bfs" and lexical == "inf":
            require(datatype in (None, XSD + "string"), "BFS inf must be a string")
            out[key] = INFINITY
        elif algorithm in ("bfs", "wcc"):
            require(datatype == XSD + "integer", "expected integer datatype")
            out[key] = uint(lexical, "actual value", INFINITY if algorithm == "bfs" else (1 << 64) - 1)
        else:
            require(datatype == XSD + "double", "expected double datatype")
            out[key] = finite(lexical, "actual score")
    return out


def compare(expected, actual, algorithm):
    require(set(actual) == set(expected), "actual vertex set differs from reference")
    if algorithm == "wcc":
        forward, reverse = {}, {}
        for node, reference_value in expected.items():
            actual_value = actual[node]
            require(forward.setdefault(reference_value, actual_value) == actual_value, "WCC component split")
            require(reverse.setdefault(actual_value, reference_value) == reference_value, "WCC components merged")
    else:
        for node, reference_value in expected.items():
            actual_value = actual[node]
            equal = (abs(actual_value - reference_value) <= 0.0001 * reference_value
                     if algorithm == "pr" else actual_value == reference_value)
            require(equal, f"{algorithm}: value mismatch at vertex {node}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=DEFAULT_MANIFEST)
    sub = parser.add_subparsers(dest="command", required=True)
    prep = sub.add_parser("prepare")
    prep.add_argument("case")
    prep.add_argument("output", type=Path)
    check = sub.add_parser("validate")
    check.add_argument("case")
    check.add_argument("algorithm", choices=["bfs", "wcc", "pr"])
    check.add_argument("actual", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.manifest, args.case, args.output)
            print("Prepared RDF and queries. No engine was executed.")
        else:
            data, case, _, _, expected = load_case(args.manifest, args.case)
            compare(expected[args.algorithm], actual_rows(args.actual, args.algorithm), args.algorithm)
            print(json.dumps({"format": {"kind": "sparkles-graphalytics-comparison", "version": 1},
                              "status": "reference-comparison-passed", "case": case["name"], "algorithm": args.algorithm,
                              "source": data["source"], "vertices": len(expected[args.algorithm]),
                              "actual_sha256": hashlib.sha256(read_bytes(args.actual)).hexdigest(),
                              "manifest_sha256": hashlib.sha256(read_bytes(args.manifest)).hexdigest(),
                              "engine_execution_verified": False}))
    except (Invalid, OSError, ValueError, TypeError, OverflowError) as error:
        print(f"graphalytics-reference: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
