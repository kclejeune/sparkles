//! Immutable classification identity, bounded reads and explicit frame projection.
use super::{array, nullable, obj, sref, string_enum};
use serde_json::{Value as J, json};

pub(super) fn put_all(put: &mut impl FnMut(&str, J)) {
    let n = json!({"type":"integer","minimum":0});
    let text = json!({"type":"string"});
    let null_ref = |name: &str| json!({"anyOf":[sref(name),{"type":"null"}]});
    let name = obj(
        &["iri", "label", "labelLang"],
        json!({
            "iri":text,"label":nullable("string"),"labelLang":nullable("string")
        }),
    );
    put("ClassificationName", name);
    put(
        "ClassificationHierarchyNode",
        obj(
            &[
                "iri",
                "label",
                "labelLang",
                "equivalenceCount",
                "children",
                "parents",
            ],
            json!({
                "iri":text,"label":nullable("string"),"labelLang":nullable("string"),
                "equivalenceCount":n,"children":n,"parents":n
            }),
        ),
    );
    put(
        "OntologyMaterializationSource",
        obj(
            &[
                "kind",
                "sourceId",
                "profile",
                "producerCommit",
                "commit",
                "boundStoreId",
                "familyId",
                "inputGraphs",
                "inputGraphCount",
                "inputGraphsTruncated",
                "warnings",
                "warningCount",
                "warningsTruncated",
                "complete",
                "consistency",
                "justifications",
            ],
            json!({
                "kind":{"type":"string","const":"materialization"},
                "sourceId":{"type":"string","pattern":"^sha256:[a-f0-9]{64}$"},
                "profile":string_enum(&["rdfs","owl-rl"]),"producerCommit":n,"commit":n,
                "boundStoreId":text,"familyId":text,
                "inputGraphs":{"type":"array","maxItems":64,"items":text},
                "inputGraphCount":n,"inputGraphsTruncated":{"type":"boolean"},
                "warnings":{"type":"array","maxItems":100,"items":text},
                "warningCount":n,"warningsTruncated":{"type":"boolean"},
                "complete":{"type":["boolean","null"],"enum":[null]},
                "consistency":{"type":"string","const":"unknown"},
                "justifications":{"type":"string","const":"unavailable"}
            }),
        ),
    );
    put(
        "ClassificationParent",
        obj(
            &["iri", "label", "labelLang", "equivalenceCount"],
            json!({
                "iri":text,"label":nullable("string"),"labelLang":nullable("string"),"equivalenceCount":n
            }),
        ),
    );
    put(
        "ClassificationSourceRow",
        json!({"oneOf":[
            obj(&["kind","ordinal","graph"], json!({"kind":{"const":"requestedGraph"},"ordinal":n,"graph":text})),
            obj(&["kind","ordinal","graph","requested"], json!({"kind":{"const":"resolvedGraph"},"ordinal":n,"graph":text,"requested":{"type":"boolean"}})),
            obj(&["kind","ordinal","iri","location","graph"], json!({"kind":{"const":"import"},"ordinal":n,"iri":text,"location":nullable("string"),"graph":nullable("string")})),
            obj(&["kind","ordinal","message"], json!({"kind":{"const":"warning"},"ordinal":n,"message":text}))
        ]}),
    );
    put(
        "ClassificationExcluded",
        obj(
            &["type", "ofn", "axiom", "gap"],
            json!({
                "type":text,"ofn":text,"axiom":{"type":"object","additionalProperties":true,"description":"An OWL axiom in the public structural JSON form."},
                "gap":{"oneOf":[
                    obj(&["kind","rule","message"], json!({"kind":{"const":"omitted"},"rule":text,"message":text})),
                    obj(&["kind","reason"], json!({"kind":{"const":"partiallyUsed"},"reason":text}))
                ]}
            }),
        ),
    );
    let producer = obj(
        &["assertions", "elOnly", "partialRequested", "expansion"],
        json!({
            "assertions":{"type":"boolean"},"elOnly":{"type":"boolean"},"partialRequested":{"type":"boolean"},"expansion":string_enum(&["cautious","eager"])
        }),
    );
    put(
        "ClassificationSourceIdentity",
        obj(
            &[
                "boundStoreId",
                "familyId",
                "commit",
                "inputFingerprint",
                "contentFingerprint",
                "producer",
            ],
            json!({
                "boundStoreId":text,"familyId":text,"commit":n,"inputFingerprint":text,"contentFingerprint":text,"producer":producer
            }),
        ),
    );
    put(
        "ClassificationResult",
        obj(
            &[
                "resultId",
                "classifiedCommit",
                "boundStoreId",
                "familyId",
                "inputFingerprint",
                "sourceFingerprint",
                "contentFingerprint",
                "imports",
                "mode",
                "assertions",
                "elOnly",
                "partialRequested",
                "expansion",
                "sourceCounts",
                "capabilities",
                "justifications",
            ],
            json!({
                "resultId":{"type":"string","format":"uuid"},"classifiedCommit":n,"boundStoreId":text,"familyId":text,
                "inputFingerprint":text,"sourceFingerprint":text,"contentFingerprint":text,
                "imports":string_enum(&["dataset","none"]),"mode":string_enum(&["lenient","strict"]),
                "assertions":{"type":"boolean"},"elOnly":{"type":"boolean"},"partialRequested":{"type":"boolean"},
                "expansion":string_enum(&["cautious","eager"]),
                "sourceCounts":obj(&["requestedGraphs","resolvedGraphs","imports","warnings"],json!({"requestedGraphs":n,"resolvedGraphs":n,"imports":n,"warnings":n})),
                "capabilities":array(string_enum(&["namedClassDirectParents","namedClassDirectChildren","namedClassRoots","namedClassEquivalents","namedClassUnsatisfiability"])),
                "justifications":{"const":"unavailable"}
            }),
        ),
    );
    put(
        "ClassificationOutcome",
        obj(
            &[
                "tier",
                "partial",
                "complete",
                "consistency",
                "excludedCount",
                "incompleteCount",
                "unmappedCount",
            ],
            json!({
                "tier":string_enum(&["el","horn","disjunctive"]),"partial":{"type":"boolean"},"complete":{"type":"boolean"},
                "consistency":string_enum(&["consistent","inconsistent","unknown"]),"excludedCount":n,"incompleteCount":n,"unmappedCount":n
            }),
        ),
    );
    let parts = string_enum(&[
        "status",
        "roots",
        "parents",
        "children",
        "equivalents",
        "sources",
        "excluded",
    ]);
    put(
        "KeptClassification",
        obj(
            &[
                "dataset",
                "commit",
                "status",
                "reason",
                "result",
                "outcome",
                "entity",
                "activity",
                "page",
                "staleCommit",
            ],
            json!({
                "dataset":text,"commit":n,"status":string_enum(&["available","running","mismatch","unavailable"]),
                "reason":nullable("string"),"result":null_ref("ClassificationResult"),"outcome":null_ref("ClassificationOutcome"),
                "entity":{"anyOf":[{"type":"null"},obj(&["iri","present","unsatisfiable","incomplete"],json!({"iri":text,"present":{"type":"boolean"},"unsatisfiable":nullable("boolean"),"incomplete":nullable("boolean"),"representative":nullable("string"),"equivalenceCount":{"type":["integer","null"]},"children":{"type":["integer","null"]},"parents":{"type":["integer","null"]}}))]},
                "activity":{"anyOf":[{"type":"null"},obj(&["kind","commit"],json!({"kind":text,"commit":n}))]},
                "staleCommit":{"type":["integer","null"],"minimum":0},
                "page":{"anyOf":[{"type":"null"},obj(&["part","about","offset","limit","total","items","nextOffset"],json!({
                    "part":parts,"about":nullable("string"),"offset":n,"limit":{"type":"integer","minimum":1,"maximum":1000},"total":n,
                    "items":{"type":"array","maxItems":1000,"items":{"anyOf":[sref("ClassificationName"),sref("ClassificationParent"),sref("ClassificationHierarchyNode"),sref("ClassificationSourceRow"),sref("ClassificationExcluded")]}},
                    "nextOffset":{"type":["integer","null"],"minimum":0}
                }))]}
            }),
        ),
    );
    let projected = |item: &str| {
        obj(
            &["total", "limit", "truncated", "items"],
            json!({
                "total":n,"limit":{"type":"integer","minimum":1,"maximum":1000},"truncated":{"type":"boolean"},"items":{"type":"array","maxItems":1000,"items":sref(item)}
            }),
        )
    };
    let num = json!({"type": "number", "minimum": 0});
    let structural_axiom = json!({
        "type": "object",
        "additionalProperties": true,
        "description": "An OWL axiom in the public structural JSON form."
    });
    let import_entry = json!({
        "type": "object",
        "required": ["iri", "graph"],
        "additionalProperties": true,
        "properties": {
            "iri": text,
            "graph": nullable("string")
        }
    });
    let excluded_entry = json!({
        "type": "object",
        "required": ["type", "ofn", "axiom"],
        "additionalProperties": true,
        "properties": {
            "type": text,
            "ofn": text,
            "axiom": structural_axiom,
            "rule": text,
            "message": text,
            "partial": text
        }
    });
    let stats_obj = json!({
        "type": "object",
        "required": [
            "incremental",
            "added",
            "removed",
            "cleared",
            "contexts",
            "conclusions",
            "estimatedBytes",
            "threads",
            "indexMs",
            "saturationMs",
            "taxonomyMs",
            "coverageMs",
            "coverageOver",
            "coverageStopped",
            "unmapped"
        ],
        "additionalProperties": true,
        "properties": {
            "incremental": {"type": "boolean"},
            "added": n,
            "removed": n,
            "cleared": n,
            "contexts": n,
            "conclusions": n,
            "estimatedBytes": n,
            "threads": n,
            "indexMs": num,
            "saturationMs": num,
            "taxonomyMs": num,
            "coverageMs": num,
            "coverageOver": {"type": "boolean"},
            "coverageStopped": {"type": "boolean"},
            "unmapped": n
        }
    });
    let node_entry = obj(
        &["node", "classes", "parents", "parentClasses", "children"],
        json!({
            "node": n,
            "classes": array(text.clone()),
            "parents": array(n.clone()),
            "parentClasses": array(text.clone()),
            "children": n
        }),
    );
    let axiom_entry = obj(
        &["type", "ofn", "axiom"],
        json!({
            "type": text,
            "ofn": text,
            "axiom": structural_axiom
        }),
    );
    let written_obj = json!({
        "type": "object",
        "required": [
            "added",
            "present",
            "removed",
            "absent",
            "declared",
            "addedAxioms",
            "removedAxioms",
            "axiomsTruncated",
            "inserted",
            "deleted",
            "kept",
            "committed"
        ],
        "additionalProperties": true,
        "properties": {
            "added": n,
            "present": n,
            "removed": n,
            "absent": array(text.clone()),
            "declared": array(text.clone()),
            "addedAxioms": array(axiom_entry.clone()),
            "removedAxioms": array(axiom_entry),
            "axiomsTruncated": {"type": "boolean"},
            "inserted": n,
            "deleted": n,
            "kept": n,
            "committed": {"type": "boolean"}
        }
    });
    let classification_required = [
        "dataset",
        "commit",
        "mode",
        "graphs",
        "imports",
        "warnings",
        "consistent",
        "consistency",
        "complete",
        "completeForTaken",
        "partial",
        "tier",
        "incompleteCount",
        "incomplete",
        "reused",
        "classes",
        "nodeCount",
        "unsatisfiableCount",
        "unsatisfiable",
        "unmappedTriples",
        "unmappedTruncated",
        "excludedCount",
        "excluded",
        "stats",
        "classificationSource",
        "resultId",
    ];
    let classification_props = json!({
        "dataset": text,
        "commit": n,
        "mode": string_enum(&["lenient", "strict"]),
        "graphs": array(text.clone()),
        "imports": array(import_entry),
        "warnings": array(text.clone()),
        "consistent": {"type": "boolean"},
        "consistency": string_enum(&["consistent", "inconsistent", "unknown"]),
        "complete": {
            "type": "boolean",
            "description": "Whether the answer is complete for the whole ontology: false when an axiom is left out or used only in part, or the run stopped. The classes outside `incomplete` have their complete answer either way."
        },
        "completeForTaken": {
            "type": "boolean",
            "description": "Whether the answer is complete for the axioms the run took, as a profile reasoner such as ELK reports it. False only for a partial answer."
        },
        "partial": {"type": "boolean"},
        "tier": string_enum(&["el", "horn", "disjunctive"]),
        "incompleteCount": n,
        "incomplete": array(text.clone()),
        "reused": {"type": "boolean"},
        "classes": n,
        "nodeCount": n,
        "unsatisfiableCount": n,
        "unsatisfiable": array(text.clone()),
        "unmappedTriples": {
            "type": "array",
            "description": "The first 100 triples of the input that map to no axiom, in the order the input gave them, as N-Triples terms. Each leaves the answer incomplete, and `stats.unmapped` counts them all. The OWL API calls such triples unparsed.",
            "items": {
                "type": "object",
                "required": ["subject", "predicate", "object"],
                "properties": {
                    "subject": {"type": "string"},
                    "predicate": {"type": "string"},
                    "object": {"type": "string"}
                }
            }
        },
        "unmappedTruncated": {
            "type": "boolean",
            "description": "Whether `stats.unmapped` counts more triples than `unmappedTriples` lists."
        },
        "excludedCount": n,
        "excluded": array(excluded_entry),
        "stats": stats_obj,
        "classificationSource": sref("ClassificationSourceIdentity"),
        "resultId": {"type": "string", "format": "uuid"},
        "nodes": array(node_entry),
        "offset": n,
        "next": n,
        "class": obj(
            &[
                "node",
                "classes",
                "parents",
                "parentClasses",
                "children",
                "iri",
                "unsatisfiable",
                "incomplete",
                "childClasses",
                "superClasses",
            ],
            json!({
                "node": n,
                "classes": array(text.clone()),
                "parents": array(n.clone()),
                "parentClasses": array(text.clone()),
                "children": n,
                "iri": text,
                "unsatisfiable": {"type": "boolean"},
                "incomplete": {"type": "boolean"},
                "childClasses": array(text.clone()),
                "superClasses": array(text.clone())
            }),
        ),
        "written": written_obj
    });
    let mut receipt_props = classification_props.clone();
    if let Some(m) = receipt_props.as_object_mut() {
        m.insert("datasetId".into(), text.clone());
        m.insert("committed".into(), json!({"type": "boolean"}));
        m.insert("commit".into(), sref("Commit"));
        m.insert("validation".into(), sref("ValidationSummary"));
    }
    let mut receipt_required: Vec<&str> = classification_required.to_vec();
    receipt_required.extend(["datasetId", "committed", "written"]);
    put(
        "OntologyClassification",
        obj(&classification_required, classification_props),
    );
    put(
        "OntologyClassificationReceipt",
        obj(&receipt_required, receipt_props),
    );
    put(
        "OntologyFrameClassification",
        obj(
            &["dataset", "commit", "classificationSource", "frame"],
            json!({
                "dataset":text,"commit":n,"classificationSource":sref("ClassificationSourceIdentity"),
                "frame":obj(&["iri","result","outcome","unsatisfiable","incomplete","parents","equivalents","excluded","sources","retention"],json!({
                    "iri":text,"result":sref("ClassificationResult"),"outcome":sref("ClassificationOutcome"),"unsatisfiable":{"type":"boolean"},"incomplete":{"type":"boolean"},
                    "parents":projected("ClassificationParent"),"equivalents":projected("ClassificationName"),"excluded":projected("ClassificationExcluded"),
                    "sources":obj(&["total","truncated","items"],json!({"total":n,"truncated":{"type":"boolean"},"items":{"type":"array","maxItems":1000,"items":sref("ClassificationSourceRow")}})),
                    "retention":obj(&["kept","resultId"],json!({"kept":{"type":"boolean"},"resultId":nullable("string")}))
                }))
            }),
        ),
    );
}
