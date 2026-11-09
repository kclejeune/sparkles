//! Claims mode (spec C25, "Ingestion into claim proposals"): the document is registered
//! as a C18 source with its chunks, and its extracted facts become claim proposals of
//! one evidence batch instead of asserted memory. The registration and the batch
//! publish in one commit of the evidence owner, so a reader sees both or neither.
//!
//! The run checks the caller's grants and the evidence settings before it converts
//! anything, plans the registration without writing it, answers `already-proposed`
//! without a model call when its idempotency key already published, extracts and
//! links the facts, and hands the batch with the registration's quads to
//! [`sparkles::evidence::propose`] with `ifHead` set to the head the plan read. A head
//! that moved is planned again, without a model call, up to three times.

use super::Status;
use super::convert::{self, Format, Refusal};
use super::extract::{Fact, Link, Linked, Obj, Vocabulary};
use super::pipeline::{Failed, Request, Run, expand_iri};
use crate::auth::{Endpoint, Level, on_branch};
use oxrdf::vocab::{rdf, rdfs, xsd};
use oxrdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};
use serde_json::{Map, Value, json};
use sparkles::evidence::{
    ClaimInput, ClaimKind, Confidence, ConfidenceFacets, EvidenceInput, EvidenceLinkInput,
    EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, MethodRef, OrdinaryChanges,
    PassageInput, Polarity, RecordKey, RecordRef, RecordVersion, RenditionContent, RenditionInput,
    Sha256Digest, SourceContent, SourceRevisionInput,
};
use sparkles::task::Control;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

/// How many times a moved head is planned again.
const REPLANS: usize = 3;
const ZERO_TO_ONE: &str = "urn:x-sparkles:evidence:ZeroToOne";

fn failed(code: &str, status: u16, message: impl Into<String>) -> Failed {
    Failed(json!({ "code": code, "status": status, "message": message.into() }))
}

fn api(e: crate::http::ApiError) -> Failed {
    let (status, body) = e.into_parts();
    let code = body["code"].as_str().unwrap_or(match status.as_u16() {
        400 => "bad-argument",
        403 => "forbidden",
        413 => "too-large",
        404 => "not-found",
        409 => "conflict",
        _ => "error",
    });
    let mut j = json!({
        "code": code,
        "status": status.as_u16(),
        "message": body["error"].as_str().or(body["message"].as_str()).unwrap_or(""),
    });
    for k in ["reference", "field"] {
        if let Some(v) = body.get(k) {
            j[k] = v.clone();
        }
    }
    Failed(j)
}

fn key(s: impl Into<String>) -> RecordKey {
    RecordKey::new(s).expect("batch keys are nonempty")
}

fn sha(bytes: &[u8]) -> Result<Sha256Digest, Failed> {
    Sha256Digest::of_bytes(bytes, &Control::none()).map_err(|e| failed("error", 500, e.to_string()))
}

/// What the evidence settings and grants allow this run, checked before conversion.
struct Target {
    main: Arc<crate::state::Dataset>,
    logical: Arc<crate::state::Dataset>,
    branch: String,
    source_graph: NamedNode,
    proposal_graph: NamedNode,
}

/// The caller needs `write` on the update endpoint of the branch, update grants on the
/// evidence graphs the batch writes, and enabled evidence settings (spec C25, "Grants
/// and settings"). Nothing is converted or sent to a model before this passes.
fn target(r: &Run, req: &Request) -> Result<Target, Failed> {
    let ctx = r.ctx;
    let st = &ctx.server.state;
    let p = ctx.principal;
    let main = crate::http::main_dataset(st, &req.dataset).map_err(api)?;
    let branch = req
        .branch
        .clone()
        .unwrap_or_else(|| sparkles::branch::MAIN.to_string());
    let logical = if branch == sparkles::branch::MAIN {
        main.clone()
    } else {
        st.branch_dataset(&main, &branch)
            .map_err(|e| api(e.into()))?
    };
    let here = on_branch(&main.name, &branch);
    if !p.can(&here, Level::Write) || !p.can_at(&here, Endpoint::Update, Level::Write) {
        return Err(failed(
            "forbidden",
            403,
            format!(
                "write access to the update endpoint of dataset {} required",
                main.name
            ),
        ));
    }
    let status = sparkles::evidence::settings(&main.dataset).map_err(|e| api(e.into()))?;
    let settings = status.settings;
    if !settings.enabled {
        return Err(failed(
            "unsupported-profile",
            409,
            format!(
                "dataset {} has no enabled evidence settings: configure them with PUT /$/evidence/{}/settings",
                main.name, main.name
            ),
        ));
    }
    let pick = |chosen: &Option<String>, all: &[String], field: &str| {
        let g = match chosen {
            Some(g) if all.iter().any(|x| x == g) => g.clone(),
            Some(g) => {
                return Err(failed(
                    "bad-argument",
                    400,
                    format!("{field} <{g}> is not a graph of that role in the evidence settings"),
                ));
            }
            None if all.len() == 1 => all[0].clone(),
            None => {
                return Err(failed(
                    "bad-argument",
                    400,
                    format!(
                        "the evidence settings name several graphs of that role: choose one with {field}"
                    ),
                ));
            }
        };
        NamedNode::new(g).map_err(|e| failed("bad-argument", 400, e.to_string()))
    };
    // a review verifies every cited passage against the rendition's retained text, so a
    // dataset that keeps no source text could never accept these claims
    if !crate::mcp::memory::ingest::ingest_settings(st, &main).keep_text {
        return Err(failed(
            "no-text",
            409,
            format!(
                "dataset {} keeps no source text (keepText is false), and a review verifies claims against the retained text: claims mode needs keepText",
                main.name
            ),
        ));
    }
    let source_graph = pick(&req.source_graph, &settings.source_records, "sourceGraph")?;
    let proposal_graph = pick(&req.proposal_graph, &settings.proposals, "proposalGraph")?;
    let view = write_view(r, &main, &logical, &branch);
    if let Some(v) = &view {
        for g in [&source_graph, &proposal_graph] {
            let t = Term::NamedNode(g.clone());
            if !v.writable(Some(&t)) {
                return Err(failed(
                    "forbidden",
                    403,
                    format!(
                        "write access to graph <{}> of dataset {} required",
                        g.as_str(),
                        main.name
                    ),
                ));
            }
        }
    }
    Ok(Target {
        main,
        logical,
        branch,
        source_graph,
        proposal_graph,
    })
}

