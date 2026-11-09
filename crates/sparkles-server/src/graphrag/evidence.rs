//! Reports on accepted evidence (spec C25, "Reports on accepted evidence").
//!
//! A run whose settings name a `reviewPolicy` selects the Head evidence view of its
//! branch and reads under the policy that view's configuration records. It reads the
//! policy's output graph with the projection's graphs and leaves every other graph of an
//! evidence role out. A triple of the output graph enters an input only with a claim the
//! principal may see, accepted with its current decision, or when another graph of the
//! projection also holds it. The claims of each such triple are read once per run into
//! one table of claim and decision pairs, which the inputs, the stored citations, the
//! activity's `prov:used` and the build state name by position.
//!
//! Every count and byte the table keeps is charged before it is kept, and a run that
//! would pass a bound refuses as `incomplete-evidence` rather than keep a partial list.

use super::build::RunError;
use sparkles::access::GraphAccess;
use sparkles::evidence::{self, ClaimState, Settings};
use sparkles::store::{BoundEvidenceView, Snapshot};
use sparkles::task::Control;
use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// The most visible claim occurrences behind one community's input.
pub const MAX_INPUT_OCCURRENCES: usize = 10_000;
/// The most distinct claim occurrences one run keeps.
pub const MAX_RUN_OCCURRENCES: usize = 1_000_000;
/// The most links between the triples a run read and their occurrences.
pub const MAX_RUN_LINKS: usize = 1_000_000;
/// The bytes a run may keep for its evidence, its citations and its level commits.
pub const RUN_ALLOWANCE: u64 = 256 << 20;
/// The memory the selection of the evidence view may hold.
const VIEW_BYTES: u64 = 64 << 20;
/// The most pairs one evidence read checks.
const CHECK_CHUNK: usize = 1_000;
/// The bytes charged for each entry of a table or map beyond its strings.
const ENTRY_BYTES: u64 = 64;
/// The bytes charged for each reference to a pair.
const REF_BYTES: u64 = 8;

/// The bounds of a run's evidence. Tests lower them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub input: usize,
    pub occurrences: usize,
    pub links: usize,
    pub bytes: u64,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            input: MAX_INPUT_OCCURRENCES,
            occurrences: MAX_RUN_OCCURRENCES,
            links: MAX_RUN_LINKS,
            bytes: RUN_ALLOWANCE,
        }
    }
}

/// The policy a run reads under, as the view records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPolicy {
    pub name: String,
    pub iri: String,
    pub version: u32,
    pub output: String,
    pub decisions: String,
    pub supports: String,
    /// the configuration history's lineage and the SHA-256 of the selected generation,
    /// joined with a slash
    pub configuration: String,
}

/// A triple as canonical N-Triples terms.
pub type TripleKey = (String, String, String);

/// What a recheck compares: the dataset's current evidence settings against those the
/// run's view recorded.
#[derive(Clone)]
pub struct EvidenceCheck {
    dataset: sparkles::Dataset,
    recorded: Settings,
}

impl EvidenceCheck {
    pub fn check(&self) -> Result<(), RunError> {
        let now = evidence::settings(&self.dataset).map_err(engine)?;
        if now.settings != self.recorded {
            return Err(RunError::Refused(
                "the evidence settings changed during the run; start a new run".into(),
            ));
        }
        Ok(())
    }
}

/// The evidence of one run.
pub struct Evidence {
    pub policy: RunPolicy,
    view: BoundEvidenceView,
    access: Option<Arc<GraphAccess>>,
    check: EvidenceCheck,
    /// graphs with an evidence role other than the policy's output, which no input reads
    pub excluded: BTreeSet<String>,
    /// the evidence graphs the footprint lists besides the projection's graphs
    pub footprint: Vec<String>,
    /// the configured proposal graphs, where claims and their withdrawals are
    pub proposals: Vec<String>,
    control: Control,
    limits: Limits,
    /// the claim and decision pairs, by position
    pairs: Vec<(String, String)>,
    by_claim: HashMap<String, u32>,
    /// the pairs of each triple of the output graph the run read
    triples: HashMap<TripleKey, Vec<u32>>,
    links: usize,
    spent: u64,
}

