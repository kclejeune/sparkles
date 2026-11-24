//! Named-domain consequences of two original EL chains through the literal top role.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::model::{ClassExpression, ObjectProperty};
use crate::vocab;

use super::index::{Ex, Index, Interner, Role};
use super::numeric::Budget;
use super::saturate::Sink;
use super::{AxiomKind, Error, ObjectPropertyExpression, SubObjectPropertyExpression};
#[cfg(test)]
use super::{
    admission_test::{Point, hit},
    top_chain_diagnostic::Probe,
};
#[cfg(test)]
use std::sync::atomic::Ordering::Relaxed;

/// Bounds both sorts and the transient row counts. A decline publishes no rules.
const PLAN_ROWS: usize = 4096;
type Row = (Option<Role>, Ex);

pub(super) struct Rules {
    rows: Box<[Row]>,
    all: usize,
    #[cfg(test)]
    probe: Option<Arc<Probe>>,
}

impl Rules {
    pub fn bytes(&self) -> usize {
        self.rows.len() * size_of::<Row>()
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Observe a completed world while it still owns its private state.
    #[cfg(test)]
    pub fn world_ready(&self, class: Ex, reported: usize) {
        if let Some(observe) = self
            .probe
            .as_ref()
            .and_then(|probe| probe.world_ready.as_ref())
        {
            observe(class, reported);
        }
    }

    /// Thing holds once per live context. A newly stored Back proves an ordinary
    /// existential at its source, regardless of when the target takes Thing.
    pub fn apply(&self, ix: &Index, edge: Option<Role>, source: Ex, out: &mut impl Sink) {
        let rows = if edge.is_some() {
            &self.rows[self.all..]
        } else {
            &self.rows[..self.all]
        };
        if rows.is_empty() {
            return;
        }
        #[cfg(test)]
        let probe = self.probe.as_deref();
        #[cfg(test)]
        let tests = probe.map(|probe| {
            let (events, tests) = if edge.is_some() {
                (&probe.back_events, &probe.back_tests)
            } else {
                (&probe.top_events, &probe.top_tests)
            };
            events.fetch_add(1, Relaxed);
            tests
        });
        let mut sent = None;
        for &(prefix, domain) in rows {
            #[cfg(test)]
            hit(Point::TopChainScan);
            if !out.reserve_data(0) {
                break;
            }
            #[cfg(test)]
            if let Some(tests) = tests {
                tests.fetch_add(1, Relaxed);
            }
            if edge.is_none_or(|r| ix.roles.sub(r, prefix.expect("predecessor row")))
                && sent != Some(domain)
            {
                if !out.top_chain_sub(
                    source,
                    domain,
                    #[cfg(test)]
                    probe,
                ) {
                    break;
                }
                sent = Some(domain);
            }
        }
    }
}

pub(super) fn shape<'a>(
    k: &'a AxiomKind,
    why: &str,
) -> Option<(&'a ObjectProperty, &'a ObjectProperty)> {
    if why == super::index::TOP_CHAIN
        && let AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(path),
            ObjectPropertyExpression::Property(head),
        ) = k
        && let [
            ObjectPropertyExpression::Property(prefix),
            ObjectPropertyExpression::Property(last),
        ] = path.as_slice()
        && last.as_str() == vocab::OWL_TOP_OBJECT_PROPERTY
    {
        Some((prefix, head))
    } else {
        None
    }
}