/// The caller's write view of the update endpoint, `None` for every graph.
fn write_view(
    r: &Run,
    main: &Arc<crate::state::Dataset>,
    logical: &Arc<crate::state::Dataset>,
    branch: &str,
) -> Option<Arc<sparkles::access::GraphAccess>> {
    let st = &r.ctx.server.state;
    let caller = crate::http::caller::CallerContext::for_caller(st, logical, r.ctx.principal);
    caller
        .options_on(
            &crate::http::Params::empty(),
            &on_branch(&main.name, branch),
            Endpoint::Update,
        )
        .graphs
}

/// The registration planned at the branch head: its result, its ordinary changes and
/// the head it read.
struct Planned {
    registered: Value,
    changes: OrdinaryChanges,
    head: u64,
}

fn plan(r: &Run, reg: &Map<String, Value>, t: &Target) -> Result<Planned, Failed> {
    let mut args = reg.clone();
    args.insert("plan".into(), true.into());
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let registered = r.tool("register_converted", Value::Object(args), branch)?;
    let head = registered["head"]
        .as_u64()
        .ok_or_else(|| failed("error", 500, "the registration plan has no head"))?;
    let changes = match registered["update"].as_str() {
        Some(u) => ordinary(u)?,
        None => OrdinaryChanges::default(),
    };
    // the source graph must be ordinary: no profile owns it
    let graph = expand_iri(
        registered["graph"].as_str().unwrap_or(""),
        &registered["prefixes"],
    );
    if let Ok(Some(snapshot)) = t.logical.store.managed_snapshot()
        && snapshot.graphs.owner_of(&graph).is_some()
    {
        return Err(failed(
            "profile-managed-graph",
            409,
            format!("graph <{graph}> is managed by a profile; name an ordinary source graph"),
        ));
    }
    let view = write_view(r, &t.main, &t.logical, &t.branch);
    if let (Some(v), Ok(g)) = (&view, NamedNode::new(graph.clone()))
        && !v.writable(Some(&Term::NamedNode(g)))
    {
        return Err(failed(
            "forbidden",
            403,
            format!(
                "write access to graph <{graph}> of dataset {} required",
                t.main.name
            ),
        ));
    }
    Ok(Planned {
        registered,
        changes,
        head,
    })
}

/// The quads of a registration's `DELETE DATA` and `INSERT DATA`.
fn ordinary(update: &str) -> Result<OrdinaryChanges, Failed> {
    use spargebra::GraphUpdateOperation as Op;
    use spargebra::term::GraphName as G;
    let parsed = spargebra::SparqlParser::new()
        .parse_update(update)
        .map_err(|e| failed("error", 500, format!("the registration plan: {e}")))?;
    let graph = |g: G| match g {
        G::NamedNode(n) => GraphName::NamedNode(n),
        G::DefaultGraph => GraphName::DefaultGraph,
    };
    let mut out = OrdinaryChanges::default();
    for op in parsed.operations {
        match op {
            Op::InsertData { data } => out.insert.extend(
                data.into_iter()
                    .map(|q| Quad::new(q.subject, q.predicate, q.object, graph(q.graph_name))),
            ),
            Op::DeleteData { data } => out.delete.extend(data.into_iter().map(|q| {
                Quad::new(
                    q.subject,
                    q.predicate,
                    Term::from(q.object),
                    graph(q.graph_name),
                )
            })),
            _ => return Err(failed("error", 500, "the registration plan is not data")),
        }
    }
    Ok(out)
}

/// The idempotency key of a claims ingestion (spec C25, "Idempotent re-ingestion"): a
/// versioned digest of a canonical framing of its source, its graphs, its content and
/// its profile, so that the same text under another source or graph is another
/// ingestion.
fn idempotency_key(
    t: &Target,
    source: &str,
    graph: &str,
    revision: &Sha256Digest,
    rendition: &Sha256Digest,
    profile: &str,
) -> Result<String, Failed> {
    let framed = json!([
        "ingest-claims",
        1,
        source,
        graph,
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
        revision.to_string(),
        rendition.to_string(),
        profile,
    ])
    .to_string();
    Ok(format!("ingest-claims:1:{}", sha(framed.as_bytes())?))
}

/// The source key of the revision: the source IRI framed with the C18 graph and the
/// evidence graphs, so that the same source and text ingested into other graphs gets
/// other record identities instead of colliding with the first ones.
fn source_key(t: &Target, source: &str, graph: &str) -> Result<String, Failed> {
    let framed = json!([
        "ingest-claims-source",
        1,
        source,
        graph,
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
    ])
    .to_string();
    Ok(format!(
        "ingest-claims-source:1:{}",
        sha(framed.as_bytes())?
    ))
}

