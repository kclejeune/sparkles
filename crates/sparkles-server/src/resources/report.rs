//! The licence obligations of a version, propagated through its references, the gate's
//! verdict on redistributing it, and its NOTICE.
//!
//! The obligations of a version are its own licence and, transitively, those of the
//! versions it references: its imports and, for a dataset version, the sources of its
//! graphs. Each graph of a dataset version has the version's own obligations, those of
//! its imports and those of its sources. A class is the most restrictive of its
//! obligations. A catalog checked at write time has no reference cycles, but the walk
//! keeps a visited set so that a file edited by hand cannot loop it.

use super::license::{self, Class, Policy, Verdict};
use super::{CatalogFile, Pin, Resource, Result, Version, find, no_such_version};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One licence that applies to data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Obligation {
    pub resource: String,
    pub title: String,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    pub class: Class,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// for restricted terms, whether an administrator accepted them
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<bool>,
}

/// The obligations of one graph of a dataset version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphObligations {
    pub graph: String,
    pub class: Class,
    pub redistributable: bool,
    /// `resource version` keys into the report's obligations
    pub obligations: Vec<String>,
}

/// The licence report of a version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LicenseReport {
    pub resource: String,
    pub version: Option<String>,
    pub class: Class,
    pub redistributable: bool,
    pub policy: Policy,
    pub verdict: Verdict,
    pub obligations: Vec<Obligation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub graphs: Vec<GraphObligations>,
    /// resources with restricted terms that no administrator accepted
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unaccepted: Vec<String>,
    pub warnings: Vec<String>,
}

fn key(o: &Obligation) -> String {
    format!("{} {}", o.resource, o.version)
}

fn obligation(r: &Resource, v: Option<&Version>) -> Obligation {
    let l = r.licence_of(v);
    // a catalog read from disk is checked, so the class reads; unknown is the safe side
    let class = l.class().unwrap_or(Class::Unknown);
    Obligation {
        resource: r.id.clone(),
        title: r.input.title.clone(),
        version: v.map_or_else(String::new, |v| v.input.version.clone()),
        license: l.expression.map(str::to_string),
        class,
        url: l.page(),
        attribution: r.input.attribution.clone(),
        accepted: (class == Class::Restricted).then(|| r.accepted()),
    }
}

/// Add the obligations of `pin` and of everything it references.
fn walk(
    c: &CatalogFile,
    pin: &Pin,
    seen: &mut BTreeSet<Pin>,
    out: &mut BTreeMap<String, Obligation>,
) {
    let mut todo = vec![pin.clone()];
    while let Some(p) = todo.pop() {
        if !seen.insert(p.clone()) {
            continue;
        }
        let Some(r) = find(c, &p.resource) else {
            continue;
        };
        let Some(v) = r.version(&p.version) else {
            continue;
        };
        let o = obligation(r, Some(v));
        out.insert(key(&o), o);
        todo.extend(Resource::pins(v).cloned());
    }
}

/// The licence report of `version` of `id`, the current version when `None`.
pub fn license_report(
    c: &CatalogFile,
    id: &str,
    version: Option<&str>,
    policy: Policy,
) -> Result<LicenseReport> {
    let r = find(c, id).ok_or_else(|| super::no_such(id))?;
    let v = match version {
        Some(x) => Some(r.version(x).ok_or_else(|| no_such_version(id, x))?),
        None => r.current(),
    };
    let own = obligation(r, v);
    let mut base = BTreeMap::new();
    base.insert(key(&own), own.clone());
    if let Some(v) = v {
        let mut seen = BTreeSet::new();
        seen.insert(Pin {
            resource: r.id.clone(),
            version: v.input.version.clone(),
        });
        for p in &v.input.imports {
            walk(c, p, &mut seen, &mut base);
        }
    }
    let mut all = base.clone();
    let mut graphs = Vec::new();
    for g in v.map(|v| v.input.graphs.as_slice()).unwrap_or_default() {
        let mut mine = base.clone();
        let mut seen = BTreeSet::new();
        for p in &g.sources {
            walk(c, p, &mut seen, &mut mine);
        }
        let class = mine
            .values()
            .map(|o| o.class)
            .max()
            .unwrap_or(Class::Unknown);
        graphs.push(GraphObligations {
            graph: g.graph.clone(),
            class,
            redistributable: class.redistributable(),
            obligations: mine.keys().cloned().collect(),
        });
        all.extend(mine);
    }
    let class = all
        .values()
        .map(|o| o.class)
        .max()
        .unwrap_or(Class::Unknown);
    let verdict = license::verdict(policy, class);
    let mut unaccepted: Vec<String> = all
        .values()
        .filter(|o| o.accepted == Some(false))
        .map(|o| o.resource.clone())
        .collect();
    unaccepted.sort();
    unaccepted.dedup();
    let mut warnings = Vec::new();
    for o in all.values() {
        match o.class {
            Class::Unknown => warnings.push(format!(
                "{}{} records no licence, so it may not be redistributed",
                o.resource,
                version_suffix(&o.version)
            )),
            Class::Restricted => warnings.push(format!(
                "{}{} has restricted terms ({}), so it may not be redistributed",
                o.resource,
                version_suffix(&o.version),
                o.license.as_deref().unwrap_or("NOASSERTION")
            )),
            _ => {}
        }
    }
    for u in &unaccepted {
        warnings.push(format!(
            "no administrator accepted the restricted terms of {u}"
        ));
    }
    Ok(LicenseReport {
        resource: r.id.clone(),
        version: v.map(|v| v.input.version.clone()),
        class,
        redistributable: class.redistributable(),
        policy,
        verdict,
        obligations: all.into_values().collect(),
        graphs,
        unaccepted,
        warnings,
    })
}

fn version_suffix(v: &str) -> String {
    if v.is_empty() {
        String::new()
    } else {
        format!(" {v}")
    }
}

/// The NOTICE of a report: each licence with its attribution, its page and the graphs
/// it covers.
pub fn notice(c: &CatalogFile, rep: &LicenseReport) -> String {
    let title = find(c, &rep.resource).map_or(rep.resource.as_str(), |r| r.input.title.as_str());
    let mut s = format!(
        "NOTICE for {title} ({}{})\n\n",
        rep.resource,
        rep.version
            .as_deref()
            .map(|v| format!(" version {v}"))
            .unwrap_or_default()
    );
    s.push_str("This data combines material under the licences below.\n");
    if !rep.redistributable {
        s.push_str("Some of it may not be redistributed.\n");
    }
    for o in &rep.obligations {
        s.push('\n');
        s.push_str(&o.title);
        s.push_str(&version_suffix(&o.version));
        s.push_str(&format!(" ({})\n", o.resource));
        s.push_str(&format!(
            "  Licence: {} ({})\n",
            o.license.as_deref().unwrap_or("NOASSERTION"),
            o.class.as_str()
        ));
        if let Some(u) = &o.url {
            s.push_str(&format!("  {u}\n"));
        }
        if let Some(a) = &o.attribution {
            for line in a.lines() {
                s.push_str(&format!("  {line}\n"));
            }
        }
        let k = key(o);
        let covered: Vec<&str> = rep
            .graphs
            .iter()
            .filter(|g| g.obligations.contains(&k))
            .map(|g| g.graph.as_str())
            .collect();
        if !covered.is_empty() && covered.len() < rep.graphs.len() {
            s.push_str(&format!("  Graphs: {}\n", covered.join(", ")));
        }
    }
    s
}