/// The partial map has original, centrally range-checked chain entries. The original
/// axiom slice supplies domain provenance. The final index supplies the role closure.
pub(super) fn plan(
    axioms: &[&AxiomKind],
    partial: &BTreeMap<&AxiomKind, &'static str>,
    it: &Interner,
    ix: &Index,
    budget: &Arc<Budget>,
    base: usize,
    #[cfg(test)] probe: Option<&Arc<Probe>>,
) -> Result<Option<Rules>, Error> {
    let memory = || Error::Memory {
        limit: budget.memory_limit(),
        used: usize::MAX,
    };
    let visit = || {
        #[cfg(test)]
        if let Some(probe) = probe {
            probe.build_visits.fetch_add(1, Relaxed);
        }
        budget.poll()
    };
    let domain = |k: &AxiomKind| match k {
        AxiomKind::ObjectPropertyDomain(
            ObjectPropertyExpression::Property(g),
            ClassExpression::Class(d),
        ) => Some((
            it.object_role_id(g).expect("normalized domain role"),
            it.class_ex(d).expect("normalized domain class"),
        )),
        _ => None,
    };
    let mut count = 0;
    for &k in axioms {
        visit()?;
        count += usize::from(domain(k).is_some());
        if count > PLAN_ROWS {
            return Ok(None);
        }
    }
    if count == 0 {
        return Ok(None);
    }
    #[cfg(test)]
    hit(Point::TopChainScratch);
    budget.baseline(base)?;
    let scratch = budget.reserve(
        count
            .checked_mul(size_of::<(Role, Ex)>())
            .ok_or_else(memory)?,
    )?;
    let mut domains = Vec::new();
    domains.try_reserve_exact(count).map_err(|_| memory())?;
    if domains.capacity() > count {
        return Err(memory());
    }
    for &k in axioms {
        visit()?;
        domains.extend(domain(k));
    }
    domains.sort_unstable();
    domains.dedup();
    let matches = |row: &mut dyn FnMut(Row)| -> Result<usize, Error> {
        let mut n = 0;
        for (&k, &why) in partial {
            visit()?;
            let Some((prefix, head)) = shape(k, why) else {
                continue;
            };
            let r = it.object_role_id(prefix).expect("normalized chain operand");
            let head = it.object_role_id(head).expect("normalized chain head");
            let prefix = if prefix.as_str() == vocab::OWL_TOP_OBJECT_PROPERTY {
                None
            } else if ix.is_universal(r) || ix.roles.is_empty_role(r) {
                continue;
            } else {
                Some(r)
            };
            for &g in &ix.roles.sup[head as usize] {
                visit()?;
                let start = domains.partition_point(|&(role, _)| role < g);
                for &(_, d) in domains[start..].iter().take_while(|&&(role, _)| role == g) {
                    visit()?;
                    #[cfg(test)]
                    if let Some(probe) = probe {
                        probe.matches.fetch_add(1, Relaxed);
                    }
                    n += 1;
                    if n > PLAN_ROWS {
                        return Ok(n);
                    }
                    row((prefix, d));
                }
            }
        }
        Ok(n)
    };
    let count = matches(&mut |_| {})?;
    if count == 0 || count > PLAN_ROWS {
        return Ok(None);
    }
    let matched = budget.reserve(count.checked_mul(size_of::<Row>()).ok_or_else(memory)?)?;
    let mut rows = Vec::new();
    rows.try_reserve_exact(count).map_err(|_| memory())?;
    if rows.capacity() > count {
        return Err(memory());
    }
    // Both passes read the same immutable inputs.
    let filled = matches(&mut |row| rows.push(row))?;
    debug_assert_eq!(filled, count);
    rows.sort_unstable_by_key(|&(prefix, domain)| (prefix.is_some(), domain, prefix));
    rows.dedup();
    let all = rows.partition_point(|r| r.0.is_none());
    #[cfg(test)]
    hit(Point::TopChainRetained);
    let len = rows.len();
    let retained = budget.reserve(len.checked_mul(size_of::<Row>()).ok_or_else(memory)?)?;
    let mut final_rows = Vec::new();
    final_rows.try_reserve_exact(len).map_err(|_| memory())?;
    if final_rows.capacity() != len {
        return Err(memory());
    }
    final_rows.extend_from_slice(&rows);
    let rules = Rules {
        rows: final_rows.into_boxed_slice(),
        all,
        #[cfg(test)]
        probe: probe.cloned(),
    };
    retained.retain(Some(rules.bytes()))?;
    drop(rows);
    drop(matched);
    drop(domains);
    drop(scratch);
    Ok(Some(rules))
}
