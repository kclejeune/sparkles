//! The bodies of spec C18: model providers and their test calls.

use super::kit::*;
use super::*;

pub(super) fn put_all(put: &mut dyn FnMut(&str, J)) {
    models(put);
    tools(put);
    memory(put);
    asking(put);
    explaining(put);
    ingestion(put);
    maintenance(put);
    graphrag(put);
    evidence(put);
}

pub(super) fn evidence(put: &mut dyn FnMut(&str, J)) {
    let graphs = |d: &str| with_desc(strings(), d);
    put(
        "EvidenceSettings",
        doc(
            closed(
                &["format", "profile", "enabled", "sourceRecords", "proposals"],
                json!({
                    "format": { "const": 1 },
                    "profile": { "const": 1 },
                    "enabled": boolean(),
                    "sourceRecords": graphs("Graphs of source revisions, renditions and passages."),
                    "proposals": graphs("Graphs of claim occurrences, evidence references and evidence links."),
                    "decisions": graphs("Graphs of review decisions."),
                    "supports": graphs("Graphs of acceptance supports, which own projected triples."),
                    "identity": graphs("Graphs of identity proposals and resolution maps."),
                    "projections": graphs("Dedicated graphs of accepted projections, with no other role."),
                    "matchingPolicies": {
                        "type": "object",
                        "maxProperties": 64,
                        "propertyNames": { "pattern": "^[a-z0-9][a-z0-9-]{0,63}$" },
                        "description": "Named matching policies of identity proposals. Their names are distinct from the review policies' names, and the two kinds together hold at most 64 policies.",
                        "additionalProperties": closed(
                            &["version", "boundary", "decisions", "maps", "output"],
                            json!({
                                "version": with_desc(json!({ "type": "integer", "minimum": 1 }), "Raised whenever the policy's meaning changes."),
                                "boundary": graphs("The identity, sourceRecords and proposals graphs whose records the policy's reviews may use. At least one is an identity graph."),
                                "decisions": with_desc(string(), "The decisions graph the policy's reviews append to."),
                                "maps": with_desc(string(), "The identity graph that holds the policy's resolution map revisions."),
                                "output": with_desc(string(), "The projection graph that holds the current resolution map."),
                            }),
                        ),
                    },
                    "policies": {
                        "type": "object",
                        "maxProperties": 64,
                        "propertyNames": { "pattern": "^[a-z0-9][a-z0-9-]{0,63}$" },
                        "description": "Named review policies of claims and assessments.",
                        "additionalProperties": closed(
                            &["version", "admittedKinds", "conflicts", "boundary", "decisions", "supports", "output"],
                            json!({
                                "version": with_desc(json!({ "type": "integer", "minimum": 1 }), "Raised whenever the policy's meaning changes."),
                                "admittedKinds": with_desc(array(string_enum(&["reported", "observation", "hypothesis", "preference"])), "The claim kinds the policy can admit. Only positive claims are admitted."),
                                "requiredScope": graphs("Scope predicates an admitted claim must carry."),
                                "conflicts": string_enum(&["refuse", "admitLabelled"]),
                                "boundary": graphs("The sourceRecords and proposals graphs whose records the policy's reviews may use."),
                                "decisions": with_desc(string(), "The decisions graph the policy's reviews append to."),
                                "supports": with_desc(string(), "The supports graph that owns the policy's projected triples."),
                                "output": with_desc(string(), "The projection graph the policy writes."),
                            }),
                        ),
                    },
                }),
            ),
            "The evidence settings of a dataset, as `evidence.json` format 1 holds them.",
            "evidence-settings",
        ),
    );
    put(
        "EvidenceSettingsStatus",
        doc(
            closed(
                &["settings", "digest", "revision", "managed", "detached"],
                json!({
                    "settings": sref("EvidenceSettings"),
                    "digest": with_desc(or_null(string()), "The SHA-256 digest of `evidence.json`, `null` before the first configuration."),
                    "revision": with_desc(int(), "The revision of the managed-graph registry, 0 when there is none."),
                    "managed": graphs("Role graphs that ordinary writes cannot change."),
                    "detached": graphs("Role graphs that an administrator detached."),
                }),
            ),
            "Recorded evidence settings and the protection they have now.",
            "evidence-settings",
        ),
    );
    let uuid = |d: &str| with_desc(json!({ "type": "string", "format": "uuid" }), d);
    put(
        "EvidenceHold",
        doc(
            closed(
                &["id", "owner", "created", "commits"],
                json!({
                    "id": uuid("The hold's identity."),
                    "owner": with_desc(string_enum(&["evidence"]), "The profile that holds the commits."),
                    "created": with_desc(json!({ "type": "string", "format": "date-time" }), "When the hold was acquired."),
                    "commits": with_desc(array(closed(
                        &["branch", "branchId", "commit"],
                        json!({
                            "branch": with_desc(or_null(string()), "The branch's name, `null` once it is deleted."),
                            "branchId": uuid("The branch's identity."),
                            "commit": with_desc(json!({ "type": "integer", "minimum": 0 }), "The commit's sequence number."),
                        }),
                    )), "The held commits, sorted by branch identity and commit."),
                }),
            ),
            "A durable source hold that keeps commits readable.",
            "evidence-holds",
        ),
    );
    put(
        "EvidenceHoldRequest",
        doc(
            closed(
                &["commits"],
                json!({
                    "id": uuid("The hold's identity. A random one when absent."),
                    "commits": with_desc(json!({
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 65536,
                        "items": closed(
                            &["commit"],
                            json!({
                                "branch": with_desc(string(), "The branch by name. `main` when neither member is present."),
                                "branchId": uuid("The branch by identity, as an evidence reference names it."),
                                "commit": with_desc(json!({ "type": "integer", "minimum": 0 }), "The commit's sequence number."),
                            }),
                        ),
                    }), "The commits to keep readable."),
                }),
            ),
            "The commits a new source hold keeps.",
            "evidence-holds",
        ),
    );
    put(
        "EvidenceHoldList",
        doc(
            closed(
                &["holds", "next", "heldCommits", "maxCommits"],
                json!({
                    "holds": array(sref("EvidenceHold")),
                    "next": with_desc(or_null(json!({ "type": "string", "format": "uuid" })), "The `after` of the next page, `null` on the last."),
                    "heldCommits": with_desc(int(), "The commits all the evidence holds name together."),
                    "maxCommits": with_desc(int(), "The most commits the dataset's holds may name together."),
                }),
            ),
            "A page of the evidence holds, by id.",
            "evidence-holds",
        ),
    );
    put(
        "EvidenceDetach",
        doc(
            closed(
                &["graph"],
                json!({ "graph": with_desc(string(), "A protected graph of the settings' roles.") }),
            ),
            "The graph to stop protecting.",
            "evidence-settings",
        ),
    );
    let kinds = || string_enum(&KINDS);
    let digest = |d: &str| {
        with_desc(
            json!({ "type": "string", "pattern": "^sha256:[0-9a-f]{64}$" }),
            d,
        )
    };
    let commit = |d: &str| with_desc(json!({ "type": "integer", "minimum": 0 }), d);
    put(
        "EvidenceResolveRequest",
        doc(
            closed(
                &["references"],
                json!({
                    "references": with_desc(json!({
                        "type": "array",
                        "maxItems": 10000,
                        "items": closed(
                            &["key"],
                            json!({
                                "key": with_desc(json!({ "type": "string", "minLength": 1, "maxLength": 256 }), "The caller's unique name of the reference, which an answer names."),
                                "storedFact": closed(
                                    &["datasetId", "branchId", "commit", "triple"],
                                    json!({
                                        "datasetId": uuid("The dataset by identity, never by name."),
                                        "branchId": uuid("The branch by identity: the dataset's id for `main`."),
                                        "commit": commit("The exact commit of the branch that the fact is read at."),
                                        "graph": with_desc(or_null(string()), "The named graph's IRI. Absent or `null` for the default graph."),
                                        "triple": with_desc(string(), "The expected triple as one N-Triples 1.2 statement without its final `.`."),
                                        "reifier": with_desc(string(), "A stable reifier IRI that reifies the triple in the graph at the commit. Required when the triple has blank nodes."),
                                        "importMap": with_desc(string(), "A source import's blank-node mapping. Refused as `unanchored-evidence` by this release."),
                                    }),
                                ),
                                "record": closed(
                                    &["iri"],
                                    json!({
                                        "iri": with_desc(string(), "The record's IRI."),
                                        "kinds": with_desc(array(kinds()), "The kinds the record may have. Any profile kind when absent."),
                                        "contentDigest": digest("The content digest the record must carry."),
                                    }),
                                ),
                            }),
                        ),
                    }), "The references, verified in order. Each names exactly one of `storedFact` and `record`."),
                }),
            ),
            "Evidence references to verify at their exact retained views.",
            "evidence-resolution",
        ),
    );
    put(
        "EvidenceResolveResult",
        doc(
            closed(
                &["resolved"],
                json!({
                    "resolved": with_desc(array(closed(
                        &["key"],
                        json!({
                            "key": string(),
                            "storedFact": closed(
                                &["datasetId", "branchId", "commit", "servedBy", "graph", "reifier"],
                                json!({
                                    "datasetId": uuid("The dataset's identity."),
                                    "branchId": uuid("The branch the reference names."),
                                    "commit": commit("The commit the fact was verified at."),
                                    "servedBy": uuid("The branch whose history served the commit: the named branch, or the upstream that serves a commit at or below its starting point."),
                                    "graph": or_null(string()),
                                    "reifier": or_null(string()),
                                }),
                            ),
                            "record": closed(
                                &["iri", "kind", "graph", "receipt", "contentDigest", "recordDigest", "span"],
                                json!({
                                    "iri": string(),
                                    "kind": kinds(),
                                    "graph": with_desc(string(), "The protected evidence graph that holds the record."),
                                    "receipt": closed(
                                        &["iri", "datasetId", "branchId", "commit"],
                                        json!({
                                            "iri": string(),
                                            "datasetId": uuid("The dataset the record was published in."),
                                            "branchId": uuid("The branch it was published on."),
                                            "commit": commit("The commit that published it."),
                                        }),
                                    ),
                                    "contentDigest": or_null(digest("The record's content digest.")),
                                    "recordDigest": or_null(digest("The digest of the record's immutable content that its typed writer recorded.")),
                                    "span": or_null(closed(
                                        &["rendition", "start", "end", "textDigest"],
                                        json!({
                                            "rendition": string(),
                                            "start": commit("The first selected code point."),
                                            "end": commit("The code point after the selection."),
                                            "textDigest": digest("The digest of the selected text."),
                                        }),
                                    )),
                                }),
                            ),
                        }),
                    )), "Every reference, verified, in request order."),
                }),
            ),
            "The verified evidence references.",
            "evidence-resolution",
        ),
    );
    let key = |d: &str| {
        with_desc(
            json!({ "type": "string", "minLength": 1, "maxLength": 256 }),
            d,
        )
    };
    let iri = |d: &str| with_desc(string(), d);
    let record_ref = |d: &str| {
        with_desc(
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "key": { "type": "string", "description": "A record of this batch, by its key." },
                    "iri": { "type": "string", "description": "An existing authenticated record, by its IRI." },
                },
                "minProperties": 1,
                "maxProperties": 1,
            }),
            d,
        )
    };
    let method = || {
        closed(
            &["method", "version"],
            json!({ "method": iri("The method's IRI."), "version": key("The method's version.") }),
        )
    };
    let content = |d: &str| {
        with_desc(
            json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "text": { "type": "string", "description": "Retained UTF-8 text." },
                    "base64": { "type": "string", "description": "Retained bytes in base64." },
                    "digest": { "type": "string", "pattern": "^sha256:[0-9a-f]{64}$", "description": "The digest of content that is not retained." },
                },
                "minProperties": 1,
                "maxProperties": 1,
            }),
            d,
        )
    };
    let confidence = || {
        closed(
            &["scale", "value", "evaluator"],
            json!({
                "scale": iri("The declared scale, such as `urn:x-sparkles:evidence:ZeroToOne`."),
                "value": with_desc(string(), "An N-Triples literal, or a decimal such as `0.8`."),
                "evaluator": iri("Who evaluated it."),
                "method": method(),
            }),
        )
    };
    let terms = || {
        with_desc(
            json!({ "type": "object", "additionalProperties": array(string()) }),
            "Domain scope: predicate IRIs to N-Triples terms. Profile, PROV-O and structural predicates are refused.",
        )
    };
    let date_time = |d: &str| with_desc(json!({ "type": "string", "format": "date-time" }), d);
    put(
        "EvidenceProposal",
        doc(
            closed(
                &[],
                json!({
                    "dryRun": with_desc(boolean(), "Check and preview the write without committing it or holding commits."),
                    "ifHead": commit("Publish only while this commit is the branch head, else `409 head-changed`."),
                    "idempotencyKey": key("Makes a retry with the same payload answer the first receipt. The operation IRI is derived from the dataset, the caller and the key."),
                    "sourceGraph": iri("The source-record graph, needed when the settings name more than one."),
                    "proposalGraph": iri("The proposal graph, needed when the settings name more than one."),
                    "identityGraph": iri("The identity graph of the batch's identity proposals, needed when the settings name more than one."),
                    "message": with_desc(string(), "The commit message."),
                    "revisions": array(closed(
                        &["key", "source", "sourceKey", "content"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "source": iri("The continuing source identity."),
                            "sourceKey": key("The importer's stable source key."),
                            "upstreamVersion": key("The upstream version identifier."),
                            "content": content("The revision's bytes or their digest."),
                            "expectedDigest": digest("The digest the retained bytes must have."),
                            "locators": array(string()),
                            "reportedRights": array(string()),
                            "publishedAt": date_time("The source's publication time, with an explicit offset."),
                            "reportedAvailableAt": date_time("An attributed allegation of earlier availability."),
                            "upstreamRevisions": array(record_ref("An upstream revision.")),
                            "families": array(string()),
                        }),
                    )),
                    "renditions": array(closed(
                        &["key", "revision", "occurrenceKey", "conversion", "mediaType", "content"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "revision": record_ref("The revision it represents."),
                            "occurrenceKey": key("The rendition's key within its revision."),
                            "conversion": method(),
                            "mediaType": string(),
                            "language": string(),
                            "content": content("Normalized `text`, retained bytes or a digest."),
                            "expectedDigest": digest("The digest the content must have."),
                        }),
                    )),
                    "passages": array(closed(
                        &["key", "revision", "rendition", "occurrenceKey", "start", "end", "textDigest"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "revision": record_ref("The rendition's revision."),
                            "rendition": record_ref("A rendition of this batch with normalized text."),
                            "occurrenceKey": key("The passage's key within its rendition."),
                            "start": commit("The first selected Unicode code point."),
                            "end": commit("The code point after the selection."),
                            "textDigest": digest("The digest of the selected text."),
                            "quote": string(),
                        }),
                    )),
                    "references": array(closed(
                        &["key", "occurrenceKey"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "occurrenceKey": key("The reference's key within what it references."),
                            "storedFact": with_desc(json!({ "type": "object" }), "A stored fact in the form of `EvidenceResolveRequest`. A fact of this dataset is held for the operation."),
                            "passage": closed(
                                &["revision", "rendition", "passage"],
                                json!({
                                    "revision": record_ref("The passage's revision."),
                                    "rendition": record_ref("The passage's rendition."),
                                    "passage": record_ref("A passage of this batch."),
                                }),
                            ),
                            "structured": closed(
                                &["revision", "pointer", "method", "valueDigest"],
                                json!({
                                    "revision": record_ref("A retained JSON revision of this batch."),
                                    "pointer": with_desc(string(), "An RFC 6901 JSON Pointer."),
                                    "method": method(),
                                    "valueDigest": digest("The digest of the selected value."),
                                }),
                            ),
                        }),
                    )),
                    "claims": array(closed(
                        &["key", "occurrenceKey", "revision", "proposition", "kind", "polarity", "author"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "occurrenceKey": key("The claim's key within its revision."),
                            "revision": record_ref("The source revision that states it."),
                            "proposition": with_desc(string(), "One N-Triples 1.2 triple without its final `.` and without blank nodes. It is described, never asserted."),
                            "kind": string_enum(&["reported", "observation", "hypothesis", "preference"]),
                            "polarity": string_enum(&["positive", "negative", "uncertain"]),
                            "author": iri("The attributed source author, not the authenticated caller."),
                            "scope": terms(),
                            "confidence": closed(
                                &[],
                                json!({ "source": confidence(), "extraction": confidence(), "analyst": confidence() }),
                            ),
                        }),
                    )),
                    "links": array(closed(
                        &["key", "occurrenceKey", "target", "evidence", "relation", "author"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "occurrenceKey": key("The link's key within its target."),
                            "target": record_ref("A claim, assessment or identity proposal."),
                            "evidence": record_ref("An evidence reference."),
                            "relation": string_enum(&["supports", "contradicts", "mentions"]),
                            "author": iri("Who made the judgment."),
                        }),
                    )),
                    "assessments": array(closed(
                        &["key", "occurrenceKey", "target", "conclusion", "method", "configurationDigest", "evaluatedAt", "evaluator"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "occurrenceKey": key("The assessment's key within its target."),
                            "target": record_ref("A claim, assessment or identity proposal."),
                            "conclusion": with_desc(string(), "An N-Triples term."),
                            "method": method(),
                            "configurationDigest": digest("The digest of the method's configuration."),
                            "evaluatedAt": date_time("The evaluation time, with an explicit offset."),
                            "evaluator": iri("The attributed evaluator."),
                            "confidence": confidence(),
                            "scope": terms(),
                            "inputs": with_desc(json!({ "type": "array", "items": record_ref("A claim, assessment, identity proposal, source revision, review decision, acceptance support or resolution map revision."), "maxItems": 64 }), "The records the evaluation consumed, each once, written as `prov:used`."),
                        }),
                    )),
                    "identities": array(closed(
                        &["key", "occurrenceKey", "entities", "relation", "author"],
                        json!({
                            "key": key("The record's key in this batch."),
                            "occurrenceKey": key("The proposal's key within its set of entities."),
                            "entities": with_desc(json!({ "type": "array", "items": string(), "minItems": 2, "maxItems": 64 }), "The exact entity IRIs, distinct, in any order."),
                            "relation": string_enum(&["possibleMatch", "distinctEntities"]),
                            "preferredEntity": iri("For a possible match, the entity it prefers as its cluster's selected entity, one of `entities`."),
                            "author": iri("The attributed proposer."),
                            "method": method(),
                        }),
                    )),
                }),
            ),
            "One batch of evidence records. At most 1,000 claims, 10,000 links, 1,000 identity proposals and 16 MiB of decoded input.",
            "evidence-proposals",
        ),
    );
    put(
        "EvidenceProposalResult",
        doc(
            closed(
                &[
                    "operation",
                    "committed",
                    "replayed",
                    "commit",
                    "branchId",
                    "firstAvailableAt",
                    "hold",
                    "records",
                ],
                json!({
                    "operation": iri("The operation, which is every record's availability receipt."),
                    "committed": with_desc(boolean(), "Whether this request published a commit."),
                    "replayed": with_desc(boolean(), "Whether the idempotency key had published the same payload, whose receipt this is."),
                    "commit": commit("The publishing commit, the first one for a replay, or the head for a dry run."),
                    "branchId": uuid("The branch the commit published on."),
                    "firstAvailableAt": with_desc(or_null(string()), "The commit's time, the first availability of its source revisions. `null` for a dry run."),
                    "hold": with_desc(or_null(json!({ "type": "string", "format": "uuid" })), "The source hold of the stored facts of this dataset that the batch references."),
                    "records": array(closed(
                        &["key", "kind", "iri", "graph", "recordDigest", "existing"],
                        json!({
                            "key": string(),
                            "kind": kinds(),
                            "iri": string(),
                            "graph": string(),
                            "recordDigest": digest("The digest of the record's immutable content."),
                            "existing": with_desc(boolean(), "An earlier operation had published the record with equal content, so this one did not write it again."),
                        }),
                    )),
                    "dryRun": closed(
                        &["wouldCommit", "inserted", "deleted", "error"],
                        json!({
                            "wouldCommit": boolean(),
                            "inserted": int(),
                            "deleted": int(),
                            "error": or_null(string()),
                        }),
                    ),
                }),
            ),
            "The outcome of a claims proposal.",
            "evidence-proposals",
        ),
    );
    let dry_run = || {
        closed(
            &["wouldCommit", "inserted", "deleted", "error"],
            json!({
                "wouldCommit": boolean(),
                "inserted": int(),
                "deleted": int(),
                "error": or_null(string()),
            }),
        )
    };
    let changed = || {
        let change = || {
            array(closed(
                &["output", "triple"],
                json!({
                    "output": iri("The output graph."),
                    "triple": with_desc(string(), "The projected triple in N-Triples."),
                }),
            ))
        };
        with_desc(
            closed(
                &["supportsAdded", "supportsEnded", "inserted", "removed"],
                json!({
                    "supportsAdded": array(string()),
                    "supportsEnded": array(string()),
                    "inserted": change(),
                    "removed": change(),
                    "map": with_desc(
                        closed(
                            &["policy", "revision", "number", "previous", "decisionsAdded", "decisionsEnded", "entities"],
                            json!({
                                "policy": iri("The matching policy."),
                                "revision": iri("The resolution map revision the operation published."),
                                "number": int(),
                                "previous": with_desc(or_null(string()), "The revision it follows, whose consumers may need review."),
                                "decisionsAdded": array(string()),
                                "decisionsEnded": array(string()),
                                "entities": with_desc(array(string()), "The entities whose selected entity changed."),
                            }),
                        ),
                        "A review under a matching policy that changed which accepted decisions its resolution map follows.",
                    ),
                }),
            ),
            "The supports, projected triples and resolution map the operation changed, for consumers that track dependent results. Empty for a replay.",
        )
    };
    put(
        "EvidenceReview",
        doc(
            closed(
                &["policy", "policyVersion", "ifHead", "decisions"],
                json!({
                    "policy": with_desc(string(), "The name of a review policy or a matching policy of the evidence settings."),
                    "policyVersion": with_desc(int(), "The policy version the reviewer saw, else `409 policy-changed`."),
                    "ifHead": commit("The branch head the review was prepared at, else `409 head-changed`. Every read is made at this head."),
                    "idempotencyKey": key("Makes a retry with the same payload answer the first receipt."),
                    "dryRun": with_desc(boolean(), "Check and preview the review without committing it."),
                    "message": with_desc(string(), "The commit message."),
                    "decisions": array(closed(
                        &["key", "target", "targetDigest", "disposition"],
                        json!({
                            "key": key("The decision's key in this request."),
                            "target": iri("The reviewed claim or assessment, or under a matching policy the reviewed identity proposal."),
                            "targetDigest": digest("The target's record digest as the reviewer saw it, else `409 target-changed`."),
                            "disposition": string_enum(&["accepted", "rejected", "needsEvidence"]),
                            "supersedes": iri("The target's current decision under the policy, required when it has one, else `409 decision-superseded`."),
                        }),
                    )),
                }),
            ),
            "Review decisions under one policy.",
            "evidence-reviews",
        ),
    );
    put(
        "EvidenceReviewResult",
        doc(
            closed(
                &[
                    "operation",
                    "committed",
                    "replayed",
                    "commit",
                    "branchId",
                    "decisions",
                    "changed",
                ],
                json!({
                    "operation": iri("The operation, every decision's availability receipt."),
                    "committed": with_desc(boolean(), "Whether this request published a commit."),
                    "replayed": with_desc(boolean(), "Whether the idempotency key had published the same payload, whose receipt this is."),
                    "commit": commit("The publishing commit, the first one for a replay, or the head for a dry run."),
                    "branchId": uuid("The branch the commit published on."),
                    "decisions": array(closed(
                        &["key", "iri", "target", "disposition", "supersedes", "support", "notProjected", "conflicts"],
                        json!({
                            "key": string(),
                            "iri": string(),
                            "target": string(),
                            "disposition": string_enum(&["accepted", "rejected", "needsEvidence"]),
                            "supersedes": or_null(string()),
                            "support": with_desc(or_null(string()), "The acceptance support that owns the projected triple."),
                            "notProjected": with_desc(or_null(string()), "Why an acceptance did not project its claim."),
                            "conflicts": with_desc(array(string()), "The contrary links and claims an `admitLabelled` policy admitted the claim against."),
                        }),
                    )),
                    "changed": changed(),
                    "mapRevision": with_desc(or_null(string()), "The resolution map revision a review under a matching policy published, also for a replay."),
                    "dryRun": dry_run(),
                }),
            ),
            "The outcome of a review.",
            "evidence-reviews",
        ),
    );
    put(
        "EvidenceWithdrawal",
        doc(
            closed(
                &["withdrawals"],
                json!({
                    "ifHead": commit("Publish only while this commit is the branch head, else `409 head-changed`."),
                    "idempotencyKey": key("Makes a retry with the same payload answer the first receipt."),
                    "dryRun": with_desc(boolean(), "Check and preview the withdrawal without committing it."),
                    "message": with_desc(string(), "The commit message."),
                    "withdrawals": array(closed(
                        &["key", "target", "targetDigest"],
                        json!({
                            "key": key("The withdrawal's key in this request."),
                            "target": iri("The withdrawn claim occurrence."),
                            "targetDigest": digest("The claim's record digest, else `409 target-changed`."),
                        }),
                    )),
                }),
            ),
            "Claim occurrences to withdraw.",
            "evidence-reviews",
        ),
    );
    put(
        "EvidenceWithdrawalResult",
        doc(
            closed(
                &[
                    "operation",
                    "committed",
                    "replayed",
                    "commit",
                    "branchId",
                    "withdrawals",
                    "changed",
                ],
                json!({
                    "operation": iri("The operation, every withdrawal's availability receipt."),
                    "committed": with_desc(boolean(), "Whether this request published a commit."),
                    "replayed": with_desc(boolean(), "Whether the idempotency key had published the same payload."),
                    "commit": commit("The publishing commit, the first one for a replay, or the head for a dry run."),
                    "branchId": uuid("The branch the commit published on."),
                    "withdrawals": array(closed(
                        &["key", "iri", "target", "existing"],
                        json!({
                            "key": string(),
                            "iri": with_desc(or_null(string()), "The withdrawal record, `null` when the claim was already withdrawn."),
                            "target": string(),
                            "existing": with_desc(boolean(), "An earlier withdrawal had already withdrawn the claim."),
                        }),
                    )),
                    "changed": changed(),
                    "dryRun": dry_run(),
                }),
            ),
            "The outcome of a withdrawal.",
            "evidence-reviews",
        ),
    );
    put(
        "EvidenceIdentities",
        doc(
            closed(
                &["policy", "entities"],
                json!({
                    "policy": with_desc(string(), "The name of a matching policy of the evidence settings."),
                    "entities": with_desc(json!({ "type": "array", "items": string(), "minItems": 1, "maxItems": 100 }), "The entity IRIs to resolve."),
                }),
            ),
            "Entities to resolve through a matching policy's current resolution map.",
            "evidence-identities",
        ),
    );
    put(
        "EvidenceIdentitiesResult",
        doc(
            closed(
                &["policy", "revision", "number", "commit", "entities"],
                json!({
                    "policy": iri("The matching policy."),
                    "revision": with_desc(or_null(string()), "The latest resolution map revision, `null` before the first."),
                    "number": with_desc(int(), "Its number, 0 before the first."),
                    "commit": commit("The commit the map was read at."),
                    "entities": array(closed(
                        &["entity", "representative", "members", "membersComplete", "decisions"],
                        json!({
                            "entity": string(),
                            "representative": with_desc(string(), "The entity the map selects, the entity itself when the map does not name it."),
                            "members": with_desc(array(string()), "The members of its cluster, sorted, at most 1,000."),
                            "membersComplete": boolean(),
                            "decisions": with_desc(array(string()), "The current accepted decisions that join the cluster."),
                        }),
                    )),
                }),
            ),
            "What the current resolution map selects for each entity.",
            "evidence-identities",
        ),
    );
    put(
        "EvidenceDependents",
        doc(
            closed(
                &["changed"],
                json!({
                    "changed": with_desc(json!({ "type": "array", "items": string(), "minItems": 1, "maxItems": 1000 }), "The record identities that changed, such as a withdrawn claim, an ended support or the revision a new map revision follows."),
                    "transitive": with_desc(boolean(), "Also find the assessments that depend on a dependent. Defaults to `false`."),
                }),
            ),
            "Changed evidence records whose dependent assessments to find.",
            "evidence-dependents",
        ),
    );
    put(
        "EvidenceDependentsResult",
        doc(
            closed(
                &["commit", "dependents"],
                json!({
                    "commit": commit("The commit the records were read at."),
                    "dependents": array(closed(
                        &["assessment", "graph", "on", "dependence", "depth"],
                        json!({
                            "assessment": iri("An authenticated assessment."),
                            "graph": iri("Its proposal graph."),
                            "on": iri("The changed record it depends on, or for a transitive dependent the assessment it depends on."),
                            "dependence": string_enum(&["target", "used"]),
                            "depth": with_desc(int(), "How many assessments lie between it and a changed record, 0 for a direct dependent."),
                        }),
                    )),
                }),
            ),
            "The assessments that depend on the changed records, each once at its smallest depth.",
            "evidence-dependents",
        ),
    );
}

