//! The `prefixes` settings kind (spec C20): a map from a prefix name to an IRI.
//!
//! Its layers are the well-known prefixes as the built-in defaults, the declared
//! `defaults.prefixes` and `datasets.<name>.prefixes`, and the runtime layer, which is
//! what the dataset's store keeps: the bindings of `prefixes.json` as strings and the
//! removed names of `prefixes-removed.json` as `null`. A `null` removes a declared or
//! well-known prefix, and a reset of the name brings it back.
//!
//! The store skips the names that the settings file declares or locks when loaded data
//! brings its prefixes ([`filter`]). `/{ds}/prefixes` writes through the same path as
//! the settings routes ([`fuseki_write`]), so its writes honor the locks.

use super::{Declared, Kind, Providers, Scope, Typed, check_as, normalize_as};
use crate::state::{AppState, Dataset};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The kind's name.
pub const NAME: &str = "prefixes";

pub static PREFIXES: Kind = Kind {
    name: NAME,
    scope: Scope::Dataset,
    file: "prefixes.json",
    members: &[],
    removable: &[],
    shared: false,
    defaults: well_known_value,
    normalize: normalize_as::<PrefixMap>,
    check: check_as::<PrefixMap>,
};

/// Whether `kind` is the prefixes kind.
pub fn is(kind: &Kind) -> bool {
    std::ptr::eq(kind, &PREFIXES)
}

/// `--max-prefixes` of `serve` and of `settings check`: the most prefixes a dataset has
/// besides the well-known ones (0: no limit).
static MAX: AtomicUsize = AtomicUsize::new(sparkles::store::DEFAULT_MAX_PREFIXES);

pub fn set_max(n: usize) {
    MAX.store(n, Ordering::Relaxed);
}

fn max() -> usize {
    MAX.load(Ordering::Relaxed)
}

fn well_known_value() -> Value {
    serde_json::to_value(sparkles::io::well_known_prefixes()).unwrap_or_default()
}

/// The effective prefixes as their type.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PrefixMap(pub BTreeMap<String, String>);

impl Typed for PrefixMap {
    fn validate(&self, _: Providers) -> Result<(), String> {
        for (name, iri) in &self.0 {
            sparkles::store::check_prefix(name, iri).map_err(|e| match e {
                sparkles::Error::Invalid(m) => m,
                e => e.to_string(),
            })?;
        }
        let wk = sparkles::io::well_known_prefixes();
        let own = self
            .0
            .iter()
            .filter(|(k, v)| wk.get(*k) != Some(*v))
            .count();
        let max = max();
        if max > 0 && own > max {
            return Err(format!(
                "{own} prefixes besides the well-known ones are more than the {max} allowed (--max-prefixes)"
            ));
        }
        Ok(())
    }
}

/// Whether a member name may be a prefix: `PN_PREFIX` in the ASCII subset, or empty.
pub fn valid_name(name: &str) -> bool {
    name.len() <= sparkles::store::MAX_PREFIX_NAME_BYTES && sparkles::store::valid_prefix_name(name)
}

/// The runtime layer as the store keeps it: bindings as strings, removals as `null`.
pub fn runtime_layer(ds: &Dataset) -> Value {
    let mut m: Map<String, Value> = ds
        .store
        .prefixes()
        .into_iter()
        .map(|(k, v)| (k, Value::String(v)))
        .collect();
    for name in ds.store.removed_prefixes() {
        m.insert(name, Value::Null);
    }
    Value::Object(m)
}

/// Store a runtime layer: its strings as bindings and its `null`s as removals.
pub fn store_layer(ds: &Dataset, layer: &Value) -> sparkles::Result<()> {
    let mut bindings = BTreeMap::new();
    let mut removed = BTreeSet::new();
    if let Value::Object(m) = layer {
        for (k, v) in m {
            match v {
                Value::String(s) => {
                    bindings.insert(k.clone(), s.clone());
                }
                Value::Null => {
                    removed.insert(k.clone());
                }
                other => {
                    return Err(sparkles::Error::invalid(format!(
                        "prefixes.{k}: {other} is not an IRI"
                    )));
                }
            }
        }
    }
    ds.store.set_prefix_layer(bindings, removed)
}

