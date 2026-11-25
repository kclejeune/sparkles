//! The `evidence` settings kind (spec C25 §7, C19): declared evidence settings,
//! field locks, and drift refusal between the C19 effective settings and the dataset's
//! recorded `evidence.json` / `managed.json`.
//!
//! `evidence.json` always holds the complete canonical object whose SHA-256 digest
//! `managed.json` records, and is written only through [`sparkles::evidence::configure`],
//! which never weakens protection. When `serve --settings` declares evidence settings or
//! locks for a dataset, the registry entry also records sparse runtime overrides of
//! unlocked fields. A single durable registry replacement publishes both layers.
//! While the effective settings differ from the recorded ones, typed evidence operations
//! refuse with `409 evidence-settings-drift` without weakening protection.

use super::http::{Op, bad, locked_error_of, plan};
use super::merge::{at, diff, merged, path_string, prune, starts_with};
use super::{Declared, Kind, Providers, Resolved, Scope, resolve};
use crate::http::ApiResult;
use crate::state::{AppState, Dataset};
use serde_json::{Map, Value, json};
use sparkles::evidence::{Settings, SettingsStatus};
use sparkles::managed::{EvidenceOverrides, ManagedError, ManagedErrorKind, OwnerKind};
use sparkles::task::Control;
use std::sync::Arc;

/// The kind's name in `serve --settings` and `/$/settings/{ds}/evidence`.
pub const NAME: &str = "evidence";

/// Top-level members of the `evidence` kind.
pub const MEMBERS: &[&str] = &[
    "format",
    "profile",
    "enabled",
    "sourceRecords",
    "proposals",
    "decisions",
    "supports",
    "identity",
    "projections",
    "policies",
    "matchingPolicies",
];

const POLICY_FIELDS: &[&str] = &[
    "version",
    "admittedKinds",
    "requiredScope",
    "conflicts",
    "boundary",
    "decisions",
    "supports",
    "output",
];

const MATCHING_POLICY_FIELDS: &[&str] = &["version", "boundary", "decisions", "maps", "output"];

pub static EVIDENCE: Kind = Kind {
    name: NAME,
    scope: Scope::Dataset,
    file: "evidence.json",
    members: MEMBERS,
    removable: &[],
    shared: false,
    defaults: defaults_value,
    normalize: normalize_value,
    check: check_value,
};

/// Whether `kind` is the evidence kind.
pub fn is(kind: &Kind) -> bool {
    std::ptr::eq(kind, &EVIDENCE)
}

/// Built-in defaults of `evidence`: disabled profile 1, format 1, empty roles and policies.
pub fn defaults_value() -> Value {
    Settings::default()
        .canonical(&Control::none())
        .expect("default evidence settings are canonical")
        .value()
        .clone()
}

fn parse_value(v: &Value) -> sparkles::Result<Settings> {
    let bytes = serde_json::to_vec(v)
        .map_err(|e| sparkles::Error::invalid(format!("invalid evidence settings: {e}")))?;
    sparkles::evidence::parse_settings(&bytes, &Control::none())
}

fn settings_error(e: sparkles::Error) -> String {
    match e {
        sparkles::Error::Managed(e) => format!("{}: {}", e.code, e.message),
        e => e.to_string(),
    }
}

fn check_value(v: &Value, _: Providers) -> Result<(), String> {
    parse_value(v).map(|_| ()).map_err(settings_error)
}

/// Normalize a JSON object as canonical `Settings`. Omitted members take built-in
/// defaults (C19 `PUT` rule), while explicit `null` removes the member and fails.
fn normalize_value(v: &Value) -> Result<Value, String> {
    if !v.is_object() {
        return Err("evidence settings must be a JSON object".into());
    }
    let full = merged(&defaults_value(), v);
    let settings = parse_value(&full).map_err(settings_error)?;
    let canonical = settings
        .canonical(&Control::none())
        .map_err(|e| e.to_string())?;
    Ok(canonical.value().clone())
}