/// The revision digest: the content digest that the C18 registration records, which is
/// the normalized text's unless the registration retained other original bytes.
fn recorded_digest(v: &Value, text: &Sha256Digest) -> Sha256Digest {
    match v.as_str().map(str::parse::<Sha256Digest>) {
        Some(Ok(d)) => d,
        _ => *text,
    }
}

/// Whether the caller's operation of `key` is recorded in the evidence graphs of the
/// selected branch.
fn already(t: &Target, principal: &NamedNode, key: &str) -> Result<Option<NamedNode>, Failed> {
    let id = sparkles::evidence::operation_id(t.main.dataset.dataset_id(), principal, key);
    let op = sparkles::evidence::operation_iri(id);
    let q = format!(
        "ASK {{ VALUES ?g {{ <{}> <{}> }} GRAPH ?g {{ <{}> a <urn:x-sparkles:evidence:Operation> }} }}",
        t.source_graph.as_str(),
        t.proposal_graph.as_str(),
        op.as_str()
    );
    let found = t.logical.dataset.ask(&q).map_err(|e| api(e.into()))?;
    Ok(found.then_some(op))
}

/// A model or converter as an IRI.
fn agent_iri(prefix: &str, name: &str) -> NamedNode {
    let enc =
        percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC).to_string();
    NamedNode::new_unchecked(format!("{prefix}{enc}"))
}

/// The facts, entities and spans of an extraction as one evidence batch.
struct Batch {
    input: EvidenceInput,
    /// the facts left out, with their code
    failed: Vec<Value>,
    flagged: Vec<Value>,
    /// the possible duplicates of new entities, reported and written nowhere
    candidates: Vec<Value>,
    entities: Value,
    claims: usize,
}

struct Source<'a> {
    source: NamedNode,
    source_key: String,
    revision_digest: Sha256Digest,
    locator: Option<NamedNode>,
    rendition_key: String,
    media_type: &'a str,
    format: &'a str,
    text: &'a str,
}

/// One claim of an extraction with its span, confidence and the model whose answer
/// gave it.
struct Proposition {
    triple: Triple,
    span: (usize, usize),
    confidence: Option<f64>,
    model: NamedNode,
}

/// The claims that an extraction makes once its entities are linked.
struct Claims {
    claims: Vec<Proposition>,
    /// the facts left out, with their code
    failed: Vec<Value>,
    flagged: Vec<Value>,
    candidates: Vec<Value>,
    entities: Value,
}

