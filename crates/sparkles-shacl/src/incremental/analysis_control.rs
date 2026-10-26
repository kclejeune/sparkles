//! Admission of owned dependency-analysis state before structural cloning.

use super::{Dep, Dir, Fallback, PropertyPath};
use oxrdf::NamedNode;

type Control<'a> = Option<&'a mut dyn FnMut() -> Result<(), Fallback>>;

pub(super) fn charge(control: &mut Control<'_>) -> Result<(), Fallback> {
    match control {
        Some(check) => check(),
        None => Ok(()),
    }
}

/// Lexical copies contribute one work unit per 128-byte chunk. Check each chunk
/// before cloning, including a single predicate with a very large IRI.
pub(super) fn admit_iri(iri: &NamedNode, control: &mut Control<'_>) -> Result<(), Fallback> {
    if control.is_none() {
        return Ok(());
    }
    for _ in 0..iri.as_str().len().div_ceil(128) {
        charge(control)?;
    }
    Ok(())
}

pub(super) fn admit_path(path: &PropertyPath, control: &mut Control<'_>) -> Result<(), Fallback> {
    if control.is_none() {
        return Ok(());
    }
    charge(control)?;
    use PropertyPath as P;
    match path {
        P::Predicate(iri) => admit_iri(iri, control)?,
        P::Sequence(paths) | P::Alternative(paths) => {
            for p in paths {
                admit_path(p, control)?;
            }
        }
        P::Inverse(p) | P::ZeroOrMore(p) | P::OneOrMore(p) | P::ZeroOrOne(p) => {
            admit_path(p, control)?
        }
    }
    Ok(())
}

pub(super) fn admit_prefix(
    paths: &[PropertyPath],
    control: &mut Control<'_>,
) -> Result<(), Fallback> {
    if control.is_none() {
        return Ok(());
    }
    charge(control)?;
    for p in paths {
        admit_path(p, control)?;
    }
    Ok(())
}

pub(super) fn prefix(
    paths: &[PropertyPath],
    control: &mut Control<'_>,
) -> Result<Vec<PropertyPath>, Fallback> {
    admit_prefix(paths, control)?;
    Ok(paths.to_vec())
}

pub(super) fn pre(
    paths: &Option<Vec<PropertyPath>>,
    control: &mut Control<'_>,
) -> Result<Option<Vec<PropertyPath>>, Fallback> {
    paths.as_ref().map(|p| prefix(p, control)).transpose()
}

pub(super) fn deps(ds: &[Dep], control: &mut Control<'_>) -> Result<Vec<Dep>, Fallback> {
    if control.is_none() {
        return Ok(ds.to_vec());
    }
    let mut out = Vec::new();
    for d in ds {
        charge(control)?;
        if let Some(p) = &d.pred {
            admit_iri(p, control)?;
        }
        out.push(Dep {
            prefix: prefix(&d.prefix, control)?,
            pred: d.pred.clone(),
            dir: d.dir,
        });
    }
    Ok(out)
}

pub(super) fn result(
    ds: &Result<Vec<Dep>, Fallback>,
    control: &mut Control<'_>,
) -> Result<Vec<Dep>, Fallback> {
    match ds {
        Ok(ds) => deps(ds, control),
        Err(f) => Err(*f),
    }
}

pub(super) fn nnf(
    path: &PropertyPath,
    inverse: bool,
    control: &mut Control<'_>,
) -> Result<PropertyPath, Fallback> {
    if control.is_none() {
        return Ok(super::nnf(path, inverse));
    }
    charge(control)?;
    use PropertyPath as P;
    if let P::Predicate(iri) = path {
        admit_iri(iri, control)?;
    }
    Ok(match path {
        P::Predicate(n) if inverse => P::Inverse(Box::new(P::Predicate(n.clone()))),
        P::Predicate(n) => P::Predicate(n.clone()),
        P::Inverse(p) => nnf(p, !inverse, control)?,
        P::Sequence(ps) | P::Alternative(ps) => {
            let mut out = Vec::new();
            for p in ps {
                out.push(nnf(p, inverse, control)?);
            }
            if matches!(path, P::Sequence(_)) {
                if inverse {
                    out.reverse();
                }
                P::Sequence(out)
            } else {
                P::Alternative(out)
            }
        }
        P::ZeroOrMore(p) => P::ZeroOrMore(Box::new(nnf(p, inverse, control)?)),
        P::OneOrMore(p) => P::OneOrMore(Box::new(nnf(p, inverse, control)?)),
        P::ZeroOrOne(p) => P::ZeroOrOne(Box::new(nnf(p, inverse, control)?)),
    })
}

type Step = (Vec<PropertyPath>, NamedNode, Dir);

pub(super) fn steps(path: &PropertyPath, control: &mut Control<'_>) -> Result<Vec<Step>, Fallback> {
    if control.is_none() {
        return Ok(super::steps(path));
    }
    charge(control)?;
    use PropertyPath as P;
    match path {
        P::Predicate(iri) => admit_iri(iri, control)?,
        P::Inverse(p) => {
            if let P::Predicate(iri) = p.as_ref() {
                admit_iri(iri, control)?;
            }
        }
        _ => {}
    }
    Ok(match path {
        P::Predicate(n) => vec![(Vec::new(), n.clone(), Dir::Out)],
        P::Inverse(p) => match p.as_ref() {
            P::Predicate(n) => vec![(Vec::new(), n.clone(), Dir::In)],
            other => steps(&nnf(other, true, control)?, control)?,
        },
        P::Sequence(ps) => {
            let mut out = Vec::new();
            for (i, p) in ps.iter().enumerate() {
                for (tail, n, dir) in steps(p, control)? {
                    let mut pre = prefix(&ps[..i], control)?;
                    pre.extend(tail);
                    charge(control)?;
                    out.push((pre, n, dir));
                }
            }
            out
        }
        P::Alternative(ps) => {
            let mut out = Vec::new();
            for p in ps {
                out.extend(steps(p, control)?);
            }
            out
        }
        P::ZeroOrMore(p) | P::OneOrMore(p) => {
            let mut out = Vec::new();
            for (tail, n, dir) in steps(p, control)? {
                admit_path(p, control)?;
                charge(control)?;
                let mut pre = vec![P::ZeroOrMore(p.clone())];
                pre.extend(tail);
                out.push((pre, n, dir));
            }
            out
        }
        P::ZeroOrOne(p) => steps(p, control)?,
    })
}
