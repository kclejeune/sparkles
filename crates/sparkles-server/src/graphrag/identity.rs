//! Stable community identities across builds (spec C22 §3.1).
//!
//! A new community inherits the IRI of a previous community at the same depth when their
//! member sets have a Jaccard similarity of at least 0.5, and each IRI is inherited at
//! most once. The matching is a deterministic greedy pass over every qualifying
//! old/new pair at a depth, in this order:
//!
//! 1. decreasing similarity, compared exactly as rationals by cross-multiplication;
//! 2. the old IRI, ascending in code-point order;
//! 3. the new community's member digest (a versioned SHA-256 of its sorted member
//!    terms), ascending;
//! 4. the new community's sorted member terms, compared in full, for a digest tie.
//!
//! A pair is accepted when neither side has matched yet. Communities left unmatched get
//! a new IRI `urn:x-sparkles:graphrag:{ds}:{depth}:{uuid}`. The result does not depend
//! on the order in which either side is given.

use sha2::{Digest, Sha256};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

/// The tag of the member digest's format.
pub const MEMBER_DIGEST_VERSION: &str = "c22-members-v1";

/// A community of the previous build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Previous {
    pub depth: u32,
    pub iri: String,
    /// canonical N-Triples terms, sorted and without repeats
    pub members: Vec<String>,
}

/// A community of the new build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fresh {
    pub depth: u32,
    /// canonical N-Triples terms, sorted and without repeats
    pub members: Vec<String>,
}

/// The identity given to a new community.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assigned {
    pub iri: String,
    /// the previous IRI it inherited, the same as `iri`, or `None` for a new IRI
    pub inherited: Option<String>,
}

/// The versioned digest of a sorted member list.
pub fn member_digest(members: &[String]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(MEMBER_DIGEST_VERSION.as_bytes());
    h.update([0u8]);
    for m in members {
        h.update((m.len() as u64).to_be_bytes());
        h.update(m.as_bytes());
    }
    h.finalize().into()
}

/// The IRI of a new community. The dataset name is percent-encoded so that the IRI stays
/// valid whatever the name holds.
pub fn new_iri(ds: &str, depth: u32, uuid: &uuid::Uuid) -> String {
    let mut enc = String::new();
    for b in ds.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            enc.push(b as char);
        } else {
            enc.push_str(&format!("%{b:02X}"));
        }
    }
    format!(
        "{}{enc}:{depth}:{}",
        super::settings::GUARDED_PREFIX,
        uuid.hyphenated()
    )
}

/// `inter / union`, held exactly.
#[derive(Clone, Copy, Debug)]
struct Ratio {
    inter: u64,
    union: u64,
}

impl Ratio {
    /// At least one half.
    fn qualifies(&self) -> bool {
        self.union > 0 && u128::from(self.inter) * 2 >= u128::from(self.union)
    }

    /// Compares exactly by cross-multiplication, in `u128`, which cannot overflow for
    /// `u64` operands.
    fn cmp(&self, o: &Ratio) -> Ordering {
        (u128::from(self.inter) * u128::from(o.union))
            .cmp(&(u128::from(o.inter) * u128::from(self.union)))
    }
}