/// The claims of `facts` with the entities of `linked`, refused with `too-large` when
/// they are more than one batch holds. A new entity's IRI is a name-based UUID of the
/// revision.
fn claims_of(
    r: &Run,
    v: &Vocabulary,
    facts: &[Fact],
    linked: &Linked,
    revision_digest: &Sha256Digest,
    mut failed_facts: Vec<Value>,
) -> Result<Claims, Failed> {
    let dataset_id = r
        .ctx
        .server
        .state
        .get(&r.dataset)
        .map(|d| d.store.dataset_id())
        .unwrap_or_default();
    let prefixes = merged(&v.prefixes, &linked.prefixes);
    let named = |s: &str| -> Option<NamedNode> {
        let s = s.trim();
        let s = s
            .strip_prefix('<')
            .and_then(|x| x.strip_suffix('>'))
            .unwrap_or(s);
        NamedNode::new(expand_iri(s, &prefixes)).ok()
    };
    // the entities: existing IRIs, or a name-based UUID of the revision for a new one
    let mut flagged: Vec<Value> = Vec::new();
    let mut candidates: Vec<Value> = Vec::new();
    let entity_iri: Vec<Option<NamedNode>> = linked
        .entities
        .iter()
        .zip(&linked.links)
        .map(|((text, class), l)| match l {
            Link::Existing(i) => named(i),
            Link::Ambiguous(i, c) => {
                flagged.push(json!({ "entity": text, "chosen": i, "candidates": c }));
                named(i)
            }
            Link::New(c) => {
                if !c.is_empty() {
                    let c: Vec<String> = c
                        .iter()
                        .filter_map(|i| named(i).map(|n| n.as_str().to_string()))
                        .collect();
                    candidates.push(json!({ "entity": text, "candidates": c }));
                }
                let norm = crate::mcp::memory::ingest::fold_ws(text).to_lowercase();
                let id = crate::mcp::memory::assert::uuid_v5(
                    &dataset_id,
                    &format!("ingest-claims-entity\0{}\0{norm}\0{class}", revision_digest),
                );
                Some(NamedNode::new_unchecked(format!(
                    "urn:uuid:{}",
                    id.hyphenated()
                )))
            }
        })
        .collect();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut claims: Vec<Proposition> = Vec::new();
    let mut first_span: HashMap<usize, (usize, usize, NamedNode)> = HashMap::new();
    for f in facts {
        let term_of = |k: &str| linked.entity_of.get(k).and_then(|e| entity_iri[*e].clone());
        let Some(s) = term_of(&f.s) else {
            failed_facts.push(json!({ "code": "unknown-mention", "message": "the subject has no entity", "p": f.p }));
            continue;
        };
        let Some(p) = named(&f.p) else {
            failed_facts.push(json!({ "code": "unknown-predicate", "message": format!("{} is not an IRI", f.p), "p": f.p }));
            continue;
        };
        let o: Term = match &f.o {
            Obj::Key(k) => match term_of(k) {
                Some(o) => o.into(),
                None => {
                    failed_facts.push(json!({ "code": "unknown-mention", "message": "the object has no entity", "p": f.p }));
                    continue;
                }
            },
            Obj::Lit(l) => match (&l.datatype, &l.lang) {
                (Some(dt), _) => {
                    let dt = match dt.strip_prefix("xsd:") {
                        Some(local) => {
                            NamedNode::new(format!("http://www.w3.org/2001/XMLSchema#{local}")).ok()
                        }
                        None => named(dt),
                    };
                    match dt {
                        Some(dt) => Literal::new_typed_literal(l.value.clone(), dt).into(),
                        None => {
                            failed_facts.push(json!({ "code": "invalid-literal", "message": "the datatype is not an IRI", "p": f.p }));
                            continue;
                        }
                    }
                }
                (None, Some(lang)) => {
                    match Literal::new_language_tagged_literal(l.value.clone(), lang) {
                        Ok(l) => l.into(),
                        Err(_) => {
                            failed_facts.push(json!({ "code": "invalid-literal", "message": "the language tag is invalid", "p": f.p }));
                            continue;
                        }
                    }
                }
                (None, None) => Literal::new_simple_literal(l.value.clone()).into(),
            },
        };
        let entities_of = std::iter::once(&f.s).chain(match &f.o {
            Obj::Key(k) => Some(k),
            Obj::Lit(_) => None,
        });
        let model = model_iri(f.pair.as_ref());
        for k in entities_of {
            if let Some(e) = linked.entity_of.get(k) {
                first_span
                    .entry(*e)
                    .or_insert((f.start, f.end, model.clone()));
            }
        }
        let triple = Triple::new(s, p, o);
        let id = format!("{triple} {}-{}", f.start, f.end);
        if seen.contains_key(&id) {
            continue;
        }
        seen.insert(id, claims.len());
        claims.push(Proposition {
            triple,
            span: (f.start, f.end),
            confidence: f.confidence,
            model,
        });
    }
    // a new entity's class and label are claims too, cited by its first passage
    let label_p = v
        .label_predicate
        .as_deref()
        .and_then(named)
        .unwrap_or_else(|| rdfs::LABEL.into_owned());
    let mut new_entities = 0usize;
    for (e, ((text, class), l)) in linked.entities.iter().zip(&linked.links).enumerate() {
        if !matches!(l, Link::New(_)) {
            continue;
        }
        let (Some(iri), Some((a, b, model))) = (&entity_iri[e], first_span.get(&e)) else {
            continue;
        };
        let (a, b) = (*a, *b);
        new_entities += 1;
        let span = (a, b);
        if let Some(c) = named(class) {
            claims.push(Proposition {
                triple: Triple::new(iri.clone(), rdf::TYPE, c),
                span,
                confidence: None,
                model: model.clone(),
            });
        }
        let label: Term = match &v.language {
            Some(lang) => Literal::new_language_tagged_literal(text.clone(), lang)
                .map(Term::from)
                .unwrap_or_else(|_| Literal::new_simple_literal(text.clone()).into()),
            None => Literal::new_simple_literal(text.clone()).into(),
        };
        claims.push(Proposition {
            triple: Triple::new(iri.clone(), label_p.clone(), label),
            span,
            confidence: None,
            model: model.clone(),
        });
    }
    if claims.len() > sparkles::evidence::MAX_CLAIMS {
        return Err(too_large(format!(
            "the extraction gives {} claims; one batch holds at most {}",
            claims.len(),
            sparkles::evidence::MAX_CLAIMS
        )));
    }
    let count = |f: fn(&Link) -> bool| linked.links.iter().filter(|l| f(l)).count();
    let entities = json!({
        "linked": count(|l| matches!(l, Link::Existing(_))),
        "new": new_entities,
        "ambiguous": count(|l| matches!(l, Link::Ambiguous(..))),
    });
    flagged.dedup();
    Ok(Claims {
        claims,
        failed: failed_facts,
        flagged,
        candidates,
        entities,
    })
}

