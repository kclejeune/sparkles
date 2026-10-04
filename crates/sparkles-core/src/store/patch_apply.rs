//! Applying an RDF Patch to a store ([`Store::apply_patch`]), as Fuseki's `patch`
//! operation applies one: the whole patch is one write transaction, and one commit of
//! kind `patch`.
//!
//! * `TX`, `TC` and `Z` are markers. `TA` anywhere aborts the whole patch, and nothing is
//!   applied.
//! * `A` and `D` rows apply in order through the transaction, so an add followed by a
//!   delete of the same quad leaves it absent.
//! * `PA` and `PD` change the dataset's prefix map once the data has committed. A graph
//!   term on them does not narrow the change, since a dataset has one prefix map.
//! * `H prev` naming a commit of this dataset is a precondition: the patch applies only
//!   when that commit is the head. The other headers are read and ignored, except
//!   `message`, which gives the commit a message when the write has none.
//! * A blank node label is local to the patch, as in `INSERT DATA`, unless the patch's
//!   leading headers name a commit of this dataset in `prev`. Then a label in the stored
//!   form `b<hex>` names the stored node with that number when the dataset has
//!   allocated it, which is the case of a patch read from this dataset's own diff or
//!   change feed.

use super::{MAX_PREFIX_IRI_BYTES, MAX_PREFIX_NAME_BYTES};
use super::{Store, WriteTxn, bnode_for, parse_bnode_label, valid_prefix_name};
use crate::commit::{CommitKind, Receipt};
use crate::error::{Error, Result};
use crate::guard::{Precondition, WriteOptions};
use crate::id::{self, Id};
use crate::patch::{PatchError, PatchErrorKind, PatchReader, PatchRow};
use crate::sparql::cdt;
use oxrdf::{BlankNode, GraphName, NamedOrBlankNode, Quad, Term, Triple};
use std::collections::HashMap;
use std::io::Read;

/// How to apply a patch.
#[derive(Clone, Debug, Default)]
pub struct PatchOptions {
    /// the write's options: validation, message, deadline, graphs, dry run
    pub write: WriteOptions,
    /// the binary (RDF Thrift) form instead of the text form
    pub binary: bool,
}

/// What applying a patch did.
#[derive(Clone, Debug)]
pub struct PatchOutcome {
    /// the commit, or the unchanged head when the patch changed no data or aborted
    pub receipt: Receipt,
    /// the rows read (up to a `TA` that aborted the patch)
    pub rows: u64,
    /// `A` rows that added a quad
    pub inserted: u64,
    /// `D` rows that removed a quad
    pub deleted: u64,
    /// a `TA` row aborted the patch: nothing was applied
    pub aborted: bool,
    /// the patch named a commit of this dataset in `prev`, and it was the head
    pub prev_checked: bool,
    /// `PA` rows that changed the prefix map
    pub prefixes_set: u64,
    /// `PD` rows that removed a prefix
    pub prefixes_removed: u64,
}

/// A prefix change of a `PA` or `PD` row.
enum PrefixOp {
    Set(String, String),
    Remove(String),
}

/// The commit `n` of dataset `id` that a commit IRI (`urn:uuid:<id>#commit:<n>`) names.
pub fn parse_commit_iri(iri: &str) -> Option<(uuid::Uuid, u64)> {
    let rest = iri.strip_prefix("urn:uuid:")?;
    let (id, seq) = rest.split_once("#commit:")?;
    Some((uuid::Uuid::parse_str(id).ok()?, seq.parse().ok()?))
}

/// The blank nodes a patch names.
struct Labels {
    /// stored labels (`b<hex>`) name stored nodes
    stored: bool,
    local: HashMap<String, Id>,
}

impl Labels {
    fn get(&self, txn: &WriteTxn<'_>, label: &str) -> Option<Id> {
        if self.stored
            && let Some(id) = parse_bnode_label(label)
            && txn.bnode_allocated(id)
        {
            return Some(id);
        }
        self.local.get(label).copied()
    }

    fn get_or_new(&mut self, txn: &mut WriteTxn<'_>, label: &str) -> Id {
        if let Some(id) = self.get(txn, label) {
            return id;
        }
        let id = txn.new_bnode();
        self.local.insert(label.to_string(), id);
        id
    }
}