/// The number of entries of a runtime layer, bindings and removals together.
pub fn entries(layer: &Value) -> usize {
    layer.as_object().map_or(0, Map::len)
}

/// The prefixes that the declared and well-known layers bind or lock for `dataset`,
/// before the runtime layer: the bindings, and the locks (a whole-kind lock, or the
/// locked names).
struct Base {
    map: BTreeMap<String, String>,
    whole: bool,
    locked: BTreeSet<String>,
}

fn base(declared: &Declared, dataset: &str, well_known: bool) -> Base {
    let mut map = if well_known {
        sparkles::io::well_known_prefixes()
    } else {
        BTreeMap::new()
    };
    let (dd, de) = declared.layers(&PREFIXES, dataset);
    for l in [dd, de].into_iter().flatten() {
        if let Value::Object(o) = l {
            for (k, v) in o {
                match v {
                    Value::String(s) => {
                        map.insert(k.clone(), s.clone());
                    }
                    _ => {
                        map.remove(k);
                    }
                }
            }
        }
    }
    let locks = declared.locked(&PREFIXES, dataset);
    Base {
        map,
        whole: locks.iter().any(Vec::is_empty),
        locked: locks
            .into_iter()
            .filter(|l| l.len() == 1)
            .map(|mut l| l.remove(0))
            .collect(),
    }
}

/// The effective prefixes of a dataset (§3), computed without the checks of a full
/// resolution, for the routes that read them on every request. With `well_known` false
/// the well-known layer is left out, which gives the prefixes that results and Graph
/// Store answers are written with (§5).
pub fn effective_with(
    declared: &Declared,
    ds: &Dataset,
    well_known: bool,
) -> BTreeMap<String, String> {
    let main = ds.main();
    let ds = main.as_deref().unwrap_or(ds);
    let Base {
        mut map,
        whole,
        locked,
    } = base(declared, &ds.name, well_known);
    if whole {
        return map;
    }
    for (k, v) in ds.store.prefixes() {
        if !locked.contains(&k) {
            map.insert(k, v);
        }
    }
    for k in ds.store.removed_prefixes() {
        if !locked.contains(&k) {
            map.remove(&k);
        }
    }
    map
}

/// The effective prefixes of a dataset, the well-known ones included
/// (`GET /$/prefixes/{ds}`).
pub fn effective(st: &AppState, ds: &Dataset) -> BTreeMap<String, String> {
    effective_with(&st.settings.declared(), ds, true)
}

/// The declared and runtime prefixes of a dataset, which results, Graph Store answers
/// and SHACL targets use.
pub fn bound(st: &AppState, ds: &Dataset) -> BTreeMap<String, String> {
    effective_with(&st.settings.declared(), ds, false)
}

/// Whether the settings file keeps `prefix` for `dataset`, so that loaded data does not
/// bind it: it declares the name, or locks it or the whole kind (§3.4).
pub fn keeps(declared: &Declared, dataset: &str, prefix: &str) -> bool {
    let (dd, de) = declared.layers(&PREFIXES, dataset);
    if [dd, de]
        .into_iter()
        .flatten()
        .any(|l| l.get(prefix).is_some())
    {
        return true;
    }
    declared
        .locked(&PREFIXES, dataset)
        .iter()
        .any(|l| l.is_empty() || (l.len() == 1 && l[0] == prefix))
}

/// The store's filter for the prefixes of loaded data of the dataset `name`, reading the
/// settings file of this moment.
pub fn filter(
    declared: Arc<arc_swap::ArcSwap<Declared>>,
    name: String,
) -> sparkles::store::PrefixFilter {
    Arc::new(move |prefix: &str| !keeps(&declared.load(), &name, prefix))
}