/// The claims of an extraction as one evidence batch, refused with `too-large` when its
/// preparation exceeds the decoded bound of spec C25 §8.
fn batch(src: &Source, c: Claims, control: &Control) -> Result<Batch, Failed> {
    let mut input = EvidenceInput::default();
    input.revisions.push(SourceRevisionInput {
        version: RecordVersion::V1,
        key: key("rev"),
        source: src.source.clone(),
        source_key: key(src.source_key.clone()),
        upstream_version: None,
        content: SourceContent::MetadataOnly {
            digest: src.revision_digest,
        },
        metadata: sparkles::evidence::SourceMetadata {
            locators: src.locator.iter().cloned().collect(),
            reported_rights: Vec::new(),
        },
        times: Default::default(),
        lineage: Default::default(),
    });
    input.renditions.push(RenditionInput {
        version: RecordVersion::V1,
        key: key("text"),
        revision: RecordRef::Local(key("rev")),
        occurrence_key: key(src.rendition_key.clone()),
        conversion: MethodRef {
            method: agent_iri("urn:x-sparkles:ingest:convert:", src.format),
            version: key(env!("CARGO_PKG_VERSION")),
        },
        media_type: src.media_type.to_string(),
        language: None,
        content: RenditionContent::NormalizedText(src.text.to_string()),
        expected_digest: None,
    });
    let chars: Vec<char> = src.text.chars().collect();
    // one passage and one reference per distinct span, in the order the claims cite them
    let mut spans: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for (n, claim) in c.claims.iter().enumerate() {
        let (a, b) = claim.span;
        let i = match spans.get(&(a, b)) {
            Some(i) => *i,
            None => {
                let i = spans.len();
                let quote: String = chars[a..b].iter().collect();
                input.passages.push(PassageInput {
                    version: RecordVersion::V1,
                    key: key(format!("p{i}")),
                    revision: RecordRef::Local(key("rev")),
                    rendition: RecordRef::Local(key("text")),
                    occurrence_key: key(format!("{a}-{b}")),
                    start: a as u64,
                    end: b as u64,
                    text_digest: sha(quote.as_bytes())?,
                    quote: Some(quote),
                });
                input.references.push(EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key(format!("r{i}")),
                    occurrence_key: key(format!("{a}-{b}")),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Local(key("rev")),
                        rendition: RecordRef::Local(key("text")),
                        passage: RecordRef::Local(key(format!("p{i}"))),
                    },
                });
                spans.insert((a, b), i);
                i
            }
        };
        let digest = sha(format!("{} {a}-{b}", claim.triple).as_bytes())?;
        let confidence = claim.confidence.map(|v| Confidence {
            scale: NamedNode::new_unchecked(ZERO_TO_ONE),
            value: Literal::new_typed_literal(decimal(v), xsd::DECIMAL),
            evaluator: claim.model.clone(),
            method: Some(MethodRef {
                method: NamedNode::new_unchecked("urn:x-sparkles:ingest:extract"),
                version: key(env!("CARGO_PKG_VERSION")),
            }),
        });
        input.claims.push(ClaimInput {
            version: RecordVersion::V1,
            key: key(format!("c{n}")),
            occurrence_key: key(digest.to_string()),
            revision: RecordRef::Local(key("rev")),
            proposition: claim.triple.clone(),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: src.source.clone(),
            scope: Default::default(),
            confidence: ConfidenceFacets {
                extraction: confidence,
                ..Default::default()
            },
        });
        input.links.push(EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key(format!("l{n}")),
            occurrence_key: key(digest.to_string()),
            target: RecordRef::Local(key(format!("c{n}"))),
            evidence: RecordRef::Local(key(format!("r{i}"))),
            relation: EvidenceRelation::Supports,
            attributed_author: claim.model.clone(),
        });
    }
    // the decoded bound is the one the proposal's preparation charges
    match sparkles::evidence::prepare_input(&input, control) {
        Ok(_) => {}
        Err(sparkles::Error::BudgetExceeded(b))
            if matches!(b.kind, sparkles::BudgetKind::DecompressedBytes) =>
        {
            return Err(too_large(format!(
                "the batch decodes to at least {} bytes; one batch holds at most {}",
                b.requested, b.limit
            )));
        }
        Err(e) => return Err(api(e.into())),
    }
    Ok(Batch {
        claims: input.claims.len(),
        input,
        failed: c.failed,
        flagged: c.flagged,
        candidates: c.candidates,
        entities: c.entities,
    })
}

/// A zero-to-one confidence as an exact decimal lexical form.
fn decimal(c: f64) -> String {
    let s = format!("{:.6}", c.clamp(0.0, 1.0));
    let s = s.trim_end_matches('0');
    if s.ends_with('.') {
        format!("{s}0")
    } else {
        s.to_string()
    }
}

fn too_large(message: String) -> Failed {
    failed("too-large", 413, message)
}

fn merged(a: &Value, b: &Value) -> Value {
    let mut out = a.clone();
    if !out.is_object() {
        out = json!({});
    }
    if let (Some(o), Some(b)) = (out.as_object_mut(), b.as_object()) {
        for (k, v) in b {
            o.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }
    out
}

/// Publish the batch with the registration planned at `planned.head`, planning again
/// when the head moved.
fn publish(
    r: &Run,
    t: &Target,
    input: &EvidenceInput,
    reg: Option<&Map<String, Value>>,
    mut planned: Option<Planned>,
    idempotency_key: &str,
    message: String,
) -> Result<(sparkles::evidence::Proposed, Option<Value>), Failed> {
    let st = r.ctx.server.state.clone();
    let p = r.ctx.principal.clone();
    let control = control_of(r);
    let mut attempt = 0;
    loop {
        r.check()?;
        let mut options =
            sparkles::evidence::ProposeOptions::new(crate::evidence::principal_iri(&p));
        options.idempotency_key = Some(idempotency_key.to_string());
        options.source_graph = Some(t.source_graph.clone());
        // a batch of the revision and rendition alone writes no proposal graph
        let proposes = !(input.references.is_empty()
            && input.claims.is_empty()
            && input.links.is_empty()
            && input.assessments.is_empty());
        options.proposal_graph = proposes.then(|| t.proposal_graph.clone());
        options.message = Some(Arc::from(message.as_str()));
        options.author = crate::http::author(&p);
        options.write = write_view(r, &t.main, &t.logical, &t.branch);
        options.authority = Some(crate::evidence::caller_fence(st.clone(), p.clone()));
        if let Some(pl) = &planned {
            options.if_head = Some(pl.head);
            options.ordinary = Some(pl.changes.clone());
        }
        t.main.store.failpoint("ingest-claims-before-publish");
        match crate::evidence::propose_ingested(
            &st,
            (&t.main, &t.logical),
            &p,
            input,
            &options,
            &control,
        ) {
            Ok(proposed) => return Ok((proposed, planned.map(|p| p.registered))),
            Err(e) => {
                let f = api(e);
                let moved = f.0["code"] == "head-changed";
                if moved
                    && attempt < REPLANS
                    && let Some(reg) = reg
                {
                    attempt += 1;
                    planned = Some(plan(r, reg, t)?);
                    continue;
                }
                return Err(f);
            }
        }
    }
}

fn proposed_json(p: &sparkles::evidence::Proposed, out: &mut Value) {
    let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
    for rec in &p.records {
        *by_kind
            .entry(crate::evidence::kind_name(rec.kind))
            .or_default() += 1;
    }
    out["operation"] = p.operation.as_str().into();
    out["committed"] = p.committed.into();
    out["replayed"] = p.replayed.into();
    out["commit"] = p.commit.into();
    out["branchId"] = json!(p.branch);
    out["firstAvailableAt"] = json!(p.first_available_at);
    out["hold"] = json!(p.hold);
    out["recordCounts"] = json!(by_kind);
    out["records"] = p
        .records
        .iter()
        .map(crate::evidence::record_json)
        .collect::<Vec<_>>()
        .into();
    for rec in &p.records {
        match rec.key.as_str() {
            "rev" => out["revision"] = rec.iri.as_str().into(),
            "text" => out["evidenceRendition"] = rec.iri.as_str().into(),
            _ => {}
        }
    }
}

/// The extraction pairs, required in claims mode.
fn models_and_pairs<'m>(
    r: &Run<'m, '_>,
    ds: &crate::state::Dataset,
    req: &Request,
) -> Result<(&'m crate::models::Models, Vec<crate::models::Pair>), Failed> {
    let settings = crate::assistant::settings(&r.ctx.server.state, ds);
    let pairs = if req.pairs.is_empty() {
        super::pipeline::extract_pairs(r.ctx.models, &settings)
    } else {
        req.pairs.clone()
    };
    match r.ctx.models.filter(|_| !pairs.is_empty()) {
        Some(m) => Ok((m, pairs)),
        None => Err(Failed::new(
            "no-model",
            "claims mode extracts facts, and the dataset's ingestion has no provider and model in the extract role: enable ingest in its assistant settings, with a provider that may receive documents",
        )),
    }
}