fn valid_policy_name(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b.len() <= sparkles::evidence::MAX_POLICY_NAME_BYTES
        && b[0] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// Validate a lock path below `evidence` (empty locks the whole kind).
pub(super) fn check_lock_path(p: &[String]) -> Result<(), String> {
    let Some((first, rest)) = p.split_first() else {
        return Ok(());
    };
    if !MEMBERS.contains(&first.as_str()) {
        return Err(format!("locked: evidence has no member {first:?}"));
    }
    match (first.as_str(), rest) {
        (_, []) => Ok(()),
        ("policies", [name]) | ("matchingPolicies", [name]) => {
            if !valid_policy_name(name) {
                return Err(format!(
                    "locked: evidence.{first}.{name} is not a valid policy name"
                ));
            }
            Ok(())
        }
        ("policies", [name, field]) => {
            if !valid_policy_name(name) {
                return Err(format!(
                    "locked: evidence.policies.{name} is not a valid policy name"
                ));
            }
            if !POLICY_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "locked: evidence.policies.{name} has no member {field:?}"
                ));
            }
            Ok(())
        }
        ("matchingPolicies", [name, field]) => {
            if !valid_policy_name(name) {
                return Err(format!(
                    "locked: evidence.matchingPolicies.{name} is not a valid policy name"
                ));
            }
            if !MATCHING_POLICY_FIELDS.contains(&field.as_str()) {
                return Err(format!(
                    "locked: evidence.matchingPolicies.{name} has no member {field:?}"
                ));
            }
            Ok(())
        }
        ("policies" | "matchingPolicies", _) => Err(format!(
            "locked: evidence.{} has no nested member below a policy field",
            path_string(p)
        )),
        _ => Err(format!(
            "locked: evidence.{first} is a single field and cannot lock sub-fields"
        )),
    }
}

/// Whether `declared` sets any evidence value or lock for `dataset`.
pub fn has_declared(declared: &Declared, dataset: &str) -> bool {
    let (dd, de) = declared.layers(&EVIDENCE, dataset);
    dd.is_some() || de.is_some() || !declared.locked(&EVIDENCE, dataset).is_empty()
}

fn main_of(ds: &Dataset) -> Option<Arc<Dataset>> {
    ds.main()
}

fn override_layer(overrides: Option<EvidenceOverrides>) -> Value {
    Value::Object(overrides.map_or_else(Map::new, EvidenceOverrides::into_patch))
}

fn recorded_layer(status: &SettingsStatus) -> sparkles::Result<Value> {
    if status.digest.is_none() {
        return Ok(Value::Object(Map::new()));
    }
    let recorded = status.settings.canonical(&Control::none())?;
    Ok(diff(&defaults_value(), recorded.value()).unwrap_or_else(|| Value::Object(Map::new())))
}

/// The runtime layer for an evidence write, and the revision the write plans against.
/// A declared kind can still plan a repair from the registry's recorded entry after
/// an interrupted write has left the canonical settings file mismatched.
fn runtime_at(ds: &Dataset, declared: &Declared) -> sparkles::Result<(Value, u64)> {
    if has_declared(declared, &ds.name) {
        let Some(mut snapshot) = ds.store.managed_snapshot()? else {
            return Ok((Value::Object(Map::new()), 0));
        };
        let overrides = snapshot
            .graphs
            .owners
            .remove(&OwnerKind::Evidence)
            .and_then(|e| e.evidence_overrides);
        return Ok((override_layer(overrides), snapshot.graphs.revision));
    }
    let status = sparkles::evidence::settings(&ds.dataset)?;
    Ok((recorded_layer(&status)?, status.revision))
}

/// The sparse override if declarations apply, or the recorded settings over the defaults.
pub fn runtime_layer(st: &AppState, ds: &Dataset) -> anyhow::Result<Value> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    Ok(runtime_at(ds, &st.settings.declared())?.0)
}

/// The resolved C19 view alongside the dataset's recorded evidence settings and drift state.
pub struct EvidenceState {
    pub resolved: Resolved,
    pub recorded: SettingsStatus,
    pub drift: bool,
}

/// Inspect both the C19 effective evidence settings and the dataset's recorded settings.
pub fn inspect(st: &AppState, ds: &Dataset) -> sparkles::Result<EvidenceState> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    inspect_declared(ds, &st.settings.declared())
}