const KINDS: [&str; 16] = [
    "SourceRevision",
    "Rendition",
    "Passage",
    "EvidenceRef",
    "Claim",
    "EvidenceLink",
    "Assessment",
    "Note",
    "Decision",
    "Task",
    "ReviewDecision",
    "IdentityProposal",
    "AcceptanceSupport",
    "Withdrawal",
    "Projection",
    "ResolutionMap",
];

fn explaining(put: &mut dyn FnMut(&str, J)) {
    put(
        "ExplainRequest",
        doc(
            closed(
                &["query"],
                json!({
                    "query": with_desc(string(), "Any query form. An update is refused with `not-a-query`."),
                    "profile": with_desc(string_enum(&["estimate", "run", "given"]), "`estimate`, the default, plans without running. `run` runs the query read-only as the caller and counts its rows without returning them. `given` explains `plan`."),
                    "plan": {
                        "description": "With `profile: \"given\"`, the plan the client received, at most 10,000 nodes and 2 MiB.",
                        "oneOf": [sref("PlanNode"), sref("CursorPlan")],
                    },
                    "commit": with_desc(int(), "With `profile: \"given\"`, the commit of that plan."),
                    "error": any_object("With `profile: \"given\"`, the error body of the run that stopped, which names the budget."),
                    "describe": with_desc(boolean(), "Whether to call the `explain` role. `true` by default when the dataset enables `explain`."),
                    "timeoutSeconds": with_desc(num(), "The deadline of a run, 30 by default."),
                    "at": { "description": "A commit or time to read, as for `/{ds}/sparql`." },
                    "branch": string(),
                    "reasoning": boolean(),
                }),
            ),
            "A query to explain.",
            "explaining-a-query",
        ),
    );
    let note = obj(
        &["node", "code", "severity", "text", "source"],
        json!({
            "node": or_null(string()),
            "code": string(),
            "severity": string_enum(&["high", "warning", "info"]),
            "text": string(),
            "source": string_enum(&["explain", "planner", "lint", "schema", "model"]),
            "range": any_object("The editor range of a lint finding that names no node."),
        }),
    );
    let sentence = obj(
        &["text", "nodes"],
        json!({ "text": string(), "nodes": strings() }),
    );
    put(
        "ExplainResult",
        doc(
            obj(
                &[
                    "dataset",
                    "queryType",
                    "profile",
                    "executed",
                    "plan",
                    "nodes",
                    "notes",
                    "explanation",
                ],
                json!({
                    "dataset": string(),
                    "commit": or_null(int()),
                    "branch": string(),
                    "queryType": string(),
                    "profile": string_enum(&["estimate", "run", "given"]),
                    "executed": with_desc(boolean(), "Whether the plan has actual counts."),
                    "plan": { "oneOf": [sref("PlanNode"), sref("CursorPlan")] },
                    "estimatedRows": or_null(num()),
                    "rows": with_desc(int(), "After a run that finished, its rows."),
                    "elapsedMs": num(),
                    "stop": any_object("The budget that stopped the query: `budget`, `limit` and `elapsedMs`."),
                    "error": any_object("The error body of a run that a budget stopped."),
                    "warnings": array(obj(&["code", "message"], json!({ "code": string(), "message": string() }))),
                    "nodes": array(any_object("One operator: `id`, `operator`, `description`, `columns`, `estimatedRows` (null when hidden), `estimatedCost`, `actualRows`, `timeMs`, `selfMs`, `complete`, `partial`, `skipped`, `stoppedEarly`, `cached`, `runs` and `pushedFilters`.")),
                    "notes": array(note.clone()),
                    "shownNotes": with_desc(int(), "How many notes to show before **More**."),
                    "hiddenEstimates": boolean(),
                    "explanation": obj(
                        &["source", "asks", "notes"],
                        json!({
                            "source": string_enum(&["template", "model"]),
                            "asks": array(sentence.clone()),
                            "notes": array(note),
                            "provider": string(),
                            "model": string(),
                            "dropped": int(),
                            "replaced": int(),
                            "template": obj(&["asks"], json!({ "asks": array(sentence) })),
                            "fallback": { "description": "Why the template text is shown although a model was asked." },
                        }),
                    ),
                    "usage": any_object("The model calls: tokens, `modelCalls`, `failedCalls` and `steps`. A provider that used its prompt cache adds `cacheWriteInputTokens`, `cacheWrite1hInputTokens`, `cacheReadInputTokens` and `budgetTokens`."),
                }),
            ),
            "The explanation in the JSON form: the members of the `plan` and `notes` events, and the `explanation` and `usage` events as members.",
            "explaining-a-query",
        ),
    );
}