fn model_iri(pair: Option<&crate::models::Pair>) -> NamedNode {
    match pair {
        Some(p) => agent_iri(
            "urn:x-sparkles:model:",
            &format!("{}/{}", p.provider, p.model),
        ),
        None => NamedNode::new_unchecked("urn:x-sparkles:model:unknown"),
    }
}

/// Run a claims ingestion of a document.
/// The run's deadline and cancellation as a task control.
fn control_of(r: &Run) -> Control {
    Control {
        deadline: Some(r.ctx.deadline),
        ..Control::with_cancel(r.ctx.cancel.clone())
    }
}

/// Extract the facts of `text` and link their entities on `branch`. Linking follows the
/// extraction chunk by chunk, so the extraction stops calling the model as soon as the
/// claims collected, with the class and label claims of new entities, pass the bound of
/// one batch.
fn extract_and_link(
    r: &mut Run,
    (models, pairs): (&crate::models::Models, &[crate::models::Pair]),
    vocab: &Vocabulary,
    (text, chunks): (&str, &[(usize, usize)]),
    branch: Option<&str>,
    revision_digest: &Sha256Digest,
) -> Result<(super::extract::Extraction, Linked), Failed> {
    let mut linker = super::extract::Linker::new();
    let mut check = |r: &mut Run, ex: &super::extract::Extraction| -> Result<(), Failed> {
        linker.update(r, ex, branch)?;
        claims_of(
            r,
            vocab,
            &ex.facts,
            &linker.linked(ex),
            revision_digest,
            Vec::new(),
        )
        .map(|_| ())
    };
    let ex = super::extract::extract(r, models, pairs, vocab, text, chunks, Some(&mut check))?;
    r.ctx.progress.status(Status::Linking, 0.82, None);
    linker.update(r, &ex, branch)?;
    let linked = linker.linked(&ex);
    Ok((ex, linked))
}