fn inspect_declared(ds: &Dataset, declared: &Declared) -> sparkles::Result<EvidenceState> {
    let (recorded, overrides) = sparkles::evidence::settings_with_overrides(&ds.dataset)?;
    let declared_kind = has_declared(declared, &ds.name);
    let rt = if declared_kind {
        override_layer(overrides)
    } else {
        recorded_layer(&recorded)?
    };
    let resolved = resolve(&EVIDENCE, declared, &ds.name, rt, Providers::Unchecked);
    let drift = declared_kind && compute_drift(&resolved, &recorded)?;
    Ok(EvidenceState {
        resolved,
        recorded,
        drift,
    })
}

fn compute_drift(resolved: &Resolved, recorded: &SettingsStatus) -> sparkles::Result<bool> {
    if resolved.status.is_err() {
        return Ok(true);
    }
    if recorded.digest.is_none() && resolved.effective["enabled"] == Value::Bool(false) {
        // An unconfigured dataset records no disabled settings. Its declaration
        // can still supply role defaults for a later enablement.
        return Ok(false);
    }
    let canonical = recorded.settings.canonical(&Control::none())?;
    Ok(&resolved.effective != canonical.value())
}

impl EvidenceState {
    pub fn json(&self, dataset: &str) -> sparkles::Result<Value> {
        let settings = self.recorded.settings.canonical(&Control::none())?;
        let mut v = self.resolved.json(dataset);
        v["recorded"] = json!({
            "settings": settings.value(),
            "digest": self.recorded.digest,
            "revision": self.recorded.revision,
            "managed": self.recorded.managed,
            "detached": self.recorded.detached,
            "drift": self.drift,
        });
        Ok(v)
    }
}

/// Non-disclosing drift error for typed evidence operations. Does not reveal protected
/// graph IRIs, policy names, or declared values to non-admin callers (C25 decision 7).
pub fn drift_error(dataset: &str) -> sparkles::Error {
    ManagedError::error(
        ManagedErrorKind::EvidenceSettingsDrift,
        format!(
            "the declared and recorded evidence settings of /{dataset} differ; an administrator must reconcile them through /$/evidence/{dataset}/settings or /$/settings/{dataset}/evidence"
        ),
    )
}

/// Refuse with `409 evidence-settings-drift` while the declared and recorded evidence
/// settings of `ds` differ.
pub fn require_reconciled(st: &AppState, ds: &Dataset) -> sparkles::Result<()> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let declared = st.settings.declared();
    if has_declared(&declared, &ds.name) && inspect_declared(ds, &declared)?.drift {
        return Err(drift_error(&ds.name));
    }
    Ok(())
}

/// The settings selected at admission of a typed operation. The store checks its
/// capability against the installed registry, and the publication fence verifies
/// that this is the registry admitted here. Neither check takes a dataset writer.
pub struct DriftFence {
    dataset: String,
    declared_at_start: Arc<Declared>,
    handle: sparkles::Dataset,
    admitted: Option<AdmittedSettings>,
}

struct AdmittedSettings {
    runtime: Value,
    recorded: SettingsStatus,
}

impl DriftFence {
    /// Capture the current declaration and its reconciled registry, if declared.
    pub fn capture(st: &AppState, ds: &Dataset) -> sparkles::Result<Self> {
        let main = main_of(ds);
        let ds = main.as_deref().unwrap_or(ds);
        let declared_at_start = st.settings.declared();
        let admitted = if has_declared(&declared_at_start, &ds.name) {
            let state = inspect_declared(ds, &declared_at_start)?;
            if state.drift {
                return Err(drift_error(&ds.name));
            }
            Some(AdmittedSettings {
                runtime: state.resolved.runtime,
                recorded: state.recorded,
            })
        } else {
            None
        };
        Ok(Self {
            dataset: ds.name.clone(),
            declared_at_start,
            handle: ds.dataset.clone(),
            admitted,
        })
    }

    /// Re-check immediately before publication or returning a typed read, with the
    /// declaration read guard held. A registry revision also pins the runtime layer.
    pub fn check(&self, st: &AppState) -> sparkles::Result<()> {
        let now = st.settings.declared();
        if !has_declared(&now, &self.dataset) {
            return Ok(());
        }
        let Some(admitted) = &self.admitted else {
            // A new declaration can activate a previously dormant runtime layer.
            return Err(drift_error(&self.dataset));
        };
        if self.handle.store().managed_revision()? != admitted.recorded.revision {
            return Err(drift_error(&self.dataset));
        }
        if Arc::ptr_eq(&now, &self.declared_at_start) {
            return Ok(());
        }
        let resolved = resolve(
            &EVIDENCE,
            &now,
            &self.dataset,
            admitted.runtime.clone(),
            Providers::Unchecked,
        );
        if compute_drift(&resolved, &admitted.recorded)? {
            return Err(drift_error(&self.dataset));
        }
        Ok(())
    }
}