/// The prefixes of `effective` that shadow a well-known prefix with another IRI
/// (§4.1), as the `warnings` member of the answer.
pub fn warnings(effective: &Value) -> Vec<Value> {
    let wk = sparkles::io::well_known_prefixes();
    let Value::Object(m) = effective else {
        return Vec::new();
    };
    m.iter()
        .filter_map(|(k, v)| {
            let iri = v.as_str()?;
            let known = wk.get(k)?;
            (known != iri).then(|| {
                json!({
                    "prefix": k,
                    "iri": iri,
                    "wellKnown": known,
                    "message": shadow_message(k, iri, known),
                })
            })
        })
        .collect()
}

fn shadow_message(name: &str, iri: &str, known: &str) -> String {
    format!("{name}: is bound to <{iri}>, which shadows the well-known {name}: <{known}>")
}

/// The warnings of a settings file: each declared prefix, in `defaults` or a dataset's
/// entry, that shadows a well-known one (`sparkles settings check`).
pub fn file_warnings(d: &Declared) -> Vec<String> {
    let wk = sparkles::io::well_known_prefixes();
    let mut out = Vec::new();
    let entries = std::iter::once(("defaults".to_string(), &d.defaults))
        .chain(d.datasets.iter().map(|(n, e)| (format!("datasets.{n}"), e)));
    for (label, e) in entries {
        let Some(Value::Object(m)) = e.values.get(NAME) else {
            continue;
        };
        for (k, v) in m {
            if let (Some(iri), Some(known)) = (v.as_str(), wk.get(k))
                && known != iri
            {
                out.push(format!(
                    "{label}.prefixes.{}",
                    shadow_message(k, iri, known)
                ));
            }
        }
    }
    out
}

/// The locked value of `prefix` for `dataset`, when the settings file locks it: the
/// declared or well-known IRI, or `None` for a name the lock keeps unbound.
pub fn locked_value(declared: &Declared, dataset: &str, prefix: &str) -> Option<Option<String>> {
    let b = base(declared, dataset, true);
    (b.whole || b.locked.contains(prefix)).then(|| b.map.get(prefix).cloned())
}

/// Whether the settings file declares `prefix` for `dataset` (`defaults` or the entry),
/// so that a `DELETE` of it stores a removal.
pub fn declared_name(declared: &Declared, dataset: &str, prefix: &str) -> bool {
    let b = base(declared, dataset, false);
    b.map.contains_key(prefix)
}

/// What `/{ds}/prefixes` asks of the runtime layer.
pub enum FusekiWrite {
    /// bind a name
    Set(String, String),
    /// remove a binding, and the declared value of a declared name
    Remove(String),
}

/// A `POST`, `PUT` or `DELETE` of `/{ds}/prefixes` (§5), through the settings write so
/// that it takes the same locks and checks. `Ok(false)` for a `DELETE` of a name that is
/// neither stored nor declared.
pub async fn fuseki_write(
    st: Arc<AppState>,
    ds: Arc<Dataset>,
    w: FusekiWrite,
) -> crate::http::ApiResult<bool> {
    use super::http::{Write, write};
    let declared = st.settings.declared();
    let main = ds.main();
    let target = main.clone().unwrap_or_else(|| ds.clone());
    let body = match &w {
        FusekiWrite::Set(p, iri) => {
            if !valid_name(p) {
                return Err(super::http::bad(format!("invalid prefix name {p:?}")));
            }
            json!({ p: iri })
        }
        FusekiWrite::Remove(p) => {
            if locked_value(&declared, &target.name, p).is_some() {
                return Err(super::http::locked_error(
                    std::slice::from_ref(p),
                    &format!("/{}", target.name),
                ));
            }
            let stored = target.store.prefixes().contains_key(p);
            let removed = target.store.removed_prefixes().contains(p);
            if declared_name(&declared, &target.name, p) && !removed {
                json!({ p: null })
            } else if stored {
                let field = super::merge::path_string(std::slice::from_ref(p));
                write(
                    st,
                    target,
                    &PREFIXES,
                    Write::Delete(Some(field)),
                    &Default::default(),
                )
                .await?;
                return Ok(true);
            } else {
                return Ok(false);
            }
        }
    };
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    write(
        st,
        target,
        &PREFIXES,
        Write::Patch(bytes.into()),
        &Default::default(),
    )
    .await?;
    Ok(true)
}
