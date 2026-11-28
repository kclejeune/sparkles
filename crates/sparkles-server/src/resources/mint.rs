//! Vocabulary IRI policies and reservations in the destination commit.

use super::{Error, ResourceInput, Result, invalid};
use crate::auth::{Endpoint, Level, Principal};
use crate::http::{ApiResult, err_code};
use crate::state::{AppState, Dataset};
use axum::http::StatusCode;
use oxrdf::{NamedNode, Quad};
use serde::{Deserialize, Serialize};
use sparkles::commit::{CommitKind, Receipt};
use sparkles::guard::WriteOptions;
use std::collections::BTreeMap;
use uuid::Uuid;

const MAX_RANGE: u64 = 65_536;
const RESERVATION: &str = "urn:x-sparkles:IriReservation";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Policy {
    pub namespace: String,
    pub rule: Rule,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields, rename_all = "kebab-case")]
pub enum Rule {
    Slug {
        template: String,
    },
    Uuid,
    Ulid,
    Sequence {
        stem: String,
        width: u8,
        start: u64,
        end: u64,
    },
    Obo {
        stem: String,
        width: u8,
        ranges: BTreeMap<String, Range>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Range {
    pub start: u64,
    pub end: u64,
}

fn token(s: &str) -> bool {
    s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn range(width: u8, start: u64, end: u64) -> Result<()> {
    if !(1..=19).contains(&width)
        || start > end
        || end - start >= MAX_RANGE
        || end >= 10u64.pow(u32::from(width))
    {
        return Err(invalid(
            "each IRI range must fit its width and contain at most 65,536 IDs",
        ));
    }
    Ok(())
}

pub(crate) fn validate(resource: &ResourceInput, policy: &Policy) -> Result<()> {
    if resource.kind != super::Kind::Vocabulary
        || !resource
            .namespaces
            .iter()
            .any(|ns| ns.iri == policy.namespace)
    {
        return Err(invalid(
            "an IRI policy must use a namespace owned by its vocabulary",
        ));
    }
    match &policy.rule {
        Rule::Slug { template } => {
            let Some((before, after)) = template.split_once("{slug}") else {
                return Err(invalid("a slug template must contain {slug}"));
            };
            if !token(before) || !token(after) {
                return Err(invalid(
                    "a slug template must use a single {slug} and literal letters, digits, dots, hyphens or underscores",
                ));
            }
        }
        Rule::Sequence {
            stem,
            width,
            start,
            end,
        } => {
            if !token(stem) {
                return Err(invalid("invalid IRI sequence stem"));
            }
            range(*width, *start, *end)?;
        }
        Rule::Obo {
            stem,
            width,
            ranges,
        } => {
            if stem.is_empty()
                || !token(stem)
                || ranges.is_empty()
                || ranges.len() > super::MAX_LIST
            {
                return Err(invalid(
                    "an OBO IRI policy needs a stem and one to 256 editor ranges",
                ));
            }
            let mut spans = Vec::new();
            for (editor, span) in ranges {
                super::text("editor", editor, 256, true)?;
                range(*width, span.start, span.end)?;
                spans.push((span.start, span.end));
            }
            spans.sort_unstable();
            if spans.windows(2).any(|pair| pair[0].1 >= pair[1].0) {
                return Err(invalid("OBO editor ranges overlap"));
            }
        }
        Rule::Uuid | Rule::Ulid => {}
    }
    Ok(())
}

fn slug(label: &str) -> Result<String> {
    super::text("label", label, 1024, true)?;
    let mut words = label
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty());
    let Some(first) = words.next() else {
        return Err(invalid(
            "a slug label needs at least one ASCII letter or digit",
        ));
    };
    let mut slug = first.to_ascii_lowercase();
    for word in words {
        slug.push('-');
        slug.push_str(&word.to_ascii_lowercase());
    }
    Ok(slug)
}

fn ulid() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let timestamp = chrono::Utc::now().timestamp_millis() as u128 & ((1 << 48) - 1);
    // A v4 UUID has 48 random high bits and 32 random low bits.
    let uuid = Uuid::new_v4().as_u128();
    let random = ((uuid >> 80) << 32) | u128::from(uuid as u32);
    let value = (timestamp << 80) | random;
    (0..26)
        .rev()
        .map(|index| char::from(ALPHABET[((value >> (5 * index)) & 31) as usize]))
        .collect()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub label: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Reservation {
    iri: String,
    receipt: Receipt,
}

pub(crate) fn reserve(
    st: &AppState,
    ds: &Dataset,
    p: &Principal,
    id: &str,
    request: Request,
    mut opts: WriteOptions,
) -> ApiResult<Reservation> {
    let denied = |message| err_code(StatusCode::FORBIDDEN, "forbidden", message);
    if ds.branch.is_some() || !super::valid_id(id) {
        return Err(invalid("minting requires a valid vocabulary id and the main branch").into());
    }
    let graph = NamedNode::new_unchecked(format!("urn:x-sparkles:iri-reservations:{id}"));
    if !p.can_at(&ds.name, Endpoint::Query, Level::Read)
        || p.view(&ds.name, Endpoint::Query)
            .is_some_and(|v| !v.reads_everything())
        || !p.can_at(&ds.name, Endpoint::Update, Level::Write)
    {
        return Err(denied(
            "minting needs Update access and Query access to the entire dataset",
        ));
    }
    opts.graphs = p.view(&ds.name, Endpoint::Update);
    if opts
        .graphs
        .as_ref()
        .is_some_and(|v| !v.writable_iri(graph.as_str()))
    {
        return Err(denied(
            "minting needs write access to the reservation graph",
        ));
    }
    let principal = p.id();
    let catalog = st.resources.read()?;
    {
        let resource = super::find(&catalog, id).ok_or_else(|| super::no_such(id))?;
        if !resource.input.graphs.iter().any(|g| g.dataset == ds.name) {
            return Err(
                invalid("the vocabulary must have a registered graph in this dataset").into(),
            );
        }
        let policy = resource
            .input
            .iri_policy
            .as_ref()
            .ok_or_else(|| invalid("the vocabulary has no IRI policy"))?;
        let mut txn = ds
            .store
            .try_write_with(CommitKind::Transaction, opts.clone())?;
        let snapshot = txn.view();
        let sequence =
            |stem: &str, width: u8, start: u64, end: u64| -> sparkles::Result<Option<String>> {
                for n in start..=end {
                    opts.check()?;
                    let candidate = format!(
                        "{}{stem}{n:0width$}",
                        policy.namespace,
                        width = usize::from(width)
                    );
                    if snapshot.lookup_iri(&candidate).is_none() {
                        return Ok(Some(candidate));
                    }
                }
                Ok(None)
            };
        let candidate = match &policy.rule {
            Rule::Slug { template } => {
                let label = request
                    .label
                    .as_deref()
                    .ok_or_else(|| invalid("the slug policy needs a label"))?;
                Some(format!(
                    "{}{}",
                    policy.namespace,
                    template.replace("{slug}", &slug(label)?)
                ))
            }
            Rule::Uuid => Some(format!("{}{}", policy.namespace, Uuid::new_v4())),
            Rule::Ulid => Some(format!("{}{}", policy.namespace, ulid())),
            Rule::Sequence {
                stem,
                width,
                start,
                end,
            } => sequence(stem, *width, *start, *end)?,
            Rule::Obo {
                stem,
                width,
                ranges,
            } => {
                let span = ranges
                    .get(&principal)
                    .ok_or_else(|| denied("no OBO range is assigned to the caller"))?;
                sequence(stem, *width, span.start, span.end)?
            }
        };
        let iri = candidate
            .filter(|iri| snapshot.lookup_iri(iri).is_none())
            .ok_or_else(|| {
                Error::Conflict(
                    "iri-unavailable",
                    "the IRI is already in use or the range is exhausted".into(),
                )
            })?;
        super::iri("minted IRI", &iri)?;
        drop(snapshot);
        let quad = Quad::new(
            NamedNode::new_unchecked(iri.clone()),
            oxrdf::vocab::rdf::TYPE,
            NamedNode::new_unchecked(RESERVATION),
            graph,
        );
        let keys = txn.encode_quad(&quad, &mut Default::default())?;
        txn.insert(keys)?;
        let receipt = txn.commit()?;
        Ok(Reservation { iri, receipt })
    }
}