/// Check that a complete canonical `target` satisfies every lock of `declared` for `dataset`.
pub fn check_put_locks(declared: &Declared, dataset: &str, target: &Value) -> ApiResult<()> {
    let locked = declared.locked(&EVIDENCE, dataset);
    if locked.is_empty() {
        return Ok(());
    }
    let cur = resolve(
        &EVIDENCE,
        declared,
        dataset,
        Value::Object(Map::new()),
        Providers::Unchecked,
    );
    let base = normalize_value(&cur.base).unwrap_or(cur.base);
    let mut refused = Vec::new();
    for l in &locked {
        if at(target, l) != at(&base, l) {
            refused.push(path_string(l));
        }
    }
    if !refused.is_empty() {
        return Err(locked_error_of(NAME, &refused, &format!("/{dataset}")));
    }
    Ok(())
}

/// Refuse `POST /$/evidence/{ds}/detach` when `enabled` is locked to `true` and `graph`
/// is required by a locked role or locked policy field of the declared settings.
/// Detaching a graph that is no longer required by the locked settings stays allowed so
/// an administrator can reconcile drift without weakening protection automatically.
pub fn check_detach_locks(st: &AppState, ds: &Dataset, graph: &str) -> ApiResult<()> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let declared = st.settings.declared();
    let locked = declared.locked(&EVIDENCE, &ds.name);
    if locked.is_empty() {
        return Ok(());
    }
    let resolved = resolve(
        &EVIDENCE,
        &declared,
        &ds.name,
        Value::Object(Map::new()),
        Providers::Unchecked,
    );
    let enabled_locked = locked
        .iter()
        .any(|l| l.is_empty() || (l.len() == 1 && l[0] == "enabled"));
    if !enabled_locked || resolved.base["enabled"] != Value::Bool(true) {
        return Ok(());
    }
    let base = parse_value(&resolved.base)?;
    let mut refused: Vec<String> = Vec::new();
    if locked.iter().any(Vec::is_empty) {
        if base.managed_graphs().iter().any(|g| g == graph) {
            return Err(locked_error_of(
                NAME,
                &[String::new()],
                &format!("/{}", ds.name),
            ));
        }
        return Ok(());
    }
    let role_lists: [(&str, &[String]); 6] = [
        ("sourceRecords", &base.source_records),
        ("proposals", &base.proposals),
        ("decisions", &base.decisions),
        ("supports", &base.supports),
        ("identity", &base.identity),
        ("projections", &base.projections),
    ];
    for (role, graphs) in role_lists {
        if graphs.iter().any(|g| g == graph) {
            let p = [role.to_string()];
            if locked.iter().any(|l| starts_with(&p, l)) {
                refused.push(role.to_string());
            }
        }
    }
    for (name, policy) in &base.policies {
        for (field, matches) in [
            ("boundary", policy.boundary.iter().any(|g| g == graph)),
            ("decisions", policy.decisions == graph),
            ("supports", policy.supports == graph),
            ("output", policy.output == graph),
        ] {
            if matches {
                let p = ["policies".to_string(), name.clone(), field.to_string()];
                if let Some(l) = locked.iter().find(|l| starts_with(&p, l)) {
                    let s = path_string(l);
                    if !refused.contains(&s) {
                        refused.push(s);
                    }
                }
            }
        }
    }
    for (name, policy) in &base.matching_policies {
        for (field, matches) in [
            ("boundary", policy.boundary.iter().any(|g| g == graph)),
            ("decisions", policy.decisions == graph),
            ("maps", policy.maps == graph),
            ("output", policy.output == graph),
        ] {
            if matches {
                let p = [
                    "matchingPolicies".to_string(),
                    name.clone(),
                    field.to_string(),
                ];
                if let Some(l) = locked.iter().find(|l| starts_with(&p, l)) {
                    let s = path_string(l);
                    if !refused.contains(&s) {
                        refused.push(s);
                    }
                }
            }
        }
    }
    if !refused.is_empty() {
        return Err(locked_error_of(NAME, &refused, &format!("/{}", ds.name)));
    }
    Ok(())
}

