//! `sparkles compare` (aliases `rdfcompare`, `rdfdiff`): two RDF files compared up to
//! blank-node isomorphism, with a diff that stays local to what changed (spec G05 §3.3).
//!
//! Each dataset is split into its ground quads and the groups of quads connected by
//! shared blank nodes. Ground quads are compared as sets; each group is canonicalized
//! with RDFC-1.0 on its own and the groups are compared as multisets of canonical forms.
//! An isomorphism maps groups onto groups, and a canonical form identifies a group up to
//! isomorphism, so the datasets are isomorphic exactly when both comparisons agree.

use super::convert::{Input, read_all};
use anyhow::Result;
use oxrdf::dataset::{CanonicalizationAlgorithm, CanonicalizationHashAlgorithm};
use oxrdf::{Dataset, GraphName, NamedOrBlankNode, Quad, Term};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::path::PathBuf;

#[derive(clap::Args)]
pub struct CompareArgs {
    /// The first file (`-`: standard input)
    a: PathBuf,
    /// The second file (`-`: standard input)
    b: PathBuf,
    /// Syntax of both files (default: from the extensions)
    #[arg(long, value_name = "LANG")]
    syntax: Option<String>,
    /// Base IRI for relative IRIs (default: each file's own `file://` IRI)
    #[arg(long, value_name = "IRI")]
    base: Option<String>,
    /// Compare the merged graphs, ignoring graph names
    #[arg(long)]
    merge: bool,
    /// Print nothing: the exit status says whether the files are isomorphic
    #[arg(long, short)]
    quiet: bool,
}

const RDFC10: CanonicalizationAlgorithm = CanonicalizationAlgorithm::Rdfc10 {
    hash_algorithm: CanonicalizationHashAlgorithm::Sha256,
};

/// A dataset split for comparison.
#[derive(Default)]
struct Split {
    ground: HashSet<String>,
    /// canonical form of a blank-node group → (its lines, how many such groups)
    groups: HashMap<String, (Vec<String>, usize)>,
    quads: usize,
}

fn line(q: &Quad) -> String {
    match &q.graph_name {
        GraphName::DefaultGraph => format!("{} {} {} .", q.subject, q.predicate, q.object),
        g => format!("{} {} {} {g} .", q.subject, q.predicate, q.object),
    }
}

/// The blank node labels of a term, triple terms included.
fn blanks<'a>(t: &'a Term, out: &mut Vec<&'a str>) {
    match t {
        Term::BlankNode(b) => out.push(b.as_str()),
        Term::Triple(tr) => {
            if let NamedOrBlankNode::BlankNode(b) = &tr.subject {
                out.push(b.as_str());
            }
            blanks(&tr.object, out);
        }
        _ => {}
    }
}

fn quad_blanks(q: &Quad) -> Vec<&str> {
    let mut v = Vec::new();
    if let NamedOrBlankNode::BlankNode(b) = &q.subject {
        v.push(b.as_str());
    }
    blanks(&q.object, &mut v);
    if let GraphName::BlankNode(b) = &q.graph_name {
        v.push(b.as_str());
    }
    v
}

fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

/// The canonical lines of one group, sorted.
fn canonical(quads: Vec<Quad>) -> Vec<String> {
    let mut d: Dataset = quads.into_iter().collect();
    d.canonicalize(RDFC10);
    let mut lines: Vec<String> = d.iter().map(|q| line(&q.into_owned())).collect();
    lines.sort_unstable();
    lines
}

fn split(quads: Vec<Quad>, merge: bool) -> Split {
    let mut unique: HashSet<Quad> = HashSet::with_capacity(quads.len());
    for mut q in quads {
        if merge {
            q.graph_name = GraphName::DefaultGraph;
        }
        unique.insert(q);
    }
    let mut s = Split {
        quads: unique.len(),
        ..Default::default()
    };
    // union-find over blank node labels
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut parent: Vec<usize> = Vec::new();
    let mut with_blanks = Vec::new();
    for q in unique {
        let bs = quad_blanks(&q);
        if bs.is_empty() {
            s.ground.insert(line(&q));
            continue;
        }
        let mut first = None;
        for b in bs {
            let id = *ids.entry(b.to_string()).or_insert_with(|| {
                parent.push(parent.len());
                parent.len() - 1
            });
            match first {
                None => first = Some(id),
                Some(f) => {
                    let (ra, rb) = (find(&mut parent, f), find(&mut parent, id));
                    if ra != rb {
                        parent[rb] = ra;
                    }
                }
            }
        }
        let id = first.expect("a blank node");
        with_blanks.push((id, q));
    }
    let mut groups: BTreeMap<usize, Vec<Quad>> = BTreeMap::new();
    for (id, q) in with_blanks {
        groups.entry(find(&mut parent, id)).or_default().push(q);
    }
    for (_, g) in groups {
        let lines = canonical(g);
        let key = lines.join("\n");
        s.groups.entry(key).or_insert((lines, 0)).1 += 1;
    }
    s
}