/// A sparkles error as a run error. A refusal keeps its code in front of its message.
pub fn engine(e: sparkles::Error) -> RunError {
    match e {
        sparkles::Error::Cancelled => RunError::Cancelled,
        sparkles::Error::Managed(m) => RunError::Refused(format!("{}: {}", m.code, m.message)),
        sparkles::Error::Invalid(m) => RunError::Refused(match m.strip_prefix("policy: ") {
            Some(rest) => format!("reviewPolicy: {rest}"),
            None => m,
        }),
        e => RunError::Failed(e.to_string()),
    }
}

fn incomplete(m: String) -> RunError {
    RunError::Refused(format!("incomplete-evidence: {m}"))
}

fn term(nt: &str) -> Option<oxrdf::Term> {
    super::store::parse_term(nt)
}

/// The triple of a key, when its subject is an IRI and its predicate an IRI.
pub fn triple_of(k: &TripleKey) -> Option<oxrdf::Triple> {
    let s = match term(&k.0)? {
        oxrdf::Term::NamedNode(n) => n,
        _ => return None,
    };
    let p = match term(&k.1)? {
        oxrdf::Term::NamedNode(n) => n,
        _ => return None,
    };
    Some(oxrdf::Triple::new(s, p, term(&k.2)?))
}

fn key_of(t: &oxrdf::Triple) -> TripleKey {
    (
        t.subject.to_string(),
        t.predicate.to_string(),
        t.object.to_string(),
    )
}