/// Assign identities to `fresh`, in its order, from `previous`. `mint` gives the UUID of
/// each new IRI in turn.
pub fn assign(
    ds: &str,
    previous: &[Previous],
    fresh: &[Fresh],
    mut mint: impl FnMut() -> uuid::Uuid,
) -> Vec<Assigned> {
    let mut out: Vec<Option<Assigned>> = vec![None; fresh.len()];
    let depths: BTreeSet<u32> = fresh.iter().map(|f| f.depth).collect();
    let digests: Vec<[u8; 32]> = fresh.iter().map(|f| member_digest(&f.members)).collect();
    for depth in depths {
        let olds: Vec<&Previous> = previous.iter().filter(|p| p.depth == depth).collect();
        // members to the old communities that hold them
        let mut holders: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, p) in olds.iter().enumerate() {
            for m in &p.members {
                holders.entry(m.as_str()).or_default().push(i);
            }
        }
        let mut pairs: Vec<(Ratio, usize, usize)> = Vec::new();
        for (j, f) in fresh.iter().enumerate().filter(|(_, f)| f.depth == depth) {
            let mut inter: HashMap<usize, u64> = HashMap::new();
            for m in &f.members {
                if let Some(hs) = holders.get(m.as_str()) {
                    for &i in hs {
                        *inter.entry(i).or_default() += 1;
                    }
                }
            }
            for (i, k) in inter {
                let union = olds[i].members.len() as u64 + f.members.len() as u64 - k;
                let r = Ratio { inter: k, union };
                if r.qualifies() {
                    pairs.push((r, i, j));
                }
            }
        }
        pairs.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| olds[a.1].iri.cmp(&olds[b.1].iri))
                .then_with(|| digests[a.2].cmp(&digests[b.2]))
                .then_with(|| fresh[a.2].members.cmp(&fresh[b.2].members))
        });
        let mut old_used = vec![false; olds.len()];
        for (_, i, j) in pairs {
            if old_used[i] || out[j].is_some() {
                continue;
            }
            old_used[i] = true;
            out[j] = Some(Assigned {
                iri: olds[i].iri.clone(),
                inherited: Some(olds[i].iri.clone()),
            });
        }
    }
    // new IRIs in a fixed order: by depth, then digest, then members
    let mut order: Vec<usize> = (0..fresh.len()).filter(|&j| out[j].is_none()).collect();
    order.sort_by(|&a, &b| {
        fresh[a]
            .depth
            .cmp(&fresh[b].depth)
            .then_with(|| digests[a].cmp(&digests[b]))
            .then_with(|| fresh[a].members.cmp(&fresh[b].members))
    });
    for j in order {
        out[j] = Some(Assigned {
            iri: new_iri(ds, fresh[j].depth, &mint()),
            inherited: None,
        });
    }
    out.into_iter()
        .map(|a| a.expect("every community is assigned"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(xs: &[u32]) -> Vec<String> {
        let mut v: Vec<String> = xs.iter().map(|x| format!("<urn:e:{x:03}>")).collect();
        v.sort();
        v
    }

    fn old(depth: u32, iri: &str, xs: &[u32]) -> Previous {
        Previous {
            depth,
            iri: iri.into(),
            members: terms(xs),
        }
    }

    fn new(depth: u32, xs: &[u32]) -> Fresh {
        Fresh {
            depth,
            members: terms(xs),
        }
    }

    fn counter() -> impl FnMut() -> uuid::Uuid {
        let mut n = 0u128;
        move || {
            n += 1;
            uuid::Uuid::from_u128(n)
        }
    }

    fn inherited(a: &[Assigned]) -> Vec<Option<&str>> {
        a.iter().map(|x| x.inherited.as_deref()).collect()
    }

    #[test]
    fn unchanged_membership_keeps_its_iri() {
        let prev = [old(0, "urn:a", &[1, 2, 3]), old(0, "urn:b", &[4, 5, 6])];
        let a = assign(
            "ds",
            &prev,
            &[new(0, &[4, 5, 6]), new(0, &[1, 2, 3])],
            counter(),
        );
        assert_eq!(inherited(&a), vec![Some("urn:b"), Some("urn:a")]);
    }

    /// One fact added to a cluster keeps the IRI (C22 A3).
    #[test]
    fn a_small_change_keeps_the_iri_and_half_is_the_threshold() {
        let prev = [old(1, "urn:a", &[1, 2, 3, 4])];
        let a = assign("ds", &prev, &[new(1, &[1, 2, 3, 4, 5])], counter());
        assert_eq!(inherited(&a), vec![Some("urn:a")]);
        // exactly one half qualifies: {1,2} vs {1,2,3,4}
        let a = assign("ds", &prev, &[new(1, &[1, 2])], counter());
        assert_eq!(inherited(&a), vec![Some("urn:a")]);
        // just below one half does not: {1,2} vs {1,2,3,4,5}
        let prev5 = [old(1, "urn:a", &[1, 2, 3, 4, 5])];
        let a = assign("ds", &prev5, &[new(1, &[1, 2])], counter());
        assert_eq!(inherited(&a), vec![None]);
    }

    #[test]
    fn depths_do_not_match_each_other() {
        let prev = [old(0, "urn:a", &[1, 2, 3])];
        let a = assign("ds", &prev, &[new(1, &[1, 2, 3])], counter());
        assert_eq!(inherited(&a), vec![None]);
        assert!(a[0].iri.starts_with("urn:x-sparkles:graphrag:ds:1:"));
    }

    #[test]
    fn a_split_gives_the_iri_to_the_larger_part() {
        let prev = [old(0, "urn:a", &[1, 2, 3, 4, 5, 6])];
        // {1..4}: 4/6, {5,6}: 2/6 which does not qualify
        let a = assign(
            "ds",
            &prev,
            &[new(0, &[5, 6]), new(0, &[1, 2, 3, 4])],
            counter(),
        );
        assert_eq!(inherited(&a), vec![None, Some("urn:a")]);
    }

    #[test]
    fn a_merge_inherits_one_iri_only() {
        let prev = [old(0, "urn:b", &[1, 2, 3]), old(0, "urn:a", &[4, 5, 6])];
        // both score 3/6; the lower old IRI wins the tie
        let a = assign("ds", &prev, &[new(0, &[1, 2, 3, 4, 5, 6])], counter());
        assert_eq!(inherited(&a), vec![Some("urn:a")]);
    }

    #[test]
    fn equal_scores_for_one_old_iri_go_by_member_digest() {
        let prev = [old(0, "urn:a", &[1, 2, 3, 4])];
        // {1,2,3} and {2,3,4} both score 3/4
        let x = new(0, &[1, 2, 3]);
        let y = new(0, &[2, 3, 4]);
        let winner = if member_digest(&x.members) < member_digest(&y.members) {
            0
        } else {
            1
        };
        let a = assign("ds", &prev, &[x.clone(), y.clone()], counter());
        assert!(a[winner].inherited.is_some());
        assert!(a[1 - winner].inherited.is_none());
        let b = assign("ds", &prev, &[y, x], counter());
        assert!(b[1 - winner].inherited.is_some());
    }

    #[test]
    fn the_best_score_wins_before_iri_order() {
        let prev = [old(0, "urn:a", &[1, 2, 3, 4]), old(0, "urn:b", &[1, 2, 3])];
        let a = assign("ds", &prev, &[new(0, &[1, 2, 3])], counter());
        assert_eq!(inherited(&a), vec![Some("urn:b")]);
    }

    #[test]
    fn input_order_does_not_change_the_result() {
        let prev = vec![
            old(0, "urn:a", &[1, 2, 3, 4]),
            old(0, "urn:b", &[5, 6, 7]),
            old(0, "urn:c", &[8, 9]),
            old(1, "urn:d", &[1, 2]),
        ];
        let fresh = vec![
            new(0, &[1, 2, 3]),
            new(0, &[2, 3, 4]),
            new(0, &[5, 6, 7, 8]),
            new(0, &[9, 10]),
            new(1, &[1, 2, 3]),
        ];
        let base = assign("ds", &prev, &fresh, counter());
        let key = |fs: &[Fresh], a: &[Assigned]| {
            let mut v: Vec<(Vec<String>, u32, Option<String>, String)> = fs
                .iter()
                .zip(a)
                .map(|(f, a)| {
                    (
                        f.members.clone(),
                        f.depth,
                        a.inherited.clone(),
                        a.iri.clone(),
                    )
                })
                .collect();
            v.sort();
            v
        };
        let mut p2 = prev.clone();
        p2.reverse();
        let mut f2 = fresh.clone();
        f2.reverse();
        let other = assign("ds", &p2, &f2, counter());
        assert_eq!(key(&fresh, &base), key(&f2, &other));
        let olds: Vec<_> = base.iter().filter_map(|a| a.inherited.clone()).collect();
        let unique: BTreeSet<_> = olds.iter().collect();
        assert_eq!(olds.len(), unique.len(), "an old IRI is inherited once");
    }

    #[test]
    fn ratios_compare_exactly() {
        let a = Ratio {
            inter: u64::MAX - 1,
            union: u64::MAX,
        };
        let b = Ratio {
            inter: u64::MAX - 2,
            union: u64::MAX - 1,
        };
        assert_eq!(a.cmp(&b), Ordering::Greater);
        assert!(Ratio { inter: 1, union: 2 }.qualifies());
        assert!(!Ratio { inter: 0, union: 0 }.qualifies());
    }

    #[test]
    fn new_iris_encode_the_dataset() {
        let iri = new_iri("my ds/é", 2, &uuid::Uuid::from_u128(1));
        assert_eq!(
            iri,
            "urn:x-sparkles:graphrag:my%20ds%2F%C3%A9:2:00000000-0000-0000-0000-000000000001"
        );
    }
}