/// `POST /$/ingest/{ds}` and its tasks (C18 Phase 4).
fn ingestion(put: &mut dyn FnMut(&str, J)) {
    let options = json!({
        "format": with_desc(string(), "The media type of `text` or of the file, such as `text/html` or `application/pdf`."),
        "name": with_desc(string(), "The file name, which may say the format."),
        "url": with_desc(string(), "Where the document comes from; fetched through the outbound policy when there is no text or file, and the source's IRI by default."),
        "title": string(),
        "iri": with_desc(string(), "The source's IRI."),
        "graph": with_desc(string(), "The named graph of the source and its facts."),
        "profile": with_desc(string(), "The ingest profile (default `default`)."),
        "mode": with_desc(string_enum(&["branch", "preview", "auto", "memory", "claims"]), "`memory` extracts a registered `source` of agent memory and writes its facts on main into the source's graph, which `agentGraphs` must match. `claims` registers the source and proposes its facts as claims of one evidence batch in the same commit, for a review policy to project; it needs enabled evidence settings, `write` on the update endpoint, update grants on the evidence graphs and `keepText`."),
        "branch": with_desc(string(), "The review branch (default `ingest.<slug>-<n>`). In `claims` mode, an existing branch for the commit, `main` by default."),
        "sourceGraph": with_desc(string(), "In `claims` mode, the evidence source-record graph when the settings name several."),
        "proposalGraph": with_desc(string(), "In `claims` mode, the evidence proposal graph when the settings name several."),
        "allowPartial": with_desc(boolean(), "Register what can be read of a PDF that needs OCR, and record the other pages."),
        "extract": with_desc(boolean(), "Extract facts with the `extract` role (default: when the dataset lets ingestion use a provider)."),
        "confirm": with_desc(boolean(), "Confirm the estimate in advance."),
        "base": with_desc(string(), "The namespace of a table's rows in its mapping draft."),
        "message": with_desc(string(), "The commit message of the registration."),
        "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
    });
    let mut json_body = options.clone();
    json_body["text"] = with_desc(string(), "The document's text.");
    json_body["source"] = with_desc(
        string(),
        "The graph IRI of a registered source to extract from instead of a document: its current rendition goes through the estimate, the `extract` role and linking. Leave out text, url and the file.",
    );
    put(
        "IngestRequest",
        doc(
            closed(&[], json_body),
            "An ingestion from text or a URL. A multipart request sends the document as its `file` part and these options as fields.",
            "ingestion",
        ),
    );
    let mut form = options;
    form["file"] = json!({ "type": "string", "contentMediaType": "application/octet-stream" });
    put(
        "IngestForm",
        doc(
            closed(&["file"], form),
            "An ingestion of an uploaded file, with the options of `IngestRequest` as fields.",
            "ingestion",
        ),
    );
    put(
        "IngestTask",
        doc(
            obj(
                &[
                    "id",
                    "dataset",
                    "status",
                    "progress",
                    "createdAt",
                    "updatedAt",
                    "input",
                    "usage",
                ],
                json!({
                    "id": string(),
                    "dataset": string(),
                    "status": string_enum(&["queued", "scanning", "converting", "registering", "awaiting-confirmation", "extracting", "linking", "writing", "awaiting-approval", "done", "failed", "cancelled"]),
                    "progress": with_desc(num(), "From 0 to 1."),
                    "message": string(),
                    "createdAt": string(),
                    "updatedAt": string(),
                    "finishedAt": string(),
                    "input": any_object("What was ingested: its name, format, size, URL or source and mode. A maintenance task has `kind` `consolidation` or `retention`, and `scheduled`."),
                    "estimate": any_object("The estimate of the extraction: chunks, tokens, the first pair, its estimated cost, the threshold and whether it needs a confirmation."),
                    "usage": any_object("Model calls, tokens, estimated cost, escalations and the pair that answered each chunk. A provider that used its prompt cache adds `cacheWriteInputTokens`, `cacheWrite1hInputTokens`, `cacheReadInputTokens` and `budgetTokens`."),
                    "result": any_object("The outcome (`registered`, `proposed`, `no-facts`, `preview`, `merged`, `approved`, `already-registered`, `already-proposed`, `mapping-draft` or `mapping-drafts`), with the source, rendition, branch, pages, proposals, a table's mapping draft and preview, or a workbook's drafts for each sheet. A `claims` ingestion reports the evidence operation, commit, first availability, revision, records and their counts by kind, the claims, failed facts, flagged entities and `factGuard` `not-run`. A consolidation's outcome is `proposed`, `merged`, `dry-run`, `nothing-to-consolidate`, `no-facts` or `pending-review`, with the branch, the repeated facts, the duplicates and the conflicts. A retention's is `deleted`, `dry-run` or `nothing-to-delete`, with each session graph and why it is kept or deleted."),
                    "error": any_object("The code and message of a failed task, such as `needs-ocr` with the pages that need OCR and their reasons."),
                }),
            ),
            "An ingestion task with its progress, estimate, usage and result.",
            "ingestion",
        ),
    );
    put(
        "IngestTaskList",
        doc(
            obj(
                &["dataset", "tasks", "capabilities"],
                json!({
                    "dataset": string(),
                    "tasks": array(sref("IngestTask")),
                    "capabilities": obj(&["pdf", "ocr", "office"], json!({ "pdf": boolean(), "ocr": boolean(), "office": boolean() })),
                }),
            ),
            "The caller's ingestion tasks, newest first, and whether this server converts PDFs and Office documents and reads scanned pages.",
            "ingestion",
        ),
    );
}

