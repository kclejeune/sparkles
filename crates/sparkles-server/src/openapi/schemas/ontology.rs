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
                "type":text,"ofn":text,"axiom":{"type":"object","description":"An OWL axiom in the public structural JSON form."},
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
            ],
            json!({
                "tier":string_enum(&["el","horn","disjunctive"]),"partial":{"type":"boolean"},"complete":{"type":"boolean"},
                "consistency":string_enum(&["consistent","inconsistent","unknown"]),"excludedCount":n,"incompleteCount":n
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
                "entity":{"anyOf":[{"type":"null"},obj(&["iri","present","unsatisfiable"],json!({"iri":text,"present":{"type":"boolean"},"unsatisfiable":nullable("boolean"),"representative":nullable("string"),"equivalenceCount":{"type":["integer","null"]},"children":{"type":["integer","null"]},"parents":{"type":["integer","null"]}}))]},
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
    put(
        "OntologyFrameClassification",
        obj(
            &["dataset", "commit", "classificationSource", "frame"],
            json!({
                "dataset":text,"commit":n,"classificationSource":sref("ClassificationSourceIdentity"),
                "frame":obj(&["iri","result","outcome","unsatisfiable","parents","equivalents","excluded","sources","retention"],json!({
                    "iri":text,"result":sref("ClassificationResult"),"outcome":sref("ClassificationOutcome"),"unsatisfiable":{"type":"boolean"},
                    "parents":projected("ClassificationParent"),"equivalents":projected("ClassificationName"),"excluded":projected("ClassificationExcluded"),
                    "sources":obj(&["total","truncated","items"],json!({"total":n,"truncated":{"type":"boolean"},"items":{"type":"array","maxItems":1000,"items":sref("ClassificationSourceRow")}})),
                    "retention":obj(&["kept","resultId"],json!({"kept":{"type":"boolean"},"resultId":nullable("string")}))
                }))
            }),
        ),
    );
}