/// The differences of `a` against `b`: ground lines, then the lines of each group
/// only in `a`, with blank node labels renumbered from `next` so that they are unique.
fn only_in(a: &Split, b: &Split, next: &mut usize) -> Vec<String> {
    let mut ground: Vec<&String> = a.ground.difference(&b.ground).collect();
    ground.sort();
    let mut out: Vec<String> = ground.into_iter().cloned().collect();
    let mut keys: Vec<&String> = a.groups.keys().collect();
    keys.sort();
    let re = regex::Regex::new(r"_:c14n(\d+)").expect("a valid regex");
    for k in keys {
        let (lines, n) = &a.groups[k];
        let m = b.groups.get(k).map_or(0, |g| g.1);
        for _ in m..*n {
            let mut most = 0;
            for l in lines {
                let relabeled = re.replace_all(l, |c: &regex::Captures<'_>| {
                    let i: usize = c[1].parse().unwrap_or(0);
                    most = most.max(i + 1);
                    format!("_:c14n{}", i + *next)
                });
                out.push(relabeled.into_owned());
            }
            *next += most;
        }
    }
    out
}

/// `sparkles compare`: exit status 0 when isomorphic, 1 when different, 2 on an error.
pub fn run(a: CompareArgs) -> Result<()> {
    match compare(&a) {
        Ok(true) => Ok(()),
        Ok(false) => std::process::exit(1),
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(2)
        }
    }
}

fn compare(a: &CompareArgs) -> Result<bool> {
    let inputs = Input::all(
        &[a.a.clone(), a.b.clone()],
        a.syntax.as_deref(),
        a.base.as_deref(),
    )?;
    if inputs.iter().all(|i| i.path.is_none()) {
        anyhow::bail!("only one of the files can be standard input");
    }
    let sa = split(read_all(&inputs[0], None)?, a.merge);
    let sb = split(read_all(&inputs[1], None)?, a.merge);
    let mut next = 0;
    let left = only_in(&sa, &sb, &mut next);
    let right = only_in(&sb, &sa, &mut next);
    let same = left.is_empty() && right.is_empty();
    if a.quiet {
        return Ok(same);
    }
    let (na, nb) = (&inputs[0].name, &inputs[1].name);
    if same {
        eprintln!("{na} and {nb} are isomorphic ({} quads)", sa.quads);
        return Ok(true);
    }
    let mut out = std::io::stdout().lock();
    for l in &left {
        writeln!(out, "< {l}")?;
    }
    for l in &right {
        writeln!(out, "> {l}")?;
    }
    out.flush()?;
    eprintln!(
        "{na} and {nb} differ: {} quads only in {na}, {} only in {nb}",
        left.len(),
        right.len()
    );
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quads(nq: &str) -> Vec<Quad> {
        oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::NQuads)
            .for_slice(nq.as_bytes())
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn diff(a: &str, b: &str) -> (Vec<String>, Vec<String>) {
        let (sa, sb) = (split(quads(a), false), split(quads(b), false));
        let mut next = 0;
        (only_in(&sa, &sb, &mut next), only_in(&sb, &sa, &mut next))
    }

    #[test]
    fn relabeled_blank_nodes_are_isomorphic() {
        let a = "_:x <http://e/p> _:y .\n_:y <http://e/q> \"1\" .\n<http://e/s> <http://e/p> <http://e/o> .\n_:z <http://e/p> \"2\" <http://e/g> .\n";
        let b = "<http://e/s> <http://e/p> <http://e/o> .\n_:b2 <http://e/q> \"1\" .\n_:b1 <http://e/p> _:b2 .\n_:k <http://e/p> \"2\" <http://e/g> .\n_:k <http://e/p> \"2\" <http://e/g> .\n";
        assert_eq!(diff(a, b), (vec![], vec![]));
    }

    #[test]
    fn a_change_shows_its_own_group_only() {
        let a = "_:a <http://e/p> \"1\" .\n_:b <http://e/p> \"2\" .\n_:c <http://e/p> _:d .\n_:d <http://e/q> \"3\" .\n";
        let b = "_:a <http://e/p> \"1\" .\n_:b <http://e/p> \"2\" .\n_:c <http://e/p> _:d .\n_:d <http://e/q> \"4\" .\n";
        let (l, r) = diff(a, b);
        assert_eq!(l.len(), 2, "{l:?}");
        assert_eq!(r.len(), 2, "{r:?}");
        assert!(l.iter().any(|x| x.contains("\"3\"")));
        assert!(r.iter().any(|x| x.contains("\"4\"")));
        // labels are unique across the diff
        let labels = |v: &[String]| -> HashSet<String> {
            v.iter()
                .flat_map(|l| {
                    l.split(' ')
                        .filter(|t| t.starts_with("_:"))
                        .map(str::to_string)
                })
                .collect()
        };
        assert!(labels(&l).is_disjoint(&labels(&r)));
    }

    #[test]
    fn group_multiplicity_counts() {
        // two copies of the same shape are not one
        let a = "_:a <http://e/p> \"1\" .\n_:b <http://e/p> \"1\" .\n";
        let b = "_:a <http://e/p> \"1\" .\n";
        let (l, r) = diff(a, b);
        assert_eq!((l.len(), r.len()), (1, 0));
    }

    #[test]
    fn ground_differences_and_merge() {
        let a = "<http://e/s> <http://e/p> <http://e/o> <http://e/g> .\n";
        let b = "<http://e/s> <http://e/p> <http://e/o> .\n";
        let (l, r) = diff(a, b);
        assert_eq!(l, ["<http://e/s> <http://e/p> <http://e/o> <http://e/g> ."]);
        assert_eq!(r, ["<http://e/s> <http://e/p> <http://e/o> ."]);
        let (sa, sb) = (split(quads(a), true), split(quads(b), true));
        assert_eq!(sa.ground, sb.ground);
    }
}