fn asking(put: &mut dyn FnMut(&str, J)) {
    let pair = obj(
        &["provider", "model"],
        json!({ "provider": string(), "model": string() }),
    );
    let send = string_enum(&["schema", "rows", "documents"]);
    let turn = closed(
        &["question", "query"],
        json!({ "question": string(), "query": string() }),
    );
    let choice = closed(&["value"], json!({ "id": string(), "value": string() }));
    let about = closed(
        &["iri"],
        json!({
            "iri": with_desc(string(), "An absolute IRI of at most 2000 characters."),
            "kind": with_desc(string(), "What the resource is, such as `class`, `property` or `concept`, at most 32 characters."),
            "label": with_desc(string(), "Its label, at most 200 characters."),
        }),
    );
    put(
        "AskRequest",
        doc(
            closed(
                &["question"],
                json!({
                    "question": with_desc(string(), "At most 2000 characters."),
                    "context": with_desc(array(turn), "Earlier turns of the conversation, at most 5."),
                    "clarification": {
                        "description": "The chosen value of a `clarify` event, as `{id, value}` or as the value alone.",
                        "oneOf": [choice, string()],
                    },
                    "at": { "description": "A commit or time to read, as for `/{ds}/sparql`." },
                    "branch": string(),
                    "reasoning": boolean(),
                    "run": with_desc(boolean(), "Whether to run the checked query. `true` by default."),
                    "summary": with_desc(boolean(), "Whether to summarize the rows. `true` by default when the dataset sends rows."),
                    "maxRows": with_desc(int(), "The rows returned in `result`, 1000 by default."),
                    "tryHarder": with_desc(string(), "The id of an earlier ask of the caller. Drafting starts at the pair after the one that drafted it."),
                    "reviewedOnly": with_desc(boolean(), "Hide the agent memory graphs of the dataset from the query, and use only reviewed community reports."),
                    "query": with_desc(string(), "A query to check, run and summarize without drafting, for **Summarize again** after an edit."),
                    "mode": with_desc(string_enum(&["auto", "local", "global"]), "Answer from the data (`local`), from the community reports (`global`), or as the draft classifies the question (`auto`, the default)."),
                    "explore": with_desc(boolean(), "Explore the data once after a draft without a query or with an empty result. Over the dataset's `repairs.explore`."),
                    "shapeCheck": with_desc(boolean(), "Repair once when the result does not fit the question's form. Over the dataset's `repairs.shapeCheck`."),
                    "drafts": with_desc(int(), "First drafts whose results vote on the query, 1 to 5. Over the dataset's `repairs.drafts`."),
                    "typeHints": with_desc(boolean(), "Link mentions with the classes the words around them name. Over the dataset's `repairs.typeHints`."),
                    "thread": with_desc(string(), "The thread a follow-up continues: the `threadId` of an earlier ask, 1 to 64 letters, digits, `_` or `-`. The first ask of a thread names none and its own id becomes the thread's."),
                    "about": with_desc(about.clone(), "The resource on the page the question was asked from. The pipeline reads the question with `(about <iri>)` after it, and the history keeps the question as written with `about` beside it."),
                }),
            ),
            "A question for the dataset.",
            "asking-in-the-server",
        ),
    );
    put(
        "AskResult",
        doc(
            obj(
                &["dataset", "question", "outcome", "usage"],
                json!({
                    "dataset": string(),
                    "question": string(),
                    "outcome": string_enum(&["answered", "empty", "checked", "not-run", "clarify", "unanswerable", "failed", "error"]),
                    "result": any_object("The checked query with `explanation`, `assumptions`, `terms`, `graph`, `commit`, `attempt`, `verdict` and `results` in the `application/x-sparkles+json` form."),
                    "mode": with_desc(string_enum(&["local", "global"]), "Whether the answer came from the data or from community reports."),
                    "summary": obj(&["text", "citations"], json!({ "text": string(), "citations": array(int()), "rowsSent": int(), "provider": string(), "model": string(), "uncited": boolean(), "uncitedSentences": strings() })),
                    "points": with_desc(array(obj(&["n", "text", "score", "reports", "findings"], json!({ "n": int(), "text": string(), "score": int(), "reports": strings(), "findings": strings() }))), "In a global answer, the points the reduce step read. The summary's citations number them."),
                    "reports": with_desc(array(any_object("A report with `iri`, `title`, `level`, `status`, `rating` and the `findings` the answer uses, each with its `cites`.")), "In a global answer, the reports it used."),
                    "global": any_object("In a global answer, the `level`, `ranking`, `reportsGraph`, the `selected` reports and the map `batches`."),
                    "clarify": any_object("`{id, question, choices: [{label, value}]}`."),
                    "error": obj(&["code", "message"], json!({ "code": string(), "message": string() })),
                    "attempts": array(any_object("One draft with its check and run.")),
                    "notes": strings(),
                    "usage": any_object("`askId`, `threadId`, the tokens (with the prompt cache's `cacheWriteInputTokens`, `cacheWrite1hInputTokens`, `cacheReadInputTokens` and `budgetTokens` when the provider used it) and estimated cost, `complexity`, the `steps` with their pairs and signals, `escalations`, `draftPair`, `answeredBy` and `tryHarder`."),
                    "threadId": with_desc(string(), "The ask's thread: the request's `thread`, or the ask's own id."),
                }),
            ),
            "The whole answer of an ask in the JSON form.",
            "asking-in-the-server",
        ),
    );
    let mut roles = Map::new();
    for r in [
        "draft",
        "repair",
        "summarize",
        "extract",
        "explain",
        "optimize",
    ] {
        roles.insert(r.into(), array(pair.clone()));
    }
    put(
        "AssistantSettings",
        doc(
            obj(
                &[],
                json!({
                    "enabled": with_desc(boolean(), "Whether the dataset has an assistant. `false` by default."),
                    "roles": { "type": "object", "description": "Role lists that replace the server's for this dataset.", "properties": J::Object(roles) },
                    "ask": with_desc(boolean(), "Whether `POST /{ds}/ask` is enabled. `true` by default."),
                    "explain": boolean(),
                    "optimize": boolean(),
                    "ingest": boolean(),
                    "send": with_desc(send.clone(), "What may leave the server. `schema` by default."),
                    "sendByProvider": { "type": "object", "description": "A lower `send` level for some providers.", "additionalProperties": send },
                    "rowsForSummary": with_desc(int(), "The rows sent to the summary, 50 by default."),
                    "budget": closed(&[], json!({ "perRequest": int(), "perPrincipalPerDay": int(), "perDatasetPerDay": int() })),
                    "deadlineSecs": with_desc(num(), "The time an ask may take, 120 seconds by default."),
                    "historyDays": with_desc(int(), "The days an asked question is kept. 0 keeps nothing."),
                    "routing": any_object("`complexityThreshold` and `exampleScore` of the escalation rules."),
                    "repairs": with_desc(closed(&[], json!({
                        "explore": with_desc(boolean(), "Explore the data once after a draft without a query or with an empty result. `true` by default."),
                        "shapeCheck": with_desc(boolean(), "Repair once when the result does not fit the question's form. `false` by default."),
                        "drafts": with_desc(int(), "First drafts whose results vote on the query, 1 to 5. 1 by default."),
                        "typeHints": with_desc(boolean(), "Link mentions with the classes the words around them name. `false` by default."),
                    })), "The repair turns of the dataset's asks, which a request may override."),
                    "status": with_desc(obj(&["models", "historyDays", "ask"], json!({
                        "models": boolean(),
                        "historyDays": int(),
                        "ask": boolean(),
                        "reason": string(),
                        "draft": array(pair.clone()),
                        "summary": with_desc(boolean(), "Whether an answer can carry a summary."),
                    })), "Answered by the server and ignored in a `PUT`."),
                }),
            ),
            "The assistant settings of a dataset.",
            "assistant-settings",
        ),
    );
    let stored = obj(
        &[],
        json!({
            "explanation": string(),
            "assumptions": strings(),
            "summary": obj(&["text", "citations"], json!({ "text": string(), "citations": array(int()), "rowsSent": int(), "provider": string(), "model": string(), "uncited": boolean() })),
            "global": obj(&["points"], json!({ "points": array(obj(&["n", "text"], json!({ "n": int(), "text": string(), "reports": strings() }))), "reports": array(obj(&["iri"], json!({ "iri": string(), "title": string() }))), "uncitedSentences": strings() })),
            "graph": obj(&[], json!({ "subject": string(), "predicate": string(), "object": string() })),
            "verdict": string_enum(&["query", "data", "unknown"]),
            "diagnosis": with_desc(string(), "Why the result was empty."),
            "notes": strings(),
            "clarify": obj(&["question", "choices"], json!({ "question": string(), "choices": array(obj(&["label", "value"], json!({ "label": string(), "value": string() }))) })),
            "error": obj(&[], json!({ "code": string(), "message": string() })),
            "entities": with_desc(strings(), "The entities the question linked exactly."),
            "communities": with_desc(strings(), "The community reports the ask read for context."),
        }),
    );
    put(
        "AskHistory",
        doc(
            obj(
                &["dataset", "historyDays", "asks"],
                json!({
                    "dataset": string(),
                    "historyDays": int(),
                    "asks": array(obj(&["id", "principal", "at", "question", "result", "outcome", "routing"], json!({
                        "id": string(),
                        "principal": string(),
                        "at": { "type": "string", "format": "date-time" },
                        "question": string(),
                        "query": string(),
                        "commit": int(),
                        "result": with_desc(string(), "The pipeline's outcome."),
                        "outcome": string_enum(&["none", "accepted", "edited", "rejected"]),
                        "note": string(),
                        "routing": any_object("The complexity, steps, escalations, answering pair and tokens."),
                        "mode": string_enum(&["local", "global"]),
                        "reports": with_desc(strings(), "The reports a global answer used, listed only while their graph is proven for the caller."),
                        "reportsGraph": string(),
                        "thread": with_desc(string(), "The ask's thread. A record kept before threads has its own id."),
                        "about": about,
                        "answer": with_desc(stored, "The answer as it was given, from format 2 of the history: never its rows. Present only while the caller's view covers every graph the ask read under the same protections."),
                        "answerWithheld": with_desc(boolean(), "The answer is kept, but the caller's view no longer covers what it read."),
                        "branch": with_desc(string(), "The branch the ask read, when it was not `main`."),
                    }))),
                }),
            ),
            "The caller's asked questions, newest first, or one thread's oldest first.",
            "ask-history",
        ),
    );
    put(
        "AskFeedback",
        doc(
            closed(
                &["outcome"],
                json!({
                    "outcome": string_enum(&["accepted", "edited", "rejected"]),
                    "note": with_desc(string(), "At most 1000 characters."),
                }),
            ),
            "Feedback on an answer.",
            "ask-history",
        ),
    );
    put(
        "ModelUsage",
        doc(
            obj(
                &["days", "datasets"],
                json!({
                    "days": int(),
                    "datasets": array(obj(&["dataset", "asks"], json!({
                        "dataset": string(),
                        "asks": int(),
                        "answers": array(any_object("`{role, provider, model, outcome, count}`.")),
                        "escalations": array(any_object("`{role, signal, count}`.")),
                        "outcomes": array(any_object("`{bucket, outcome, count}`.")),
                        "tokens": array(any_object("`{provider, model, inputTokens, outputTokens, estimatedCost}`.")),
                    }))),
                }),
            ),
            "The routing counters by dataset.",
            "asking-in-the-server",
        ),
    );
}

/// The members every tool body may hold to pick a state.
fn read_args() -> Vec<(&'static str, J)> {
    vec![
        (
            "reasoning",
            with_desc(
                boolean(),
                "Include materialized inferences (default: true when the dataset has them).",
            ),
        ),
        (
            "timeoutSeconds",
            with_desc(num(), "At most the server's query timeout. 30 by default."),
        ),
        (
            "atCommit",
            with_desc(int(), "Read the snapshot of this commit."),
        ),
        (
            "at",
            json!({ "type": ["integer", "string"], "description": "A commit number, `commit:N`, `time:<RFC 3339>`, `snapshot:<name>` or `head`." }),
        ),
    ]
}

fn with_read_args(mut props: J) -> J {
    for (k, v) in read_args() {
        props[k] = v;
    }
    props
}

