//! Feature bodies described member by member: reasoning requests and diagnostics,
//! full-text hits, vector index answers, spatial conversion, RDFS on read and ShEx.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    reasoning(put);
    search(put);
    rdfs(put);
    shex(put);
}

fn reasoning(put: &mut dyn FnMut(&str, J)) {
    let graphs =
        json!({ "type": "array", "items": string(), "description": "`default` or graph IRIs." });
    put(
        "ReasonRequest",
        doc(
            obj(
                &[],
                json!({
                    "profile": { "type": "string", "default": "rdfs", "examples": ["rdfs", "rdfs-simple", "owl-rl", "rules"] },
                    "rules": { "type": "string", "description": "The rules of the `rules` profile, in Jena's rule syntax." },
                    "vocabularies": { "type": "array", "items": string(), "examples": [["geosparql"]], "description": "Built-in vocabularies added to the profile." },
                    "geoDefaultGeometry": boolean(),
                    "rerun": { "type": "boolean", "description": "Re-run the recorded profile, rules, extras and input graphs." },
                    "full": { "type": "boolean", "description": "A full run instead of an incremental one." },
                    "dataGraphs": graphs.clone(),
                    "ontologyGraphs": graphs,
                    "imports": string_enum(&["none", "dataset", "fetch"]),
                    "locationMapping": array(json!({
                        "anyOf": [
                            obj(&["name", "altName"], json!({ "name": string(), "altName": string() })),
                            obj(&["prefix", "altPrefix"], json!({ "prefix": string(), "altPrefix": string() })),
                        ],
                    })),
                    "refreshImports": { "type": "boolean", "description": "Load again the imports that earlier runs fetched." },
                }),
            ),
            "A reasoning run: the profile, its rules and extras, and the input graphs.",
            "input-graphs-and-imports",
        ),
    );
    put(
        "AutoReasonRequest",
        doc(
            obj(
                &["enabled"],
                json!({
                    "enabled": boolean(),
                    "debounceSeconds": { "type": "number", "minimum": 0 },
                    "maxDelaySeconds": { "type": "number", "minimum": 0 },
                }),
            ),
            "The dataset's own setting for automatic re-runs.",
            "reasoning-status-and-diagnostics",
        ),
    );
    let severity = string_enum(&["inconsistency", "warning"]);
    put(
        "DiagnosticsReport",
        doc(
            obj(
                &[
                    "diagnosticsFormat",
                    "dataset",
                    "commit",
                    "computedAt",
                    "scope",
                    "status",
                    "note",
                    "checks",
                    "findings",
                ],
                json!({
                    "diagnosticsFormat": { "const": 1 },
                    "dataset": string(),
                    "commit": { "type": "integer", "description": "The commit checked." },
                    "computedAt": string(),
                    "scope": obj(&["graph", "inferences", "closure"], json!({
                        "graph": string_enum(&["default", "graphs"]),
                        "graphs": strings(),
                        "inferences": obj(&["included"], json!({
                            "included": boolean(),
                            "profile": string(),
                            "stale": nullable("boolean"),
                            "commitsSince": nullable("integer"),
                        })),
                        "closure": string_enum(&["subclass", "none"]),
                    })),
                    "status": string_enum(&["violations-found", "none-found", "incomplete"]),
                    "note": { "type": "string", "description": "The report never claims consistency." },
                    "checks": array(obj(
                        &["id", "rules", "severity", "status", "findings", "millis"],
                        json!({
                            "id": string(),
                            "rules": strings(),
                            "severity": severity.clone(),
                            "status": string_enum(&["violations", "none", "truncated", "timeout", "error"]),
                            "findings": int(),
                            "millis": int(),
                            "error": string(),
                        }),
                    )),
                    "findings": array(obj(
                        &["check", "rule", "severity", "focus", "evidence", "basis", "message"],
                        json!({
                            "check": string(),
                            "rule": string(),
                            "severity": severity,
                            "focus": sref("RdfTerm"),
                            "evidence": {
                                "type": "object",
                                "additionalProperties": { "anyOf": [sref("RdfTerm"), array(sref("RdfTerm"))] },
                            },
                            "basis": string_enum(&["asserted", "uses-inferences"]),
                            "message": string(),
                        }),
                    )),
                }),
            ),
            "OWL 2 RL inconsistency checks and their findings.",
            "reasoning-status-and-diagnostics",
        ),
    );
}