/// `t` with each blank node replaced by the stored label of the node `f` gives it, or
/// `None` when `f` gives none.
fn relabel(t: &Term, f: &mut impl FnMut(&BlankNode) -> Option<Id>) -> Option<Term> {
    Some(match t {
        Term::BlankNode(b) => Term::BlankNode(bnode_for(f(b)?)),
        Term::Triple(tr) => {
            let s = match &tr.subject {
                NamedOrBlankNode::BlankNode(b) => NamedOrBlankNode::BlankNode(bnode_for(f(b)?)),
                s => s.clone(),
            };
            let o = relabel(&tr.object, f)?;
            Term::Triple(Box::new(Triple::new(s, tr.predicate.clone(), o)))
        }
        Term::Literal(l) if cdt::may_name_bnodes(l) => {
            let mut ok = true;
            let l = cdt::relabel_literal(l, &mut |b| match f(&BlankNode::new_unchecked(b)) {
                Some(id) => id::bnode_label(id.payload()),
                None => {
                    ok = false;
                    b.to_string()
                }
            });
            match l {
                Some(l) if ok => Term::Literal(l),
                Some(_) => return None,
                None => t.clone(),
            }
        }
        t => t.clone(),
    })
}

fn subject_term(s: &NamedOrBlankNode) -> Term {
    match s {
        NamedOrBlankNode::NamedNode(n) => Term::NamedNode(n.clone()),
        NamedOrBlankNode::BlankNode(b) => Term::BlankNode(b.clone()),
    }
}

fn graph_term(g: &GraphName) -> Option<Term> {
    match g {
        GraphName::DefaultGraph => None,
        GraphName::NamedNode(n) => Some(Term::NamedNode(n.clone())),
        GraphName::BlankNode(b) => Some(Term::BlankNode(b.clone())),
    }
}

/// The ids of an `A` row's quad, making new blank nodes for new labels.
fn encode_add(txn: &mut WriteTxn<'_>, labels: &mut Labels, q: &Quad) -> Result<[Id; 4]> {
    let mut one = |txn: &mut WriteTxn<'_>, t: &Term| -> Result<Id> {
        if let Term::BlankNode(b) = t {
            return Ok(labels.get_or_new(txn, b.as_str()));
        }
        // give every label of a triple term or a composite literal its node first,
        // then relabel
        if matches!(t, Term::Triple(_)) || matches!(t, Term::Literal(l) if cdt::may_name_bnodes(l))
        {
            let mut names = Vec::new();
            collect_labels(t, &mut names);
            for l in &names {
                labels.get_or_new(txn, l);
            }
            let t = relabel(t, &mut |b| labels.get(txn, b.as_str())).expect("labels given");
            return txn.intern(&t);
        }
        txn.intern(t)
    };
    let s = one(txn, &subject_term(&q.subject))?;
    let p = one(txn, &Term::NamedNode(q.predicate.clone()))?;
    let o = one(txn, &q.object)?;
    let g = match graph_term(&q.graph_name) {
        None => Id::DEFAULT_GRAPH,
        Some(g) => one(txn, &g)?,
    };
    Ok([s, p, o, g])
}

fn collect_labels(t: &Term, out: &mut Vec<String>) {
    match t {
        Term::BlankNode(b) => out.push(b.as_str().to_string()),
        Term::Triple(tr) => {
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                out.push(b.as_str().to_string());
            }
            collect_labels(&tr.object, out);
        }
        Term::Literal(l) => {
            cdt::relabel_literal(l, &mut |b| {
                out.push(b.to_string());
                b.to_string()
            });
        }
        _ => {}
    }
}

/// The id of a term of a `D` row that the store has, or `Id::UNDEF`.
fn lookup(txn: &WriteTxn<'_>, labels: &Labels, t: &Term) -> Id {
    let found = match t {
        Term::BlankNode(b) => labels.get(txn, b.as_str()),
        t => relabel(t, &mut |b| labels.get(txn, b.as_str()))
            .and_then(|t| id::inline_id(&t).or_else(|| txn.lookup_key(&id::term_key(&t)))),
    };
    found.unwrap_or(Id::UNDEF)
}

/// A term error of row `row`: an engine error raised while a row's terms were stored.
fn row_error(e: Error, row: u64) -> Error {
    match e {
        Error::Invalid(msg) => {
            let mut p = PatchError::new(PatchErrorKind::Term, msg);
            p.row = Some(row);
            Error::Patch(Box::new(p))
        }
        e => e,
    }
}