pub(super) fn run(r: &mut Run, req: &Request) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let t = target(r, req)?;
    let (models, pairs) = models_and_pairs(r, &t.main, req)?;
    let principal = crate::evidence::principal_iri(ctx.principal);
    let name = req.name.clone().or_else(|| {
        req.url
            .as_deref()
            .map(|u| super::pipeline::base_name(u).to_string())
    });
    let format = Format::detect(req.media_type.as_deref(), name.as_deref(), &req.bytes)?;
    if matches!(format, Format::Csv | Format::Tsv) || format.is_spreadsheet() {
        return Err(Refusal::new(
            "unsupported-format",
            415,
            "claims mode ingests documents: a table is mapped with a draft in the other modes",
        )
        .into());
    }
    // 1. conversion
    ctx.progress.status(Status::Converting, 0.05, None);
    r.check()?;
    let converted = convert::convert(
        &req.bytes,
        format,
        &convert::Options {
            allow_partial: req.allow_partial,
            max_input: convert::MAX_INPUT_BYTES,
            max_text: convert::MAX_TEXT_BYTES,
            deadline: ctx.deadline,
            pdf: ctx.pdf.clone(),
            max_images: 0,
        },
    )?;
    r.check()?;
    // the text exactly as the registration normalizes it, so spans agree with it
    let text = crate::mcp::memory::ingest::normalize(&converted.text);
    let title = req
        .title
        .clone()
        .or_else(|| converted.title.clone())
        .or_else(|| name.clone());
    let iri = req.iri.clone().or_else(|| {
        req.url
            .clone()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    });
    let profile = req.profile.clone().unwrap_or_else(|| "default".into());
    // 2. the registration, planned without writing it
    ctx.progress.status(Status::Registering, 0.1, None);
    let mut reg = Map::new();
    if let Some(g) = &req.graph {
        reg.insert("graph".into(), g.clone().into());
    }
    if let Some(i) = &iri {
        reg.insert("iri".into(), i.clone().into());
    }
    if let Some(t) = &title {
        reg.insert(
            "title".into(),
            t.chars().take(1000).collect::<String>().into(),
        );
    }
    reg.insert("format".into(), converted.media_type.clone().into());
    reg.insert("text".into(), text.clone().into());
    reg.insert("profile".into(), profile.clone().into());
    if let Some(m) = &req.message {
        reg.insert("message".into(), m.clone().into());
    }
    reg.insert(
        "rendition".into(),
        super::pipeline::rendition_extras(&converted),
    );
    let planned = plan(r, &reg, &t)?;
    let registered = planned.registered.clone();
    let prefixes = registered["prefixes"].clone();
    let source = expand_iri(registered["source"].as_str().unwrap_or(""), &prefixes);
    let rendition = expand_iri(registered["rendition"].as_str().unwrap_or(""), &prefixes);
    let graph = expand_iri(registered["graph"].as_str().unwrap_or(""), &prefixes);
    let rendition_digest = sha(text.as_bytes())?;
    let revision_digest = recorded_digest(&registered["digest"], &rendition_digest);
    let key = idempotency_key(
        &t,
        &source,
        &graph,
        &revision_digest,
        &rendition_digest,
        &profile,
    )?;
    let mut out = json!({
        "mode": "claims",
        "format": format.name(),
        "source": source,
        "graph": graph,
        "rendition": rendition,
        "digest": registered["digest"],
        "length": registered["length"],
        "profile": profile,
        "factGuard": "not-run",
    });
    if t.branch != sparkles::branch::MAIN {
        out["branch"] = t.branch.clone().into();
    }
    // 3. a repeat of a published ingestion answers without a model call
    if let Some(op) = already(&t, &principal, &key)? {
        out["outcome"] = "already-proposed".into();
        out["operation"] = op.as_str().into();
        out["message"] =
            "this revision was already ingested as claims; nothing was extracted or written".into();
        return Ok(out);
    }
    // 4. the estimate, the extraction and linking
    let chunks = crate::mcp::memory::ingest::chunk_bounds(&text, false);
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let vocab = Vocabulary::read_on(r, &profile, branch)?;
    estimate(r, req, models, &pairs, &vocab, &text, &chunks, &t)?;
    let (ex, linked) = extract_and_link(
        r,
        (models, &pairs),
        &vocab,
        (&text, &chunks),
        branch,
        &revision_digest,
    )?;
    ctx.progress.status(Status::Writing, 0.9, None);
    let src = Source {
        source: NamedNode::new(source.clone()).map_err(|e| failed("error", 500, e.to_string()))?,
        source_key: source_key(&t, &source, &graph)?,
        revision_digest,
        locator: req
            .url
            .as_deref()
            .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
            .and_then(|u| NamedNode::new(u).ok()),
        rendition_key: rendition.clone(),
        media_type: &converted.media_type,
        format: format.name(),
        text: &text,
    };
    let c = claims_of(
        r,
        &vocab,
        &ex.facts,
        &linked,
        &revision_digest,
        ex.failed.clone(),
    )?;
    let b = batch(&src, c, &control_of(r))?;
    let message = format!(
        "Ingest claims from {}",
        title.as_deref().unwrap_or(source.as_str())
    );
    let (proposed, registered) =
        publish(r, &t, &b.input, Some(&reg), Some(planned), &key, message)?;
    if let Some(reg) = registered {
        out["alreadyRegistered"] = reg["alreadyRegistered"].clone();
        out["chunks"] = json!(reg["chunks"].as_array().map_or(0, Vec::len));
        if let Some(p) = reg.get("previousRendition") {
            out["previousRendition"] = p.clone();
        }
    }
    finish(&mut out, &proposed, &b, &ex);
    Ok(out)
}

fn finish(
    out: &mut Value,
    proposed: &sparkles::evidence::Proposed,
    b: &Batch,
    ex: &super::extract::Extraction,
) {
    out["outcome"] = if b.claims > 0 { "proposed" } else { "no-facts" }.into();
    out["claims"] = b.claims.into();
    out["extracted"] = ex.facts.len().into();
    out["failed"] = b.failed.clone().into();
    out["entities"] = b.entities.clone();
    if !b.flagged.is_empty() {
        out["flagged"] = b.flagged.clone().into();
    }
    if !b.candidates.is_empty() {
        out["candidates"] = b.candidates.clone().into();
    }
    if ex.chunks_failed > 0 {
        out["chunksFailed"] = ex.chunks_failed.into();
    }
    proposed_json(proposed, out);
}

#[allow(clippy::too_many_arguments)]
fn estimate(
    r: &Run,
    req: &Request,
    models: &crate::models::Models,
    pairs: &[crate::models::Pair],
    vocab: &Vocabulary,
    text: &str,
    chunks: &[(usize, usize)],
    t: &Target,
) -> Result<(), Failed> {
    let st = &r.ctx.server.state;
    let settings = crate::assistant::settings(st, &t.main);
    let ingest = crate::mcp::memory::ingest::ingest_settings(st, &t.main);
    let limits = super::pipeline::Limits {
        confirm_tokens: ingest
            .confirm_tokens
            .unwrap_or(super::pipeline::DEFAULT_CONFIRM_TOKENS),
        auto_confidence: ingest
            .auto_confidence
            .unwrap_or(super::pipeline::DEFAULT_AUTO_CONFIDENCE),
    };
    super::pipeline::confirm_estimate(
        r,
        req,
        models,
        pairs,
        vocab,
        text,
        chunks,
        &limits,
        (&t.main, &settings),
    )?;
    r.check()
}