fn tools(put: &mut dyn FnMut(&str, J)) {
    let issue = obj(
        &["code", "severity", "message"],
        json!({
            "code": string(),
            "severity": string_enum(&["error", "warning"]),
            "message": string(),
            "term": string(),
            "line": int(),
            "column": int(),
            "suggestions": array(obj(&["term", "count", "why"], json!({
                "term": string(), "label": string(), "count": int(), "why": string(),
            }))),
        }),
    );
    put(
        "CheckRequest",
        doc(
            closed(
                &["query"],
                with_read_args(json!({
                    "query": with_desc(string(), "A SPARQL query. The dataset's prefixes are predeclared."),
                    "explain": with_desc(boolean(), "Add the plan's estimated rows and its warnings."),
                    "maxSuggestions": with_desc(int(), "Suggestions per issue, 0 to 10. 3 by default."),
                    "terms": with_desc(boolean(), "List every constant IRI of the query."),
                })),
            ),
            "The arguments of `check_query` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "CheckResult",
        doc(
            obj(
                &["dataset", "commit", "ok", "issues", "prefixes"],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "ok": with_desc(boolean(), "False when an issue is an error."),
                    "issues": array(issue.clone()),
                    "estimatedRows": num(),
                    "terms": array(obj(&["term", "iri", "kind", "occurs"], json!({
                        "term": with_desc(string(), "The IRI, compacted with `prefixes`."),
                        "iri": string(),
                        "kind": string_enum(&["class", "property", "entity"]),
                        "label": string(),
                        "count": with_desc(int(), "Instances of a class, or triples of a property, in the caller's view."),
                        "types": strings(),
                        "occurs": json!({ "type": ["boolean", "null"], "description": "Whether the term occurs in the caller's view. Null when the check did not look it up." }),
                    }))),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The issues of a query, and with `terms` the terms it uses.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "DiagnoseRequest",
        doc(
            closed(
                &["query"],
                with_read_args(json!({
                    "query": with_desc(string(), "The query that returned no rows."),
                })),
            ),
            "The arguments of `why_empty` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    let step = obj(
        &["kind", "text", "solutions"],
        json!({
            "kind": string_enum(&["pattern", "join", "filter"]),
            "text": string(),
            "solutions": json!({ "type": ["boolean", "null"], "description": "Null when the check ran out of time." }),
        }),
    );
    put(
        "Diagnosis",
        doc(
            obj(
                &[
                    "dataset", "commit", "empty", "steps", "complete", "message", "prefixes",
                ],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "empty": json!({ "type": ["boolean", "null"], "description": "Whether the query has no solutions. Null when the check ran out of time." }),
                    "first": obj(&["kind", "text", "constants", "issues"], json!({
                        "kind": string_enum(&["pattern", "join", "filter"]),
                        "text": string(),
                        "line": int(),
                        "column": int(),
                        "constants": array(obj(&["term", "occurs"], json!({ "term": string(), "occurs": boolean() }))),
                        "issues": array(issue),
                    })),
                    "steps": array(step),
                    "verdict": with_desc(string_enum(&["query", "data", "unknown"]), "For an empty result: `query` when a check issue explains the first element without solutions, `data` when the query is well formed for the data and the data holds no match, `unknown` otherwise."),
                    "unchecked": with_desc(strings(), "Parts of the query the diagnosis does not cut, such as MINUS."),
                    "complete": with_desc(boolean(), "Whether every check finished and every part was checked."),
                    "message": string(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "Why a query has no solutions: the first pattern, join or filter without any.",
            "checking-and-explaining-queries",
        ),
    );
    put(
        "RecallRequest",
        doc(
            closed(
                &[],
                with_read_args(json!({
                    "query": with_desc(string(), "The question or the words to search for. Give query, seeds or both."),
                    "seeds": with_desc(strings(), "Entity IRIs to start from."),
                    "types": with_desc(strings(), "Class IRIs that seeds found by search must have."),
                    "graphs": with_desc(strings(), "Graph IRIs to read, or `default`."),
                    "hops": int(),
                    "seedLimit": int(),
                    "maxTriples": int(),
                    "maxBytes": int(),
                    "includeSuperseded": boolean(),
                    "statuses": array(string_enum(&["reviewed", "unreviewed", "proposed"])),
                    "unreviewedWeight": num(),
                    "recency": with_desc(string(), "A half-life such as `90d` (units s, m, h, d, w, y). Found seeds rank by the age of their newest fact and by how many graphs assert their facts."),
                    "communities": with_desc(boolean(), "Add the GraphRAG community report of each seed's community."),
                    "communityLevel": with_desc(int(), "The level of those communities, 0 being the coarsest. The finest by default."),
                    "format": with_desc(string_enum(&["json"]), "Always `json` here."),
                })),
            ),
            "The arguments of `recall` without `dataset`.",
            "checking-and-explaining-queries",
        ),
    );
    let status = string_enum(&["reviewed", "unreviewed"]);
    put(
        "RecallResult",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "entities",
                    "citations",
                    "conflicts",
                    "truncated",
                    "prefixes",
                ],
                json!({
                    "dataset": string(),
                    "commit": int(),
                    "entities": array(obj(&["iri", "types", "hop", "facts"], json!({
                        "iri": string(),
                        "label": string(),
                        "types": strings(),
                        "seed": int(),
                        "hop": int(),
                        "facts": array(obj(&["s", "p", "o", "citation"], json!({
                            "s": string(), "p": string(), "o": string(), "citation": int(),
                            "status": status.clone(),
                        }))),
                    }))),
                    "citations": array(obj(&["id", "graph"], json!({
                        "id": int(), "graph": string(), "reifier": string(), "source": string(),
                        "at": string(), "by": string(), "confidence": string(), "quote": string(),
                        "status": status,
                    }))),
                    "superseded": array(obj(&["s", "p", "o", "graph", "reifier"], json!({
                        "s": string(), "p": string(), "o": string(), "graph": string(), "reifier": string(),
                        "at": string(), "invalidatedAt": string(), "replacedBy": strings(),
                    }))),
                    "conflicts": array(obj(&["s", "p", "values"], json!({
                        "s": string(), "p": string(),
                        "values": array(obj(&["o", "citation"], json!({ "o": string(), "citation": int() }))),
                    }))),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                    "communities": with_desc(obj(&["reports"], json!({
                        "level": int(),
                        "unavailable": with_desc(string_enum(&["off", "access"]), "Why no report is listed."),
                        "reports": array(obj(&["iri", "community", "title", "summary", "status", "memberCount", "seeds"], json!({
                            "iri": string(), "community": string(), "title": string(), "summary": string(),
                            "status": string(), "memberCount": int(), "seeds": array(int()),
                        }))),
                    })), "With `communities`, the report of each seed's community."),
                }),
            ),
            "The facts around the seeds, with a citation for each.",
            "checking-and-explaining-queries",
        ),
    );
}

/// The maintenance tasks of agent memory (C18 Phase 5).
fn maintenance(put: &mut dyn FnMut(&str, J)) {
    put(
        "ConsolidateRequest",
        doc(
            closed(
                &[],
                json!({
                    "mode": with_desc(string_enum(&["branch", "auto"]), "`branch` (the default, or the dataset's `consolidation.mode`) leaves the proposals on a review branch. `auto` merges them when every fact passes and needs `admin`."),
                    "minSources": with_desc(int(), "The distinct sources that must assert a fact, 2 to 100 (default: the dataset's, else 2)."),
                    "dryRun": with_desc(boolean(), "Report the repeated facts, duplicates and conflicts, and write nothing."),
                    "message": with_desc(string(), "The commit message of the proposals."),
                    "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
                }),
            ),
            "A consolidation pass. An empty body uses the dataset's settings.",
            "memory-maintenance",
        ),
    );
    put(
        "RetentionRequest",
        doc(
            closed(
                &[],
                json!({
                    "after": with_desc(string(), "The age after which a session graph is deleted, such as `365d` (default: the dataset's `retention.after`)."),
                    "graphs": with_desc(strings(), "IRI patterns of the session graphs (default: the dataset's)."),
                    "requireConsolidated": with_desc(boolean(), "Delete only graphs whose facts a reviewed graph asserts too (default: the dataset's, else true)."),
                    "dryRun": with_desc(boolean(), "List what would be deleted, and delete nothing."),
                    "deadlineSeconds": with_desc(num(), "The task's deadline, 1 to 86400 seconds (3600 by default)."),
                }),
            ),
            "A retention pass. An empty body applies the dataset's `retention`.",
            "memory-maintenance",
        ),
    );
    let entry = obj(
        &["settings"],
        json!({
            "settings": any_object("The dataset's `consolidation` or `retention` member, or null."),
            "lastRun": with_desc(string(), "When the last scheduled task started."),
            "lastTask": with_desc(string(), "Its id, readable at `/$/ingest/{ds}/{task}`."),
            "nextRun": with_desc(string(), "When the next scheduled task starts, or `due`. Absent without a schedule."),
        }),
    );
    let kind = obj(
        &["open"],
        json!({
            "open": int(),
            "oldest": with_desc(string(), "When the oldest open item of the kind arrived."),
        }),
    );
    put(
        "ReviewCounts",
        doc(
            obj(
                &["open", "kinds", "branches", "truncated", "updated"],
                json!({
                    "open": with_desc(int(), "Open items: unreviewed facts, and open review branches counted once each."),
                    "oldest": with_desc(string(), "When the oldest open item arrived. Absent when nothing is open."),
                    "kinds": {
                        "type": "object",
                        "description": "The kinds with open items: `session`, `import`, `consolidation`, `ingest`, `review`, `inbox` or `proposal`.",
                        "additionalProperties": kind,
                    },
                    "branches": array(obj(
                        &["name", "kind", "created"],
                        json!({
                            "name": string(),
                            "kind": string(),
                            "created": string(),
                            "facts": with_desc(int(), "The facts the branch proposes, for the 10 newest branches."),
                        }),
                    )),
                    "truncated": with_desc(boolean(), "More than 5,000 unreviewed facts wait, and the count stopped there."),
                    "updated": with_desc(string(), "When the server last counted."),
                }),
            ),
            "The open items of a dataset's memory review inbox, as the server last counted them.",
            "memory-maintenance",
        ),
    );
    put(
        "MaintenanceStatus",
        doc(
            obj(
                &["dataset", "consolidation", "retention"],
                json!({
                    "dataset": string(),
                    "consolidation": entry.clone(),
                    "retention": entry,
                    "review": sref("ReviewCounts"),
                }),
            ),
            "The schedules of consolidation and retention, and for a caller with `write` on a dataset with memory settings, the open items of its review inbox.",
            "memory-maintenance",
        ),
    );
}

fn memory(put: &mut dyn FnMut(&str, J)) {
    settings(put);
    server_settings(put);
    put(
        "MemorySettings",
        doc(
            closed(
                &[],
                json!({
                    "agentGraphs": with_desc(strings(), "Graph IRIs or `*` patterns of agent memory. Facts asserted only there are unreviewed."),
                    "consolidatedGraph": with_desc(string(), "The graph that promoted facts are written to."),
                    "agents": { "type": "object", "description": "Per-agent policies by agent name.", "additionalProperties": closed(&[], json!({
                        "conversationFacts": string_enum(&["immediate", "review"]),
                    })) },
                    "imports": with_desc(closed(&["base"], json!({
                        "base": with_desc(string(), "The prefix of every import graph, an IRI that ends in `/` or `#`. `agentGraphs` must cover it."),
                        "secretPatterns": array(closed(&["name", "regex"], json!({ "name": string(), "regex": string() }))),
                        "transcripts": with_desc(boolean(), "Whether transcripts may be imported (Phase 3m-b)."),
                        "extract": with_desc(string_enum(&["agent", "server", "none"]), "Who extracts facts from imported prose."),
                    })), "The imports of coding agents' memory files."),
                    "consolidation": with_desc(closed(&[], json!({
                        "every": with_desc(string(), "How often the server runs a pass, such as `1d` (at least `1h`). Without it a pass runs only on request."),
                        "mode": string_enum(&["branch", "auto"]),
                        "minSources": with_desc(int(), "The distinct sources that must assert a fact, 2 to 100, 2 by default."),
                    })), "The consolidation pass. Needs `agentGraphs` and `consolidatedGraph`."),
                    "retention": with_desc(closed(&["after"], json!({
                        "after": with_desc(string(), "The age of a session graph's newest fact after which the graph is deleted, such as `365d` (at least `1d`)."),
                        "graphs": with_desc(strings(), "IRI patterns with `*` of the session graphs. By default, the agent graphs whose IRI holds `/sessions/`."),
                        "requireConsolidated": with_desc(boolean(), "Delete only graphs whose facts a reviewed graph asserts too (true by default)."),
                        "every": with_desc(string(), "How often the server applies it, `1d` by default."),
                    })), "The retention of session graphs, off without it."),
                    "scratchBranches": with_desc(closed(&[], json!({
                        "ttl": with_desc(string(), "How long a scratch branch may sit idle before the server deletes it, from `1m` to `3650d`, such as `24h`. `off` keeps the dataset's scratch branches even when the server's `--mcp-scratch-branch-ttl` sets a duration. Without it, the server flag decides. A dataset with this member needs a release that reads `memory.json` format 2."),
                    })), "When idle scratch branches expire. Written as `format` 2 of `memory.json`."),
                }),
            ),
            "The memory settings of a dataset.",
            "memory-settings",
        ),
    );
    let suggestion = obj(
        &["id", "question", "query", "by", "at"],
        json!({
            "id": string(),
            "question": string(),
            "query": string(),
            "explanation": string(),
            "by": with_desc(string(), "The principal who suggested it."),
            "at": { "type": "string", "format": "date-time" },
        }),
    );
    put(
        "Suggestion",
        doc(
            suggestion.clone(),
            "A suggested example.",
            "suggested-examples",
        ),
    );
    put(
        "SuggestionList",
        doc(
            obj(
                &["dataset", "suggestions"],
                json!({ "dataset": string(), "suggestions": array(suggestion) }),
            ),
            "The suggested examples, newest first.",
            "suggested-examples",
        ),
    );
    put(
        "SuggestRequest",
        doc(
            closed(
                &["question", "query"],
                json!({
                    "question": with_desc(string(), "At most 2000 characters."),
                    "query": with_desc(string(), "A SPARQL query of at most 65,536 characters."),
                    "explanation": with_desc(string(), "At most 400 characters."),
                }),
            ),
            "A question and the query that answers it.",
            "suggested-examples",
        ),
    );
    imports(put);
    review(put);
}

/// The review inbox, branch review, reviewer actions and ingest profiles (C18 Phase 3).
fn review(put: &mut dyn FnMut(&str, J)) {
    let fact_ref = closed(
        &["s", "p", "o", "graph"],
        json!({
            "s": string(), "p": string(),
            "o": with_desc(string(), "The object in N-Triples form, as the inbox lists it."),
            "graph": string(),
        }),
    );
    let signal = string_enum(&["pass", "fail", "none", "unchecked"]);
    let fact = obj(
        &["s", "p", "o", "graph", "status", "reifiers"],
        json!({
            "s": string(), "p": string(), "o": string(), "graph": string(),
            "shown": any_object("The terms in compact form."),
            "sLabel": string(), "oLabel": string(),
            "status": string_enum(&["unreviewed", "proposed", "reviewed"]),
            "reifiers": strings(),
            "time": string(), "confidence": string(), "quote": string(),
            "by": string(), "agent": string(),
            "span": obj(&["rendition", "start", "end"], json!({ "rendition": string(), "start": int(), "end": int() })),
            "signals": obj(&["span", "link", "guard", "corroboration"], json!({
                "span": signal.clone(), "link": signal.clone(), "guard": signal.clone(), "corroboration": signal,
            })),
            "passes": with_desc(boolean(), "Whether the span, link and guard signals pass."),
            "candidates": array(obj(&["iri"], json!({ "iri": string(), "shown": string(), "label": string() }))),
            "notes": strings(),
        }),
    );
    let branch = obj(
        &["name", "kind", "ahead", "behind"],
        json!({
            "name": string(),
            "kind": string_enum(&["ingest", "review", "inbox", "consolidation", "proposal"]),
            "ahead": int(), "behind": int(),
            "created": string(), "modified": string(), "note": string(), "creator": string(),
            "facts": with_desc(int(), "The facts the branch proposes."),
            "retracts": with_desc(int(), "The facts of main the branch retracts."),
        }),
    );
    put(
        "MemoryInbox",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "sessions",
                    "branches",
                    "open",
                    "truncated",
                ],
                json!({
                    "dataset": string(), "commit": int(),
                    "agentGraphs": strings(),
                    "target": with_desc(string(), "The consolidated graph that promotions write to by default."),
                    "sessions": array(obj(&["graph", "facts"], json!({
                        "graph": string(), "shown": string(), "by": strings(),
                        "first": string(), "last": string(),
                        "facts": array(fact.clone()),
                    }))),
                    "branches": array(branch),
                    "open": with_desc(int(), "Unreviewed facts plus open review branches."),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                    "communities": sref("GraphragInbox"),
                }),
            ),
            "Everything that waits for a person: unreviewed session facts with their signals, by session, and the open review branches.",
            "review-inbox",
        ),
    );
    put(
        "BranchReview",
        doc(
            obj(
                &[
                    "dataset", "branch", "facts", "retracts", "entities", "sources",
                ],
                json!({
                    "dataset": string(), "branch": string(), "kind": string(),
                    "base": int(), "head": int(), "ahead": int(), "behind": int(),
                    "note": string(), "creator": string(),
                    "facts": array(fact.clone()),
                    "retracts": array(fact),
                    "rejected": with_desc(int(), "Facts made and retracted on the branch."),
                    "entities": array(obj(&["iri", "types", "candidates"], json!({
                        "iri": string(), "shown": string(), "label": string(), "types": strings(),
                        "candidates": array(obj(&["iri"], json!({ "iri": string(), "shown": string(), "label": string() }))),
                    }))),
                    "sources": array(obj(&["rendition", "length"], json!({
                        "rendition": string(), "source": string(), "title": string(), "format": string(),
                        "length": int(), "text": string(), "textOmitted": boolean(),
                        "pages": array(obj(&["page", "start"], json!({ "page": int(), "start": int() }))),
                        "ocrPages": array(int()),
                        "omittedPages": array(int()),
                    }))),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What a review branch proposes and retracts, its new entities and the text of the sources it cites.",
            "review-inbox",
        ),
    );
    put(
        "PromoteRequest",
        doc(
            closed(
                &["facts"],
                json!({
                    "facts": array(fact_ref.clone()),
                    "target": with_desc(string(), "The graph to promote into (default: `consolidatedGraph`)."),
                    "branch": with_desc(string(), "An open review branch to add the facts to (default: a new `review.{person}.{date}-{n}`)."),
                    "message": string(),
                    "timeoutSeconds": num(),
                }),
            ),
            "The facts to promote, at most 500.",
            "review-inbox",
        ),
    );
    put(
        "PromoteResult",
        doc(
            obj(
                &["dataset", "branch", "target", "promoted", "committed"],
                json!({
                    "dataset": string(), "branch": string(), "target": string(),
                    "promoted": int(), "commit": int(), "committed": boolean(),
                }),
            ),
            "The review branch that holds the promoted facts, for the merge page.",
            "review-inbox",
        ),
    );
    put(
        "RejectRequest",
        doc(
            closed(
                &["facts"],
                json!({
                    "facts": array(fact_ref.clone()),
                    "branch": with_desc(string(), "The review branch the facts are on (default: main)."),
                    "reason": with_desc(string(), "Added to the commit message, at most 500 characters."),
                    "timeoutSeconds": num(),
                }),
            ),
            "The facts to retract, at most 500.",
            "review-inbox",
        ),
    );
    put(
        "RejectResult",
        doc(
            obj(
                &["dataset", "branch", "rejected", "commits"],
                json!({
                    "dataset": string(), "branch": string(), "rejected": int(),
                    "commits": array(any_object("The result of `assert_facts` for one graph.")),
                }),
            ),
            "The retractions, one commit per graph.",
            "review-inbox",
        ),
    );
    put(
        "RelinkRequest",
        doc(
            closed(
                &["branch", "from", "to"],
                json!({
                    "branch": string(),
                    "from": with_desc(string(), "The new entity's IRI."),
                    "to": with_desc(string(), "The existing entity's IRI."),
                    "timeoutSeconds": num(),
                }),
            ),
            "**Use existing**: name an existing entity instead of a new one on a review branch.",
            "review-inbox",
        ),
    );
    put(
        "EditFactRequest",
        doc(
            closed(
                &["fact", "o"],
                json!({
                    "fact": fact_ref,
                    "o": with_desc(string(), "The new object, as `assert_facts` takes it."),
                    "branch": with_desc(string(), "The review branch the fact is on (default: main)."),
                    "timeoutSeconds": num(),
                }),
            ),
            "**Edit value**: replace a fact's object, with the new reifier derived from the old one.",
            "review-inbox",
        ),
    );
    put(
        "MemoryWriteResult",
        doc(
            obj(
                &["dataset"],
                json!({
                    "dataset": string(), "branch": string(), "committed": boolean(),
                    "commit": int(), "inserted": int(), "deleted": int(),
                }),
            ),
            "The commit of a reviewer's change.",
            "review-inbox",
        ),
    );
    let profile = closed(
        &[],
        json!({
            "classes": with_desc(strings(), "The classes new entities may have (default: the classes with instances or a declaration)."),
            "predicates": with_desc(strings(), "The predicates facts may use (default: those with triples or a declaration)."),
            "shapes": with_desc(string(), "Extra SHACL shapes in Turtle."),
            "labelPredicate": string(),
            "language": with_desc(string(), "A BCP 47 language tag for new labels."),
            "vocabulary": with_desc(string(), "A graph whose classes and properties are offered."),
        }),
    );
    put(
        "IngestProfile",
        doc(profile.clone(), "An ingest profile.", "ingest-profiles"),
    );
    put(
        "IngestProfiles",
        doc(
            obj(
                &["dataset", "keepText", "profiles"],
                json!({
                    "dataset": string(),
                    "keepText": with_desc(boolean(), "Whether sources keep their text as chunks."),
                    "profiles": { "type": "object", "additionalProperties": profile },
                }),
            ),
            "The ingest settings of a dataset.",
            "ingest-profiles",
        ),
    );
    put(
        "IngestSettingsRequest",
        doc(
            closed(
                &[],
                json!({
                    "keepText": boolean(),
                    "confirmTokens": with_desc(or_null(int()), "The estimate in tokens above which an ingestion waits for a confirmation (200000 by default)."),
                    "autoConfidence": with_desc(or_null(num()), "The confidence that `auto` mode needs of every fact (0.8 by default)."),
                }),
            ),
            "Whether sources keep their text, and the thresholds of ingestion. Members left out keep their value, and `null` restores a default.",
            "ingest-profiles",
        ),
    );
}

/// `POST /{ds}/facts` and `POST /{ds}/memory/brief` (C18 Phase 3m-a).
fn imports(put: &mut dyn FnMut(&str, J)) {
    let fact = closed(
        &["s", "p", "o"],
        json!({
            "s": string(), "p": string(),
            "o": with_desc(string(), "An IRI, a key of `entities`, or a literal in SPARQL syntax."),
            "mode": string_enum(&["add", "replace"]),
            "confidence": num(),
            "quote": with_desc(string(), "At most 1000 characters."),
            "span": with_desc(closed(&["rendition", "start", "end"], json!({
                "rendition": string(), "start": int(), "end": int(),
            })), "The passage of a registered source that supports the fact, in code points of its rendition."),
            "derivedFrom": with_desc(strings(), "Reifiers of existing facts this fact rests on, at most 20."),
        }),
    );
    let retract = json!({ "oneOf": [
        with_desc(string(), "A reifier IRI."),
        closed(&["s", "p", "o", "graph"], json!({ "s": string(), "p": string(), "o": string(), "graph": string() })),
    ]});
    put(
        "AssertFactsRequest",
        doc(
            closed(
                &[],
                json!({
                    "graph": with_desc(string(), "The named graph to write (default: `source.iri`)."),
                    "source": closed(&["iri"], json!({ "iri": string(), "title": string() })),
                    "entities": array(closed(&["key", "label", "types"], json!({
                        "key": string(), "label": string(), "types": strings(),
                        "altLabels": strings(), "distinctFrom": strings(),
                    }))),
                    "facts": array(fact),
                    "retract": array(retract),
                    "replaceScope": string_enum(&["graph", "writable"]),
                    "message": string(),
                    "idempotencyKey": with_desc(string(), "At most 128 characters. A retry with the same key writes nothing and answers `alreadyApplied`."),
                    "agent": closed(&["name"], json!({ "name": string(), "model": string() })),
                    "iriBase": string(),
                    "allowUnknownIris": boolean(),
                    "dryRun": boolean(),
                    "changes": int(),
                    "ifHead": int(),
                    "timeoutSeconds": num(),
                    "branch": with_desc(string(), "Write on this branch instead of main."),
                    "retractStale": with_desc(string(), "A rendition of `register_source`. The facts of the graph that cite only earlier renditions of its source, and that this call does not assert again, are retracted."),
                }),
            ),
            "The arguments of `assert_facts` without `dataset`.",
            "importing-agent-memory",
        ),
    );
    let change = obj(
        &["triple", "graph"],
        json!({ "triple": string(), "graph": string(), "reason": string() }),
    );
    put(
        "AssertFactsResult",
        doc(
            obj(
                &["dataset", "graph", "committed", "head"],
                json!({
                    "dataset": string(), "branch": string(), "graph": string(),
                    "committed": boolean(), "wouldCommit": boolean(),
                    "commit": int(), "head": int(), "alreadyApplied": boolean(),
                    "activity": string(),
                    "minted": { "type": "object", "additionalProperties": string() },
                    "inserted": int(), "deleted": int(),
                    "superseded": array(change.clone()), "retracted": array(change.clone()),
                    "conflicts": array(change),
                    "warnings": array(any_object("A problem the write did not refuse.")),
                    "validation": any_object("The guard's report."),
                    "dryRun": any_object("A dry run's preview."),
                    "elapsedMs": num(),
                    "notice": with_desc(string(), "Set when the review policy wrote the facts to the agent's inbox branch."),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What `assert_facts` wrote, or would write in a dry run.",
            "importing-agent-memory",
        ),
    );
    let chunk = obj(
        &["iri", "index", "start", "end"],
        json!({ "iri": string(), "index": int(), "start": int(), "end": int(), "text": string() }),
    );
    put(
        "RegisterSourceRequest",
        doc(
            closed(
                &["text"],
                json!({
                    "graph": with_desc(string(), "The named graph of the source and its facts (default: the source's IRI)."),
                    "iri": with_desc(string(), "The source's IRI (default: a `urn:uuid` minted from the text)."),
                    "title": string(),
                    "format": with_desc(string(), "The media type of the original document (default `text/plain`)."),
                    "text": with_desc(string(), "The document as text or Markdown, at most 2 MiB."),
                    "profile": string(),
                    "message": string(),
                    "dryRun": boolean(),
                    "original": with_desc(string(), "The file's bytes in base64 when the text is their normalized form. They are kept so an export can write the file back unchanged, and their SHA-256 becomes the digest."),
                    "reanchor": with_desc(boolean(), "Give each fact that cites the previous rendition a span in the new text where its quote occurs once, and retract the others."),
                    "reanchorFrom": with_desc(string(), "An earlier source whose facts are copied into this source's graph where their quotes occur in this text."),
                    "timeoutSeconds": num(),
                }),
            ),
            "The arguments of `register_source` without `dataset`.",
            "importing-agent-memory",
        ),
    );
    put(
        "RegisterSourceResult",
        doc(
            obj(
                &[
                    "dataset",
                    "graph",
                    "source",
                    "rendition",
                    "digest",
                    "length",
                    "alreadyRegistered",
                    "committed",
                    "chunks",
                ],
                json!({
                    "dataset": string(), "branch": string(), "graph": string(),
                    "source": string(), "rendition": string(), "digest": string(),
                    "length": int(), "alreadyRegistered": boolean(), "committed": boolean(),
                    "commit": int(), "head": int(), "textKept": boolean(), "profile": string(),
                    "previousRendition": string(), "staleFacts": int(), "originalKept": boolean(),
                    "reanchored": int(), "retracted": int(), "copied": int(),
                    "chunks": array(chunk),
                    "elapsedMs": num(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "What `register_source` stored.",
            "importing-agent-memory",
        ),
    );
    put(
        "SourceList",
        doc(
            obj(
                &["dataset", "commit", "sources", "truncated"],
                json!({
                    "dataset": string(), "branch": string(), "commit": int(),
                    "sources": array(obj(
                        &["source", "graph", "rendition", "chunks", "facts"],
                        json!({
                            "source": string(), "graph": string(), "title": string(),
                            "format": string(), "digest": string(), "rendition": string(),
                            "length": int(), "chunks": int(), "facts": int(),
                            "lastIngestion": string(), "needsExtraction": boolean(),
                            "invalidatedAt": string(),
                        }),
                    )),
                    "truncated": boolean(),
                    "prefixes": any_object("The prefixes the compact terms use."),
                }),
            ),
            "The sources `list_sources` found.",
            "importing-agent-memory",
        ),
    );
    put(
        "BriefRequest",
        doc(
            closed(
                &[],
                json!({
                    "scope": string_enum(&["project", "entity", "session"]),
                    "projectKey": with_desc(string(), "The project's key, such as `github.com/acme/shop`, with scope project."),
                    "entity": with_desc(string(), "An IRI, or a label that must link exactly, with scope entity."),
                    "query": with_desc(string(), "The words to recall, with scope session."),
                    "includeUnreviewed": boolean(),
                    "maxChars": with_desc(int(), "500 to 100,000, 8000 by default."),
                    "maxFacts": with_desc(int(), "1 to 500, 60 by default."),
                    "halfLifeDays": with_desc(num(), "The age at which a fact's weight halves, 90 by default."),
                    "unreviewedWeight": with_desc(num(), "0 to 1, 0.7 by default."),
                    "timeoutSeconds": num(),
                }),
            ),
            "What to brief.",
            "importing-agent-memory",
        ),
    );
    put(
        "BriefResult",
        doc(
            obj(
                &[
                    "dataset",
                    "commit",
                    "scope",
                    "reviewedOnly",
                    "matched",
                    "shown",
                    "text",
                ],
                json!({
                    "dataset": string(), "commit": int(), "scope": string(),
                    "reviewedOnly": boolean(),
                    "imports": with_desc(boolean(), "Whether the dataset has an import base."),
                    "matched": int(), "shown": int(),
                    "text": with_desc(string(), "The brief as plain text. Its first line says that the lines are recalled data."),
                    "facts": array(any_object("One fact with its citations.")),
                    "citations": array(any_object("One source of the facts.")),
                    "prefixes": any_object("The prefixes the compact terms use."),
                    "review": sref("ReviewCounts"),
                    "communities": with_desc(array(any_object("A report with `iri`, `community`, `title`, `summary`, `status` and `memberCount`.")), "In a project brief, the best rated reports of the coarsest level that the caller may read."),
                }),
            ),
            "The brief of a project, an entity or a session. For a caller with `write` on the dataset, `review` and the `# review:` lines of `text` say what waits for review.",
            "importing-agent-memory",
        ),
    );
}

fn models(put: &mut dyn FnMut(&str, J)) {
    let level = string_enum(&["auto", "json-schema", "json-object", "tool", "text"]);
    let pair = closed(
        &["provider", "model"],
        json!({
            "provider": string(),
            "model": string(),
            "maxOutputTokens": int(),
            "requestTimeoutSecs": num(),
        }),
    );
    let model = obj(
        &[
            "name",
            "contextTokens",
            "maxOutputTokens",
            "structuredOutput",
            "requestTimeoutSecs",
            "status",
        ],
        json!({
            "name": string(),
            "contextTokens": int(),
            "maxOutputTokens": int(),
            "structuredOutput": with_desc(level.clone(), "The configured level. `auto` detects it."),
            "requestTimeoutSecs": num(),
            "detected": or_null(with_desc(level.clone(), "The level detected or configured for the pair, once known.")),
            "pricing": obj(&["inputPerMTok", "outputPerMTok"], json!({ "inputPerMTok": num(), "outputPerMTok": num(), "cacheWritePerMTok": num(), "cacheWrite1hPerMTok": num(), "cacheReadPerMTok": num() })),
            "status": obj(&["state"], json!({
                "state": string_enum(&["untested", "ok", "failing"]),
                "at": { "type": "string", "format": "date-time" },
                "message": string(),
            })),
        }),
    );
    let local_model = obj(
        &[
            "provider",
            "model",
            "dtype",
            "threads",
            "idleUnloadSecs",
            "runtime",
            "state",
        ],
        json!({
            "provider": string(),
            "model": string(),
            "repo": string(),
            "revision": string(),
            "path": string(),
            "dtype": string_enum(&["f32", "bf16"]),
            "threads": int(),
            "idleUnloadSecs": int(),
            "runtime": with_desc(boolean(), "Whether the build has the local runtime (cargo feature `embed-local`)."),
            "state": string_enum(&["absent", "downloading", "present", "loading", "loaded", "unloaded"]),
            "arch": string(),
            "dimension": int(),
            "maxTokens": int(),
            "weightBytes": or_null(int()),
            "loads": int(),
            "unloads": int(),
            "texts": int(),
            "queued": int(),
            "lastError": string(),
            "downloadError": string(),
        }),
    );
    let provider = obj(
        &[
            "name",
            "kind",
            "endpoint",
            "status",
            "concurrency",
            "models",
        ],
        json!({
            "name": string(),
            "kind": string_enum(&["ollama", "openai", "anthropic", "local"]),
            "endpoint": with_desc(string(), "Empty for the local kind."),
            "status": with_desc(string_enum(&["ok", "secret-missing"]), "`secret-missing` when the named key cannot be read."),
            "concurrency": int(),
            "models": array(model),
            "apiKey": with_desc(obj(&["secret", "source"], json!({ "secret": string(), "source": with_desc(string_enum(&["declared", "runtime", "missing"]), "Where the key comes from: `--model-secret`, a value stored through `PUT /$/server/secrets/{name}`, or nowhere.") })), "The name of the secret that holds the key and its source. The key itself is never returned."),
            "allowedModels": strings(),
            "requestsPerMinute": int(),
            "budget": with_desc(obj(&["tokensPerDay", "usedToday"], json!({ "tokensPerDay": int(), "usedToday": int() })), "`usedToday` counts input written to the prompt cache at 1.25 tokens, or 2 tokens with the 1-hour TTL, and input read from it at 0.1 token."),
            "promptCaching": with_desc(obj(&["enabled"], json!({ "enabled": boolean(), "ttl": string_enum(&["5m", "1h"]) })), "For the anthropic kind: whether stable prompt prefixes are marked for the provider's cache, and their TTL. For the openai kind, enabled true sends an Ask repair as the first draft's user message followed by a second user message."),
            "unverified": with_desc(boolean(), "True when `tls.insecureSkipVerify` turns certificate verification off for the provider."),
            "tls": with_desc(obj(&["verification"], json!({
                "verification": with_desc(string_enum(&["system", "custom-ca", "off"]), "How the provider's certificate is checked: against the system roots, against them and `tls.caCert`, or not at all."),
                "caCert": with_desc(obj(&["status"], json!({
                    "file": string(),
                    "secret": string(),
                    "source": with_desc(string_enum(&["declared", "runtime", "missing"]), "For a secret, where its value comes from."),
                    "status": with_desc(string_enum(&["ok", "unreadable"]), "`unreadable` when the file or secret cannot be read or holds no PEM certificate."),
                    "message": string(),
                })), "The CA certificate reference of `tls.caCert`. The certificate itself is not returned."),
            })), "The provider's TLS options, present when its configuration has `tls`."),
        }),
    );
    let mut roles = Map::new();
    for r in [
        "draft",
        "repair",
        "summarize",
        "extract",
        "explain",
        "optimize",
    ] {
        roles.insert(r.into(), array(pair.clone()));
    }
    put(
        "ModelProviders",
        doc(
            obj(
                &["configured", "providers", "roles"],
                json!({
                    "configured": with_desc(boolean(), "Whether the server runs with `--model-config`."),
                    "providers": array(provider),
                    "roles": { "type": "object", "properties": J::Object(roles) },
                    "routing": any_object("The routing settings of the configuration."),
                    "modelsDir": with_desc(string(), "The model store of local models (spec F12)."),
                    "local": array(local_model),
                }),
            ),
            "The model providers, their models and the role lists.",
            "model-providers",
        ),
    );
    put(
        "ModelTestRequest",
        doc(
            closed(
                &[],
                json!({
                    "model": with_desc(string(), "The model to test. By default the first one the role lists name."),
                    "timeoutSeconds": with_desc(num(), "At most 600. 60 by default."),
                }),
            ),
            "A test call to one provider and model.",
            "model-providers",
        ),
    );
    put(
        "ModelTestResult",
        doc(
            obj(
                &[
                    "provider",
                    "model",
                    "ok",
                    "structuredOutput",
                    "latencyMs",
                    "inputTokens",
                    "outputTokens",
                    "requests",
                ],
                json!({
                    "provider": string(),
                    "model": string(),
                    "ok": boolean(),
                    "level": or_null(level),
                    "structuredOutput": with_desc(boolean(), "Whether the answer came at a structured level."),
                    "latencyMs": int(),
                    "inputTokens": with_desc(int(), "Input tokens read without the provider's cache."),
                    "outputTokens": int(),
                    "cacheWriteInputTokens": with_desc(int(), "Input tokens written to the provider's prompt cache, present when the provider used its cache."),
                    "cacheWrite1hInputTokens": with_desc(int(), "Of those, the tokens written with the 1-hour TTL."),
                    "cacheReadInputTokens": with_desc(int(), "Input tokens read from the provider's prompt cache."),
                    "budgetTokens": with_desc(int(), "The tokens the call counts against a token budget: output, uncached input, cache writes at 1.25 tokens (2 with the 1-hour TTL) and cache reads at 0.1 token."),
                    "requests": int(),
                    "error": obj(&["code", "message"], json!({ "code": string(), "message": string() })),
                }),
            ),
            "The outcome of a test call.",
            "model-providers",
        ),
    );
}

/// The layered settings of spec C19.
fn settings(put: &mut dyn FnMut(&str, J)) {
    let source = string_enum(&["default", "declared", "runtime", "locked"]);
    put(
        "SettingsKind",
        doc(
            obj(
                &[
                    "dataset",
                    "kind",
                    "effective",
                    "declared",
                    "runtime",
                    "sources",
                    "locked",
                    "overridden",
                    "overrides",
                    "status",
                    "etag",
                ],
                json!({
                    "dataset": string(),
                    "kind": string_enum(&["assistant", "memory", "ingest", "prefixes", "graphrag", "paths", "forms"]),
                    "effective": any_object("The effective object: the built-in defaults, the declared values and the runtime layer merged, with the locked fields from the settings file."),
                    "declared": any_object("The settings file's `defaults` and dataset entry for this kind, merged."),
                    "runtime": any_object("The runtime layer: the fields changed through the API. `null` removes a declared member."),
                    "sources": { "type": "object", "description": "The source of each field of `effective`, by dotted path such as `budget.perRequest`.", "additionalProperties": source },
                    "locked": with_desc(strings(), "The fields the settings file locks."),
                    "overridden": with_desc(strings(), "Locked fields whose runtime value is kept in the file but ignored."),
                    "overrides": with_desc(array(sref("SettingsOverride")), "The runtime values used in place of a different value of the settings file, one entry per field. Locked fields are left out, since their runtime value is ignored."),
                    "status": closed(&["valid"], json!({ "valid": boolean(), "error": string() })),
                    "etag": with_desc(string(), "The entity tag of the runtime layer, as in the `ETag` header."),
                    "warnings": with_desc(array(sref("PrefixWarning")), "The `prefixes` kind only: the prefixes that shadow a well-known prefix with another IRI."),
                }),
            ),
            "One settings kind of a dataset with its layers and sources.",
            "settings",
        ),
    );
    put(
        "PrefixWarning",
        doc(
            closed(
                &["prefix", "iri", "wellKnown", "message"],
                json!({
                    "prefix": with_desc(string(), "The prefix name."),
                    "iri": with_desc(string(), "The IRI the dataset binds it to."),
                    "wellKnown": with_desc(string(), "The well-known IRI of the name."),
                    "message": string(),
                }),
            ),
            "A prefix whose name is well-known but whose IRI differs (spec C20).",
            "settings",
        ),
    );
    put(
        "SettingsOverride",
        doc(
            closed(
                &["path", "declared", "runtime"],
                json!({
                    "path": with_desc(string(), "The field, as a dotted path. It is the runtime leaf, or the field above it that the declared layers set to something other than an object."),
                    "declared": with_desc(json!({}), "The value of the declared layers that the runtime value replaces."),
                    "runtime": with_desc(json!({}), "The runtime value in use, or `null` for a declared provider removed at runtime."),
                }),
            ),
            "A runtime value that is used in place of a different declared value. A reset of `path` brings the declared value back.",
            "settings",
        ),
    );
    put(
        "DatasetSettings",
        doc(
            obj(
                &["dataset", "kinds"],
                json!({
                    "dataset": string(),
                    "kinds": { "type": "object", "additionalProperties": sref("SettingsKind") },
                }),
            ),
            "Every settings kind of a dataset.",
            "settings",
        ),
    );
    let file = obj(
        &["path", "readAt", "error", "errorAt"],
        json!({
            "path": or_null(string()),
            "readAt": or_null(string()),
            "error": with_desc(or_null(string()), "Why the last reload failed, while the previous file stays in use."),
            "errorAt": or_null(string()),
        }),
    );
    let mut status = file.clone();
    status["required"] = json!([
        "path",
        "readAt",
        "error",
        "errorAt",
        "declared",
        "unmatched",
        "kinds",
        "serverKinds",
        "models"
    ]);
    status["properties"]["declared"] =
        with_desc(strings(), "The dataset names of the settings file.");
    status["properties"]["unmatched"] = with_desc(
        strings(),
        "Declared names that match no dataset. Their entries apply when such a dataset is created.",
    );
    status["properties"]["kinds"] = strings();
    status["properties"]["serverKinds"] = with_desc(
        strings(),
        "The server-wide kinds of `/$/server/settings/{kind}`.",
    );
    status["properties"]["models"] = with_desc(
        file,
        "The model configuration of `--model-config`, which SIGHUP reads again too.",
    );
    put(
        "SettingsStatus",
        doc(
            status,
            "The settings file of `serve --settings` and its reads.",
            "settings",
        ),
    );
}

/// The server-wide settings and runtime secrets of spec C19 §11.
fn server_settings(put: &mut dyn FnMut(&str, J)) {
    let source = string_enum(&["default", "declared", "runtime", "locked"]);
    put(
        "ServerSettingsKind",
        doc(
            obj(
                &[
                    "scope",
                    "kind",
                    "effective",
                    "declared",
                    "runtime",
                    "sources",
                    "locked",
                    "overridden",
                    "overrides",
                    "status",
                    "etag",
                ],
                json!({
                    "scope": string_enum(&["server"]),
                    "kind": string_enum(&["models"]),
                    "effective": any_object("The effective object. For `models` it is a `ModelsSettings`: `providers`, `roles` and `routing`, from the built-in defaults, `--model-config` and the runtime layer, with the locked fields from the declared configuration."),
                    "declared": any_object("The model configuration of `--model-config`."),
                    "runtime": any_object("The runtime layer kept in `<dataDir>/models.json`. `null` for a provider removes a declared provider."),
                    "sources": { "type": "object", "description": "The source of each field of `effective`, by dotted path such as `providers.claude.endpoint`.", "additionalProperties": source },
                    "locked": with_desc(strings(), "The fields that `server.locked` of the settings file locks, without the `models.` prefix."),
                    "overridden": with_desc(strings(), "Locked fields whose runtime value is kept but ignored."),
                    "overrides": with_desc(array(sref("SettingsOverride")), "The runtime values used in place of a different value of `--model-config`, one entry per field, such as a changed budget or a removed provider. Locked fields are left out."),
                    "status": closed(&["valid"], json!({ "valid": boolean(), "error": string() })),
                    "etag": with_desc(string(), "The entity tag of the runtime layer, as in the `ETag` header."),
                }),
            ),
            "A server-wide settings kind with its layers and sources.",
            "server-settings",
        ),
    );
    put(
        "SecretList",
        doc(
            obj(
                &["storage", "keySource", "keyMemory", "secrets"],
                json!({
                    "storage": with_desc(string_enum(&["sealed", "plaintext"]), "How runtime values are kept in the data directory: `sealed` by default in builds with the `backup` feature, or `plaintext` in files with mode 0600 when the server runs with `--secrets-key none` or was built without that feature."),
                    "keySource": with_desc(or_null(string_enum(&["configured", "generated"])), "Where the key that seals the values comes from: `configured` for `serve --secrets-key` or the systemd credential `sparkles-secrets-key`, `generated` for the key the server keeps in `<dataDir>/secrets.key`, which it writes when it first stores a value with sealing enabled. `null` for plaintext values."),
                    "keyMemory": with_desc(or_null(string_enum(&["locked", "heap"])), "Where the server keeps the key: `locked` in a locked page that core dumps leave out, or `heap` in a buffer that is zeroed on release when a protected page is unavailable. `null` until there is a key."),
                    "secrets": array(obj(
                        &["name", "source", "declared", "locked", "setAt", "overridden", "providers"],
                        json!({
                            "name": string(),
                            "source": with_desc(string_enum(&["declared", "runtime", "missing"]), "The source in force: a stored value, `--model-secret`, or none."),
                            "declared": with_desc(boolean(), "Whether `--model-secret` names the secret."),
                            "locked": with_desc(boolean(), "Whether `server.locked` names `secrets.NAME`, so that only the declared source applies."),
                            "setAt": with_desc(or_null(string()), "When the runtime value was stored."),
                            "overridden": with_desc(boolean(), "A runtime value is stored but the lock ignores it."),
                            "providers": with_desc(strings(), "The providers whose `apiKey` or `tls.caCert` names the secret."),
                            "channels": with_desc(strings(), "The notification channels that name the secret (spec C21)."),
                        }),
                    )),
                }),
            ),
            "The model secrets. A value is never returned.",
            "model-secrets",
        ),
    );
    let delivery = || {
        or_null(obj(
            &["at", "event"],
            json!({
                "at": string(),
                "event": string(),
                "id": string(),
                "status": with_desc(int(), "The HTTP status of a delivery."),
                "error": with_desc(string(), "Why a delivery failed. It never holds a secret."),
            }),
        ))
    };
    put(
        "NotificationStatus",
        doc(
            obj(
                &[
                    "enabled", "channels", "routes", "queued", "active", "recent",
                ],
                json!({
                    "enabled": boolean(),
                    "channels": array(obj(
                        &["name", "type", "target", "secrets", "lastSuccess", "lastFailure", "sent", "failed"],
                        json!({
                            "name": string(),
                            "type": string_enum(&["webhook", "ntfy"]),
                            "target": with_desc(string(), "Where the channel delivers, without credentials."),
                            "secrets": with_desc(strings(), "The secrets the channel names."),
                            "lastSuccess": delivery(),
                            "lastFailure": delivery(),
                            "sent": with_desc(int(), "Deliveries that succeeded since the start."),
                            "failed": with_desc(int(), "Deliveries that failed, were refused or were dropped since the start."),
                        }),
                    )),
                    "routes": any_object("The routes from event types to channels."),
                    "queued": with_desc(int(), "Deliveries waiting in the queue."),
                    "active": array(obj(
                        &["key", "event", "first", "last", "count"],
                        json!({
                            "key": string(),
                            "event": string(),
                            "dataset": or_null(string()),
                            "first": string(),
                            "last": string(),
                            "count": int(),
                        }),
                    )),
                    "recent": array(any_object("A delivery: `at`, `id`, `event`, `channel`, `result` (`ok`, `failed`, `refused` or `dropped`), `attempts`, and `status` or `error`.")),
                }),
            ),
            "Whether notifications are on, each channel's last success and failure, the conditions notified and the recent deliveries.",
            "notifications",
        ),
    );
    put(
        "NotificationTest",
        doc(
            obj(
                &["channel", "type", "result", "status", "id", "latencyMs"],
                json!({
                    "channel": string(),
                    "type": string_enum(&["webhook", "ntfy"]),
                    "result": string_enum(&["ok"]),
                    "status": with_desc(int(), "The HTTP status the channel answered."),
                    "id": with_desc(string(), "The envelope's id."),
                    "latencyMs": int(),
                }),
            ),
            "A test notification that the channel accepted.",
            "notifications",
        ),
    );
    put(
        "SecretValue",
        doc(
            obj(
                &["value"],
                json!({ "value": with_desc(string(), "The key. It is stored and never returned.") }),
            ),
            "The value of a model secret.",
            "model-secrets",
        ),
    );
}

fn graphrag(put: &mut dyn FnMut(&str, J)) {
    let statuses = string_enum(&["unreviewed", "reviewed", "rejected", "stale", "failed"]);
    put(
        "GraphragBuildRequest",
        doc(
            closed(
                &[],
                json!({
                    "confirm": with_desc(boolean(), "Start without waiting for a confirmation of the estimate. The default is `false`, and the task then waits for `POST /$/graphrag/{ds}/confirm`."),
                }),
            ),
            "A build or a refresh of a dataset's community reports.",
            "graphrag",
        ),
    );
    put(
        "GraphragConfirmRequest",
        doc(
            closed(
                &["task"],
                json!({
                    "task": with_desc(string(), "The id of the waiting task."),
                    "confirm": with_desc(boolean(), "`true` (the default) starts the run, and `false` ends it without sending anything."),
                }),
            ),
            "The answer to a build's estimate.",
            "graphrag",
        ),
    );
    put(
        "GraphragStatus",
        doc(
            obj(
                &["dataset", "enabled", "reportsGraph", "available"],
                json!({
                    "dataset": string(),
                    "enabled": boolean(),
                    "reportsGraph": string(),
                    "refresh": string_enum(&["manual", "auto"]),
                    "available": with_desc(boolean(), "Whether the caller may read the reports graph. When it is `false`, the answer holds nothing derived from the reports."),
                    "reason": string(),
                    "reports": with_desc(json!({ "type": "object", "additionalProperties": int() }), "The current reports by status."),
                    "levels": array(obj(&["level", "communities"], json!({ "level": int(), "communities": int() }))),
                    "lastBuild": obj(&[], json!({
                        "sourceCommit": int(),
                        "branch": string(),
                        "finishedAt": { "type": "string", "format": "date-time" },
                        "tokens": int(),
                        "pending": with_desc(int(), "The communities the last run left for a later one."),
                    })),
                    "budget": with_desc(obj(&[], json!({
                        "tokensPerBuild": int(),
                        "tokensPerDay": int(),
                        "usedToday": int(),
                    })), "For a dataset admin only."),
                    "anomalies": with_desc(array(any_object("An accounting anomaly with the pair, the counting source and both figures.")), "For a dataset admin only."),
                }),
            ),
            "The state of a dataset's community reports.",
            "graphrag",
        ),
    );
    let report_ref = obj(
        &["iri"],
        json!({ "iri": string(), "title": string(), "status": statuses.clone(), "rating": int() }),
    );
    put(
        "GraphragCommunities",
        doc(
            obj(
                &["available", "communities"],
                json!({
                    "available": boolean(),
                    "reportsGraph": string(),
                    "communities": array(obj(&["iri"], json!({
                        "iri": string(),
                        "level": int(),
                        "parent": or_null(string()),
                        "memberCount": int(),
                        "report": or_null(report_ref),
                    }))),
                }),
            ),
            "The communities of the reports graph, by level and then by rating.",
            "graphrag",
        ),
    );
    put(
        "GraphragMembershipsRequest",
        doc(
            closed(
                &["nodes"],
                json!({
                    "nodes": { "type": "array", "items": string(), "maxItems": 20000 },
                    "level": int(),
                }),
            ),
            "The nodes whose communities to read, at one level or at every level.",
            "graphrag",
        ),
    );
    put(
        "GraphragMemberships",
        doc(
            obj(
                &["available", "memberships", "communities"],
                json!({
                    "available": boolean(),
                    "reason": string(),
                    "reportsGraph": string(),
                    "memberships": array(obj(&["node", "community"], json!({
                        "node": string(),
                        "community": string(),
                        "level": int(),
                    }))),
                    "communities": array(obj(&["iri"], json!({
                        "iri": string(),
                        "level": int(),
                        "memberCount": int(),
                        "title": or_null(string()),
                    }))),
                }),
            ),
            "The communities that hold the listed nodes, with each community's member count and report title.",
            "graphrag",
        ),
    );
    put(
        "GraphragCommunity",
        doc(
            obj(
                &["iri"],
                json!({
                    "iri": string(),
                    "level": int(),
                    "parent": or_null(string()),
                    "memberCount": int(),
                    "factCount": int(),
                    "members": with_desc(array(obj(&["iri"], json!({ "iri": string(), "label": or_null(string()) }))), "Up to 500 members."),
                    "children": array(obj(&["iri"], json!({ "iri": string(), "title": or_null(string()) }))),
                    "report": obj(&["iri"], json!({
                        "iri": string(),
                        "title": or_null(string()),
                        "summary": or_null(string()),
                        "status": statuses.clone(),
                        "rating": or_null(int()),
                        "failure": or_null(string()),
                        "revisionOf": or_null(string()),
                        "droppedCitations": or_null(int()),
                        "droppedUnsupported": or_null(int()),
                        "sourceCommit": or_null(int()),
                        "activity": obj(&[], json!({ "model": string(), "inputTokens": int(), "outputTokens": int(), "startedAt": string() })),
                        "findings": array(obj(&["text"], json!({
                            "text": string(),
                            "about": strings(),
                            "cites": with_desc(strings(), "Cited facts as N-Triples triple terms, reifiers, and cited child findings."),
                            "unmatchedNames": strings(),
                        }))),
                    })),
                }),
            ),
            "A community with its members, children and report.",
            "graphrag",
        ),
    );
    put(
        "GraphragReviewRequest",
        doc(
            closed(
                &["report"],
                json!({
                    "report": with_desc(string(), "The report's IRI."),
                    "status": string_enum(&["reviewed", "rejected"]),
                    "title": with_desc(string(), "A new title. With `title` or `summary` the edit is a new report with status `reviewed` that supersedes this one."),
                    "summary": string(),
                }),
            ),
            "A review of a community report.",
            "graphrag",
        ),
    );
    put(
        "GraphragInbox",
        doc(
            obj(
                &["reportsGraph", "available"],
                json!({
                    "reportsGraph": string(),
                    "available": with_desc(boolean(), "Whether the caller may read the reports graph. When it is `false`, nothing else derived from the reports is listed."),
                    "unreviewed": int(),
                    "reports": with_desc(array(obj(&["community", "report"], json!({
                        "community": string(),
                        "report": string(),
                        "title": or_null(string()),
                        "rating": or_null(int()),
                        "level": or_null(int()),
                    }))), "Up to 50 unreviewed reports, highest rating first."),
                    "truncated": boolean(),
                    "anomalies": with_desc(array(any_object("An accounting anomaly.")), "For a dataset admin only."),
                }),
            ),
            "The community reports that wait for review, present when GraphRAG is enabled for the dataset.",
            "graphrag",
        ),
    );
    put(
        "GraphragReviewResult",
        obj(
            &["report", "status"],
            json!({ "report": string(), "status": statuses, "commit": or_null(int()) }),
        ),
    );
}