/// Record complete settings and their sparse runtime layer together.
/// Must be called while holding the evidence settings and declaration guards.
pub fn configure_complete(
    st: &AppState,
    ds: &Dataset,
    settings: &Settings,
    control: &Control,
) -> ApiResult<SettingsStatus> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let canonical = settings.canonical(control)?;
    let declared = st.settings.declared();
    check_put_locks(&declared, &ds.name, canonical.value())?;
    let layer = if has_declared(&declared, &ds.name) {
        let cur = resolve(
            &EVIDENCE,
            &declared,
            &ds.name,
            Value::Object(Map::new()),
            Providers::Unchecked,
        );
        let base = normalize_value(&cur.base).unwrap_or(cur.base);
        Some(sparse_patch(
            diff(&base, canonical.value()).unwrap_or_default(),
        ))
    } else {
        None
    };
    Ok(sparkles::evidence::configure_layered(
        &ds.dataset,
        settings,
        layer,
        None,
        control,
    )?)
}

fn sparse_patch(mut layer: Value) -> Map<String, Value> {
    prune(&mut layer);
    match layer {
        Value::Object(layer) => layer,
        _ => Map::new(),
    }
}

/// A settings-specific error shared by PUT normalization and PATCH validation.
pub(super) fn write_error(e: String) -> crate::http::ApiError {
    if let Some(message) = e.strip_prefix("unsupported-profile: ") {
        return ManagedError::error(ManagedErrorKind::UnsupportedProfile, message).into();
    }
    bad(format!("{NAME}: {e}"))
}

/// Apply a C19 write (`PUT`, `PATCH`, `DELETE`) on `/$/settings/{ds}/evidence`.
/// Records the effective settings and runtime layer through `configure_layered`
/// so protection is never weakened and `evidence.json` stays canonical and synchronized
/// with `managed.json`.
pub fn write_c19(
    st: &AppState,
    ds: &Dataset,
    op: Op,
    if_match: Option<&str>,
) -> ApiResult<EvidenceState> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let lock = st.settings.write_lock(&ds.name, &EVIDENCE);
    let _g = lock.lock();
    let _declared = st.settings.hold_evidence_declarations();
    let declared = st.settings.declared();
    let (rt, revision) = runtime_at(ds, &declared)?;
    let cur = resolve(&EVIDENCE, &declared, &ds.name, rt, Providers::Unchecked);
    let owner = format!("/{}", ds.name);
    let is_put = matches!(op, Op::Put(_));
    let new = plan(&cur, op, if_match, &owner)?;
    if is_put && !cur.locked.is_empty() {
        let base = normalize_value(&cur.base).unwrap_or_else(|_| cur.base.clone());
        let after_raw = merged(&cur.base, &new);
        let after = normalize_value(&after_raw).unwrap_or(after_raw);
        let mut refused = Vec::new();
        for l in &cur.locked {
            if at(&after, l) != at(&base, l) {
                refused.push(path_string(l));
            }
        }
        if !refused.is_empty() {
            return Err(locked_error_of(NAME, &refused, &owner));
        }
    }
    let mut resolved = resolve(&EVIDENCE, &declared, &ds.name, new, Providers::Unchecked);
    if let Err(e) = &resolved.status {
        return Err(write_error(e.clone()));
    }
    let settings = parse_value(&resolved.effective)?;
    let declared_kind = has_declared(&declared, &ds.name);
    let layer = declared_kind.then(|| sparse_patch(resolved.runtime.clone()));
    let recorded = sparkles::evidence::configure_layered(
        &ds.dataset,
        &settings,
        layer,
        Some(revision),
        &Control::none(),
    )?;
    if !declared_kind {
        // With no declaration the response uses the newly recorded settings.
        resolved = resolve(
            &EVIDENCE,
            &declared,
            &ds.name,
            recorded_layer(&recorded)?,
            Providers::Unchecked,
        );
    }
    let drift = declared_kind && compute_drift(&resolved, &recorded)?;
    Ok(EvidenceState {
        resolved,
        recorded,
        drift,
    })
}