fn search(put: &mut dyn FnMut(&str, J)) {
    put(
        "TextHits",
        doc(
            obj(
                &["dataset", "commit", "limited", "hits"],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "limited": { "type": "boolean", "description": "As many hits as the limit." },
                    "hits": array(obj(
                        &["s", "score", "literal"],
                        json!({
                            "s": sref("RdfTerm"),
                            "score": nullable("number"),
                            "literal": with_desc(sref("RdfTerm"), "The literal, or with highlighting its fragments."),
                            "snippet": { "type": "string", "description": "With highlighting: the fragments as HTML, escaped, with the matches in `<mark>`." },
                            "g": sref("RdfTerm"),
                            "p": sref("RdfTerm"),
                        }),
                    )),
                }),
            ),
            "Full-text search hits, best first.",
            "full-text-search",
        ),
    );
    put(
        "VectorIndexCreated",
        doc(
            obj(
                &["index", "task"],
                json!({ "index": sref("VectorIndexStatus"), "task": sref("Task") }),
            ),
            "The index as created, and the task that builds it.",
            "vector-indexes",
        ),
    );
    put(
        "RecallReport",
        doc(
            obj(
                &["k", "samples", "ef", "recall", "hnswMs", "exactMs"],
                json!({
                    "k": int(),
                    "samples": int(),
                    "ef": { "type": "integer", "description": "The graph search's width." },
                    "recall": { "type": ["number", "null"], "minimum": 0, "maximum": 1, "description": "Recall@k against the exact search, `null` when the index has no vector to sample." },
                    "hnswMs": { "type": "number", "description": "Mean milliseconds per search through the graph." },
                    "exactMs": { "type": "number", "description": "Mean milliseconds per exact search." },
                }),
            ),
            "Recall@k of the graph search against the exact search.",
            "vector-indexes",
        ),
    );
    put(
        "GeoConvertRequest",
        doc(
            obj(
                &["literals"],
                json!({
                    "literals": {
                        "type": "array",
                        "maxItems": 10_000,
                        "items": obj(&["value", "datatype"], json!({
                            "value": { "type": "string", "description": "The lexical form." },
                            "datatype": { "type": "string", "examples": ["http://www.opengis.net/ont/geosparql#wktLiteral"] },
                        })),
                    },
                }),
            ),
            "Geometry literals to convert to GeoJSON.",
            "hulls-aggregates-jena-filter-functions-utm-and-conversion",
        ),
    );
    put(
        "GeoConvertResult",
        doc(
            obj(
                &["results"],
                json!({
                    "results": array(json!({
                        "oneOf": [
                            closed(&["geometry"], json!({ "geometry": any_object("A GeoJSON geometry in CRS84.") })),
                            closed(&["error"], json!({ "error": string() })),
                        ],
                    })),
                }),
            ),
            "One result per literal, in order: its geometry, or why it has none.",
            "hulls-aggregates-jena-filter-functions-utm-and-conversion",
        ),
    );
}

fn rdfs(put: &mut dyn FnMut(&str, J)) {
    put(
        "RdfsStatus",
        doc(
            json!({
                "anyOf": [
                    closed(&["enabled"], json!({ "enabled": { "const": false } })),
                    obj(
                        &["enabled", "source", "schema"],
                        json!({
                            "enabled": { "const": true },
                            "source": string_enum(&["upload", "graph"]),
                            "graph": { "type": "string", "description": "`source: graph`: the schema graph, or `default`." },
                            "schema": obj(
                                &[
                                    "classesWithSuperclasses",
                                    "propertiesWithSuperproperties",
                                    "propertiesWithDomains",
                                    "propertiesWithRanges",
                                    "skipped",
                                ],
                                json!({
                                    "classesWithSuperclasses": int(),
                                    "propertiesWithSuperproperties": int(),
                                    "propertiesWithDomains": int(),
                                    "propertiesWithRanges": int(),
                                    "skipped": { "type": "integer", "description": "Schema triples with a blank node or a literal, which are left out." },
                                }),
                            ),
                            "warnings": strings(),
                        }),
                    ),
                ],
            }),
            "The dataset's RDFS-on-read setting and the schema it gives at the head, or `{enabled: false}`.",
            "rdfs-on-read",
        ),
    );
    put(
        "RdfsRequest",
        doc(
            obj(
                &["graph"],
                json!({ "graph": { "type": "string", "description": "A graph IRI, or `default`." } }),
            ),
            "The graph that holds the schema. A schema document in an RDF syntax is sent with its own media type instead.",
            "rdfs-on-read",
        ),
    );
}