impl Evidence {
    /// Admit a run under `settings.reviewPolicy`, before it reads anything: the store's
    /// configuration history must be active, and the policy a review policy of the Head
    /// view's configuration whose decisions, supports and output graphs the principal
    /// may read, with a projection that still authenticates. `None` without a policy.
    pub fn admit(
        ds: &crate::state::Dataset,
        name: Option<&str>,
        access: Option<Arc<GraphAccess>>,
        cancel: Arc<AtomicBool>,
        limits: Limits,
    ) -> Result<Option<Evidence>, RunError> {
        let Some(name) = name else {
            return Ok(None);
        };
        let control = Control::with_cancel(cancel);
        let main = ds.main();
        let main: &crate::state::Dataset = main.as_deref().unwrap_or(ds);
        let preparation =
            sparkles::access::Preparation::new(control.clone(), VIEW_BYTES).map_err(engine)?;
        let view = main
            .dataset
            .store()
            .evidence_view_with_branch(
                ds.dataset.store(),
                &sparkles::history::At::Head,
                &preparation,
            )
            .map_err(engine)?;
        let policy = evidence::with_policy_claims(&view, name, access.clone(), &control, |pc| {
            Ok(pc.policy())
        })
        .map_err(engine)?;
        let recorded = evidence::recorded_settings(&view).map_err(engine)?;
        let Some(p) = recorded.policies.get(name) else {
            return Err(engine(evidence::claims::unknown_policy(name)));
        };
        let output = policy.output.as_str().to_string();
        let excluded: BTreeSet<String> = recorded
            .source_records
            .iter()
            .chain(&recorded.proposals)
            .chain(&recorded.decisions)
            .chain(&recorded.supports)
            .chain(&recorded.identity)
            .chain(&recorded.projections)
            .filter(|g| **g != output)
            .cloned()
            .collect();
        let readable = |g: &str| {
            let t = oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(g));
            access.as_ref().is_none_or(|a| a.readable(Some(&t)))
        };
        let mut footprint: Vec<String> = vec![p.decisions.clone(), p.supports.clone()];
        footprint.extend(recorded.proposals.iter().filter(|g| readable(g)).cloned());
        footprint.sort();
        footprint.dedup();
        let recorded_proposals = recorded.proposals.clone();
        let c = view.configuration();
        let policy = RunPolicy {
            name: name.to_string(),
            iri: policy.iri.as_str().to_string(),
            version: policy.version,
            output,
            decisions: p.decisions.clone(),
            supports: p.supports.clone(),
            configuration: format!("{}/{}", c.lineage_id(), c.generation()),
        };
        Ok(Some(Evidence {
            policy,
            access,
            check: EvidenceCheck {
                dataset: main.dataset.clone(),
                recorded,
            },
            view,
            excluded,
            footprint,
            proposals: recorded_proposals,
            control,
            limits,
            pairs: Vec::new(),
            by_claim: HashMap::new(),
            triples: HashMap::new(),
            links: 0,
            spent: 0,
        }))
    }

    /// The snapshot the run reads throughout.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.view.shared_snapshot().clone()
    }

    /// The check a recheck repeats.
    pub fn check(&self) -> EvidenceCheck {
        self.check.clone()
    }

    /// Charge `bytes` to the run's allowance before they are kept.
    pub fn charge(&mut self, bytes: u64) -> Result<(), RunError> {
        let spent = self.spent.saturating_add(bytes);
        if spent > self.limits.bytes {
            return Err(incomplete(format!(
                "the run's evidence and citations need more than {} bytes",
                self.limits.bytes
            )));
        }
        self.spent = spent;
        Ok(())
    }

    /// Charge `n` references to pairs.
    pub fn charge_refs(&mut self, n: usize) -> Result<(), RunError> {
        self.charge((n as u64).saturating_mul(REF_BYTES))
    }

    /// The position of a pair, kept on first use.
    pub fn intern(&mut self, claim: &str, decision: &str) -> Result<u32, RunError> {
        if let Some(&i) = self.by_claim.get(claim)
            && self.pairs[i as usize].1 == decision
        {
            return Ok(i);
        }
        if self.pairs.len() >= self.limits.occurrences {
            return Err(incomplete(format!(
                "the run reads more than {} claim occurrences",
                self.limits.occurrences
            )));
        }
        self.charge(claim.len() as u64 + decision.len() as u64 + 2 * ENTRY_BYTES)?;
        let i = self.pairs.len() as u32;
        self.pairs.push((claim.to_string(), decision.to_string()));
        self.by_claim.insert(claim.to_string(), i);
        Ok(i)
    }

    /// The pair at position `i`.
    pub fn pair(&self, i: u32) -> &(String, String) {
        &self.pairs[i as usize]
    }

    /// The position of a pair the run read, when it read it.
    pub fn position(&self, claim: &str, decision: &str) -> Option<u32> {
        self.by_claim
            .get(claim)
            .copied()
            .filter(|&i| self.pairs[i as usize].1 == decision)
    }

    /// Read the claims of the triples of the output graph among `triples` that the run
    /// has not read yet, in one evidence read. A triple counts a claim of the policy's
    /// live acceptance supports that project it when the principal may see the claim's
    /// occurrence and it is accepted, with its current decision.
    pub fn read(&mut self, triples: &[TripleKey]) -> Result<(), RunError> {
        let todo: Vec<(TripleKey, oxrdf::Triple)> = {
            let mut seen = BTreeSet::new();
            triples
                .iter()
                .filter(|k| !self.triples.contains_key(*k) && seen.insert((*k).clone()))
                .filter_map(|k| triple_of(k).map(|t| (k.clone(), t)))
                .collect()
        };
        // a key that is no triple of the output graph, such as one with a blank or
        // literal subject, has no claim
        for k in triples {
            if !self.triples.contains_key(k) && triple_of(k).is_none() {
                self.triples.insert(k.clone(), Vec::new());
            }
        }
        if todo.is_empty() {
            return Ok(());
        }
        let found: Vec<Vec<(String, String)>> = evidence::with_policy_claims(
            &self.view,
            &self.policy.name,
            self.access.clone(),
            &self.control,
            |pc| {
                let mut out = Vec::with_capacity(todo.len());
                for (_, t) in &todo {
                    let claims = pc
                        .supports(std::slice::from_ref(t))?
                        .pop()
                        .unwrap_or_default();
                    let mut pairs = Vec::new();
                    for o in pc.visible(&claims)? {
                        if o.state != ClaimState::Accepted {
                            continue;
                        }
                        if let Some(d) = o.decision {
                            pairs.push((o.claim.as_str().to_string(), d.as_str().to_string()));
                        }
                    }
                    pairs.sort();
                    out.push(pairs);
                }
                Ok(out)
            },
        )
        .map_err(engine)?;
        for ((k, _), pairs) in todo.into_iter().zip(found) {
            if self.links + pairs.len() > self.limits.links {
                return Err(incomplete(format!(
                    "the run links its triples to more than {} claim occurrences",
                    self.limits.links
                )));
            }
            self.links += pairs.len();
            self.charge(
                (k.0.len() + k.1.len() + k.2.len()) as u64
                    + ENTRY_BYTES
                    + pairs.len() as u64 * REF_BYTES,
            )?;
            let mut at = Vec::with_capacity(pairs.len());
            for (c, d) in &pairs {
                at.push(self.intern(c, d)?);
            }
            at.sort_unstable();
            at.dedup();
            self.triples.insert(k, at);
        }
        Ok(())
    }

    /// The pairs of a triple the run read, empty when it has no claim.
    pub fn claims(&self, k: &TripleKey) -> &[u32] {
        self.triples.get(k).map_or(&[], Vec::as_slice)
    }

    /// Refuse an input with more claim occurrences behind it than the bound allows.
    pub fn check_input(&self, community: &str, n: usize) -> Result<(), RunError> {
        if n > self.limits.input {
            return Err(incomplete(format!(
                "more than {} visible claim occurrences lie behind the input of community {community}",
                self.limits.input
            )));
        }
        Ok(())
    }

    /// Which of the recorded pairs `old` have ended: the claim is not accepted under the
    /// policy at the run's view and the principal's access, its decision is no longer
    /// the claim's current one, or the claim is no longer among the claims the run reads
    /// for its proposition. `present` says which propositions the principal's view of
    /// the output graph holds.
    pub fn ended(
        &mut self,
        old: &[(String, String)],
        present: &dyn Fn(&[oxrdf::Triple]) -> Result<Vec<bool>, RunError>,
    ) -> Result<Vec<bool>, RunError> {
        let mut out = vec![true; old.len()];
        for (chunk_at, chunk) in old.chunks(CHECK_CHUNK).enumerate() {
            let claims: Vec<oxrdf::NamedNode> = chunk
                .iter()
                .map(|(c, _)| oxrdf::NamedNode::new_unchecked(c.as_str()))
                .collect();
            let states = evidence::with_policy_claims(
                &self.view,
                &self.policy.name,
                self.access.clone(),
                &self.control,
                |pc| pc.states(&claims),
            )
            .map_err(engine)?;
            // the propositions of the pairs that are still accepted with their decision
            let mut candidates: Vec<(usize, oxrdf::Triple)> = Vec::new();
            for (k, (s, (_, d))) in states.into_iter().zip(chunk).enumerate() {
                let Some(o) = s else { continue };
                if o.state == ClaimState::Accepted
                    && o.decision.as_ref().is_some_and(|x| x.as_str() == d)
                {
                    candidates.push((k, o.proposition));
                }
            }
            let props: Vec<oxrdf::Triple> = candidates.iter().map(|(_, t)| t.clone()).collect();
            let here = present(&props)?;
            let keys: Vec<TripleKey> = candidates
                .iter()
                .zip(&here)
                .filter(|(_, h)| **h)
                .map(|((_, t), _)| key_of(t))
                .collect();
            self.read(&keys)?;
            for ((k, t), h) in candidates.iter().zip(here) {
                if !h {
                    continue;
                }
                let (c, d) = &chunk[*k];
                let now = self.position(c, d);
                if now.is_some_and(|i| self.claims(&key_of(t)).contains(&i)) {
                    out[chunk_at * CHECK_CHUNK + k] = false;
                }
            }
        }
        Ok(out)
    }
}

/// A stable digest of a fact read under a policy: the triple, and for a triple with
/// claims the policy's IRI and version and the sorted pairs of claim and current
/// decision, so that a changed support or decision counts as a changed fact.
pub fn fact_digest(f: &super::report::Fact, policy: &RunPolicy, pairs: &[(String, String)]) -> u64 {
    use sha2::{Digest, Sha256};
    if pairs.is_empty() {
        return super::build::fact_digest(f);
    }
    let mut h = Sha256::new();
    for part in [&f.s, &f.p, &f.o, &policy.iri] {
        h.update((part.len() as u64).to_be_bytes());
        h.update(part.as_bytes());
    }
    h.update(u64::from(policy.version).to_be_bytes());
    let mut sorted: Vec<&(String, String)> = pairs.iter().collect();
    sorted.sort();
    for (c, d) in sorted {
        for part in [c, d] {
            h.update((part.len() as u64).to_be_bytes());
            h.update(part.as_bytes());
        }
    }
    let d: [u8; 32] = h.finalize().into();
    u64::from_be_bytes(d[..8].try_into().expect("8 bytes"))
}