impl Store {
    /// Apply an RDF Patch, in the text or the binary form, as one write transaction, as
    /// Fuseki applies one. A patch that changes data makes one commit of kind `patch`.
    /// A failed patch applies nothing: a syntax or term error is [`Error::Patch`], and so
    /// is a `prev` header that names a commit of this dataset other than the head.
    pub fn apply_patch(&self, r: impl Read, o: &PatchOptions) -> Result<PatchOutcome> {
        let mut reader = PatchReader::new(r, o.binary);
        let mut opts = o.write.clone();
        // the leading headers decide the precondition, the message and the blank nodes
        let mut first = None;
        let mut prev_checked = false;
        let mut stored = false;
        while let Some(row) = reader.next_row()? {
            let PatchRow::Header(name, value) = &row else {
                first = Some(row);
                break;
            };
            match name.as_str() {
                "prev" => {
                    if let Some(n) = self.own_commit(value) {
                        prev_checked = true;
                        stored = true;
                        opts.precondition = Some(prev_precondition(
                            opts.precondition.take(),
                            value_iri(value),
                            n,
                        ));
                    }
                }
                "message" => header_message(&mut opts, value)?,
                _ => {}
            }
        }
        let mut txn = self.try_write_with(CommitKind::Patch, opts)?;
        let mut labels = Labels {
            stored,
            local: HashMap::new(),
        };
        let mut rows = reader.rows();
        let (mut inserted, mut deleted) = (0u64, 0u64);
        let mut prefixes = Vec::new();
        // adds are held while no delete has come, so that a patch of adds only can
        // take the bulk path
        let mut adds: Vec<[Id; 4]> = Vec::new();
        let mut deleting = false;
        let mut aborted = false;
        let mut next = first;
        loop {
            let row = match next.take() {
                Some(r) => r,
                None => match reader.next_row()? {
                    Some(r) => {
                        rows = reader.rows();
                        r
                    }
                    None => break,
                },
            };
            if rows.is_multiple_of(4096) {
                txn.opts.check()?;
            }
            match row {
                PatchRow::Header(name, value) => match name.as_str() {
                    "prev" => {
                        if let Some(n) = self.own_commit(&value) {
                            prev_checked = true;
                            let head = txn.guard.head.seq;
                            if n != head {
                                return Err(
                                    PatchError::prev_mismatch(&value_iri(&value), n, head).into()
                                );
                            }
                        }
                    }
                    "message" if txn.opts.message.is_none() => {
                        header_message(&mut txn.opts, &value)?
                    }
                    _ => {}
                },
                PatchRow::Begin | PatchRow::Commit | PatchRow::Segment => {}
                PatchRow::Abort => {
                    aborted = true;
                    break;
                }
                PatchRow::Add(q) => {
                    check_graph(&txn, &q)?;
                    let ids =
                        encode_add(&mut txn, &mut labels, &q).map_err(|e| row_error(e, rows))?;
                    if deleting {
                        inserted += txn.insert(ids)? as u64;
                    } else {
                        adds.push(ids);
                    }
                }
                PatchRow::Delete(q) => {
                    check_graph(&txn, &q)?;
                    if !deleting {
                        deleting = true;
                        for ids in std::mem::take(&mut adds) {
                            inserted += txn.insert(ids)? as u64;
                        }
                    }
                    let ids = [
                        lookup(&txn, &labels, &subject_term(&q.subject)),
                        lookup(&txn, &labels, &Term::NamedNode(q.predicate.clone())),
                        lookup(&txn, &labels, &q.object),
                        match graph_term(&q.graph_name) {
                            None => Id::DEFAULT_GRAPH,
                            Some(g) => lookup(&txn, &labels, &g),
                        },
                    ];
                    if ids.contains(&Id::UNDEF) {
                        // a quad of terms the store lacks deletes nothing, but protections
                        // are checked all the same, as for DELETE DATA
                        let graph = graph_term(&q.graph_name);
                        txn.check_requested(ids, q.predicate.as_str(), graph.as_ref())?;
                    } else {
                        deleted += txn.delete(ids)? as u64;
                    }
                }
                PatchRow::PrefixSet(p, iri) => {
                    self.check_prefix_row(&txn, &p, Some(&iri))
                        .map_err(|e| row_error(e, rows))?;
                    prefixes.push(PrefixOp::Set(p, iri));
                }
                PatchRow::PrefixRemove(p) => {
                    self.check_prefix_row(&txn, &p, None)
                        .map_err(|e| row_error(e, rows))?;
                    prefixes.push(PrefixOp::Remove(p));
                }
            }
        }
        if aborted {
            let head = txn.guard.head;
            drop(txn);
            return Ok(PatchOutcome {
                receipt: Receipt {
                    dataset_id: self.dataset_id,
                    committed: false,
                    commit: head,
                    validation: None,
                    annotation: self.annotation(head.seq).unwrap_or_default(),
                },
                rows,
                inserted: 0,
                deleted: 0,
                aborted: true,
                prev_checked,
                prefixes_set: 0,
                prefixes_removed: 0,
            });
        }
        // the prefix map the patch leaves must fit, before anything is written
        self.fold_prefixes(&prefixes, true)?;
        let bulk = !deleting && adds.len() as u64 >= self.opts.bulk_threshold;
        if bulk {
            txn.insert_bulk(std::mem::take(&mut adds))?;
        } else {
            for ids in std::mem::take(&mut adds) {
                inserted += txn.insert(ids)? as u64;
            }
        }
        let receipt = txn.commit()?;
        if bulk && receipt.committed {
            inserted = receipt.commit.inserted;
        }
        let (prefixes_set, prefixes_removed) = self.fold_prefixes(&prefixes, false)?;
        Ok(PatchOutcome {
            receipt,
            rows,
            inserted,
            deleted,
            aborted: false,
            prev_checked,
            prefixes_set,
            prefixes_removed,
        })
    }