fn shex(put: &mut dyn FnMut(&str, J)) {
    put(
        "ShexRequest",
        doc(
            closed(
                &["schema"],
                json!({
                    "schema": { "type": "string", "description": "The schema text." },
                    "schemaFormat": string_enum(&["shexc", "shexj", "shexr"]),
                    "map": { "type": ["string", "array", "object"], "description": "A compact shape map, or a JSON one." },
                    "externs": { "type": "string", "description": "Definitions of the schema's EXTERNAL shapes." },
                    "imports": { "type": "object", "additionalProperties": string(), "description": "Bodies of imported schemas, by IRI." },
                    "base": string(),
                }),
            ),
            "The JSON envelope of a ShEx validation: the schema and everything that goes with it.",
            "shex-validation",
        ),
    );
    let term = sref("RdfTerm");
    let value = |kinds: &[&str]| {
        obj(
            &["kind", "value", "constraint"],
            json!({ "kind": string_enum(kinds), "value": term.clone(), "constraint": string() }),
        )
    };
    let failure = json!({
        "anyOf": [
            value(&["nodeKind", "datatype", "facet", "valueSet"]),
            obj(&["kind", "predicate", "inverse", "min", "max", "count"], json!({
                "kind": { "const": "cardinality" },
                "predicate": string(),
                "inverse": boolean(),
                "min": int(),
                "max": nullable("integer"),
                "count": int(),
            })),
            obj(&["kind", "predicate", "value"], json!({
                "kind": string_enum(&["closed", "extra"]),
                "predicate": string(),
                "value": term.clone(),
            })),
            obj(&["kind", "detail"], json!({ "kind": { "const": "noMatch" }, "detail": string() })),
            obj(&["kind", "shape", "value"], json!({
                "kind": { "const": "reference" },
                "shape": string(),
                "value": term.clone(),
            })),
            obj(&["kind", "shape"], json!({ "kind": string_enum(&["not", "external"]), "shape": string() })),
            obj(&["kind", "extension", "message"], json!({
                "kind": { "const": "semAct" },
                "extension": string(),
                "message": string(),
            })),
        ],
    });
    let appinfo = obj(
        &["failures"],
        json!({
            "failures": { "type": "array", "items": failure, "description": "Up to 8 failures." },
            "prints": strings(),
        }),
    );
    let status = string_enum(&["conformant", "nonconformant"]);
    put(
        "ShexReport",
        doc(
            obj(
                &["conforms", "counts", "results", "warnings", "millis"],
                json!({
                    "conforms": { "type": "boolean", "description": "Every association conforms." },
                    "counts": obj(&["conformant", "nonconformant"], json!({ "conformant": int(), "nonconformant": int() })),
                    "results": array(obj(
                        &["node", "shape", "status"],
                        json!({
                            "node": term.clone(),
                            "shape": { "anyOf": [term, closed(&["type"], json!({ "type": { "const": "start" } }))] },
                            "status": status.clone(),
                            "reason": { "type": "string", "description": "The first failure, in one line." },
                            "appinfo": appinfo.clone(),
                        }),
                    )),
                    "warnings": strings(),
                    "millis": int(),
                    "stats": obj(&["pairs", "evaluations", "waves"], json!({
                        "pairs": int(),
                        "evaluations": int(),
                        "waves": array(int()),
                    })),
                }),
            ),
            "A ShEx validation result, in shape-map order.",
            "shex-validation",
        ),
    );
    put(
        "ShexResultMap",
        doc(
            array(obj(
                &["node", "shape", "status"],
                json!({
                    "node": { "type": "string", "description": "In compact syntax." },
                    "shape": { "type": "string", "description": "`<iri>`, `_:label` or `START`." },
                    "status": status,
                    "reason": string(),
                    "appinfo": appinfo,
                }),
            )),
            "The ShapeMap draft's JSON result map (`format=shapemap`).",
            "shex-validation",
        ),
    );
}