/// Run a claims ingestion of a registered source, named by its graph or IRI. Its C18
/// records exist, so the commit holds the batch alone.
pub(super) fn run_source(r: &mut Run, req: &Request, source: &str) -> Result<Value, Failed> {
    let ctx = r.ctx;
    let t = target(r, req)?;
    let (models, pairs) = models_and_pairs(r, &t.main, req)?;
    let principal = crate::evidence::principal_iri(ctx.principal);
    let on = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let read = super::pipeline::read_source(r, source, on)?;
    let profile = req.profile.clone().unwrap_or_else(|| "default".into());
    let rendition_digest = sha(read.text.as_bytes())?;
    let revision_digest = recorded_digest(&read.found["digest"], &rendition_digest);
    let source_iri = expand_iri(read.found["source"].as_str().unwrap_or(""), &read.prefixes);
    let key = idempotency_key(
        &t,
        &source_iri,
        &read.graph,
        &revision_digest,
        &rendition_digest,
        &profile,
    )?;
    let mut out = json!({
        "mode": "claims",
        "source": source_iri,
        "graph": read.graph,
        "rendition": read.rendition,
        "length": read.found["length"],
        "chunks": read.chunks.len(),
        "profile": profile,
        "factGuard": "not-run",
    });
    if t.branch != sparkles::branch::MAIN {
        out["branch"] = t.branch.clone().into();
    }
    if let Some(op) = already(&t, &principal, &key)? {
        out["outcome"] = "already-proposed".into();
        out["operation"] = op.as_str().into();
        return Ok(out);
    }
    let branch = (t.branch != sparkles::branch::MAIN).then_some(t.branch.as_str());
    let vocab = Vocabulary::read_on(r, &profile, branch)?;
    estimate(r, req, models, &pairs, &vocab, &read.text, &read.chunks, &t)?;
    let (ex, linked) = extract_and_link(
        r,
        (models, &pairs),
        &vocab,
        (&read.text, &read.chunks),
        branch,
        &revision_digest,
    )?;
    ctx.progress.status(Status::Writing, 0.9, None);
    let media_type = read.found["format"]
        .as_str()
        .unwrap_or("text/markdown")
        .to_string();
    let src = Source {
        source: NamedNode::new(source_iri.clone())
            .map_err(|e| failed("error", 500, e.to_string()))?,
        source_key: source_key(&t, &source_iri, &read.graph)?,
        revision_digest,
        locator: None,
        rendition_key: read.rendition.clone(),
        media_type: &media_type,
        format: "registered",
        text: &read.text,
    };
    let c = claims_of(
        r,
        &vocab,
        &ex.facts,
        &linked,
        &revision_digest,
        ex.failed.clone(),
    )?;
    let b = batch(&src, c, &control_of(r))?;
    let message = format!("Ingest claims from {source_iri}");
    let (proposed, _) = publish(r, &t, &b.input, None, None, &key, message)?;
    finish(&mut out, &proposed, &b, &ex);
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn decimals_are_exact_lexical_forms() {
        assert_eq!(super::decimal(0.9), "0.9");
        assert_eq!(super::decimal(1.0), "1.0");
        assert_eq!(super::decimal(0.0), "0.0");
        assert_eq!(super::decimal(0.125), "0.125");
    }

    #[test]
    fn a_registration_plan_becomes_ordinary_changes() {
        let c = super::ordinary(
            "DELETE DATA { GRAPH <urn:g> { <urn:s> <urn:p> \"old\" . } } ; INSERT DATA { GRAPH <urn:g> { <urn:s> <urn:p> \"new\" . } }",
        )
        .ok()
        .unwrap();
        assert_eq!(c.delete.len(), 1);
        assert_eq!(c.insert.len(), 1);
        assert_eq!(c.insert[0].graph_name.to_string(), "<urn:g>");
    }

    /// One claim whose literal object holds `bytes` bytes, as a batch.
    fn one_claim(bytes: usize) -> Result<super::Batch, super::Failed> {
        use oxrdf::{Literal, NamedNode, Triple};
        let text = "Ana Lima moved to the payments team this week.";
        let src = super::Source {
            source: NamedNode::new_unchecked("urn:source"),
            source_key: "k".into(),
            revision_digest: super::sha(text.as_bytes()).ok().unwrap(),
            locator: None,
            rendition_key: "urn:rendition".into(),
            media_type: "text/markdown",
            format: "markdown",
            text,
        };
        let c = super::Claims {
            claims: vec![super::Proposition {
                triple: Triple::new(
                    NamedNode::new_unchecked("urn:s"),
                    NamedNode::new_unchecked("urn:p"),
                    Literal::new_simple_literal("x".repeat(bytes)),
                ),
                span: (0, 8),
                confidence: Some(0.9),
                model: NamedNode::new_unchecked("urn:x-sparkles:model:m"),
            }],
            failed: Vec::new(),
            flagged: Vec::new(),
            candidates: Vec::new(),
            entities: serde_json::json!({}),
        };
        super::batch(&src, c, &sparkles::task::Control::default())
    }

    #[test]
    fn the_decoded_bound_is_the_one_the_proposal_charges() {
        // a batch whose content is most of the 16 MiB is admitted, though an estimate
        // that counted its fields twice would refuse it
        let b = one_claim(15 << 20).ok().unwrap();
        assert_eq!(b.claims, 1);
        let e = one_claim(16 << 20).err().unwrap();
        assert_eq!(e.0["code"], "too-large", "{}", e.0);
        assert_eq!(e.0["status"], 413);
    }
}