    /// The commit of this dataset a `prev` value names, if it names one.
    fn own_commit(&self, value: &Term) -> Option<u64> {
        let Term::NamedNode(n) = value else {
            return None;
        };
        parse_commit_iri(n.as_str())
            .filter(|(id, _)| *id == self.dataset_id)
            .map(|(_, n)| n)
    }

    /// A `PA` or `PD` row: refused to a write limited to some graphs (which cannot
    /// change the prefixes), and checked as [`Store::set_prefix`] checks a prefix.
    fn check_prefix_row(&self, txn: &WriteTxn<'_>, prefix: &str, iri: Option<&str>) -> Result<()> {
        if txn.opts.graphs.is_some() {
            return Err(Error::NotPermitted(
                "a patch with prefix rows (PA, PD) needs write access to the whole dataset".into(),
            ));
        }
        if prefix.len() > MAX_PREFIX_NAME_BYTES || !valid_prefix_name(prefix) {
            return Err(Error::invalid(format!("invalid prefix name {prefix:?}")));
        }
        if let Some(iri) = iri {
            if iri.len() > MAX_PREFIX_IRI_BYTES {
                return Err(Error::invalid(format!(
                    "prefix IRI longer than {MAX_PREFIX_IRI_BYTES} bytes"
                )));
            }
            oxrdf::NamedNode::new(iri)
                .map_err(|e| Error::invalid(format!("invalid IRI {iri:?}: {e}")))?;
        }
        Ok(())
    }

    /// Apply prefix changes in order to the prefix map, or only check that the result
    /// fits within [`StoreOptions::max_prefixes`](super::StoreOptions) (`check`).
    /// Returns the prefixes set and removed. After the data has committed, a new prefix
    /// that another request's change has left no room for is skipped with a warning.
    fn fold_prefixes(&self, ops: &[PrefixOp], check: bool) -> Result<(u64, u64)> {
        if ops.is_empty() {
            return Ok((0, 0));
        }
        let mut cur = self.prefixes.lock();
        let mut next = cur.clone();
        let (mut set, mut removed) = (0, 0);
        for op in ops {
            match op {
                PrefixOp::Set(p, iri) => {
                    if next.get(p) == Some(iri) {
                        continue;
                    }
                    if !next.contains_key(p) && self.prefixes_full(next.len()) {
                        if check {
                            return Err(Error::invalid(format!(
                                "the patch would give the dataset more than {} prefixes",
                                self.opts.max_prefixes
                            )));
                        }
                        tracing::warn!(target: "sparkles::store::patch_apply", "the patch's prefix {p:?} was not added: no room left");
                        continue;
                    }
                    next.insert(p.clone(), iri.clone());
                    set += 1;
                }
                PrefixOp::Remove(p) => {
                    if next.remove(p).is_some() {
                        removed += 1;
                    }
                }
            }
        }
        if !check && next != *cur {
            self.save_prefixes(&next)?;
            *cur = next;
        }
        Ok((set, removed))
    }
}

/// A write limited to some graphs may change a row's graph: checked before the row's
/// terms are looked up, so that the answer never depends on the data.
fn check_graph(txn: &WriteTxn<'_>, q: &Quad) -> Result<()> {
    let Some(access) = &txn.opts.graphs else {
        return Ok(());
    };
    let g = graph_term(&q.graph_name);
    if access.writable(g.as_ref()) {
        Ok(())
    } else {
        Err(crate::access::GraphAccess::refused(g.as_ref()))
    }
}

fn value_iri(v: &Term) -> String {
    match v {
        Term::NamedNode(n) => n.as_str().to_string(),
        t => t.to_string(),
    }
}

/// The write's precondition and the check that commit `n` is the head.
fn prev_precondition(before: Option<Precondition>, prev: String, n: u64) -> Precondition {
    Precondition::new(move |head| {
        if let Some(b) = &before {
            b.check(head)?;
        }
        if head.commit == n {
            Ok(())
        } else {
            Err(PatchError::prev_mismatch(&prev, n, head.commit).into())
        }
    })
}

/// A `message` header with a string value gives the commit its message.
fn header_message(opts: &mut WriteOptions, value: &Term) -> Result<()> {
    if opts.message.is_some() {
        return Ok(());
    }
    if let Term::Literal(l) = value
        && l.datatype() == oxrdf::vocab::xsd::STRING
    {
        opts.message = crate::annotations::validate_message(l.value())?;
    }
    Ok(())
}
