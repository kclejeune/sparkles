//! The `evidence` settings kind (spec C25 §7, C19): declared evidence settings,
//! field locks, and drift refusal between the C19 effective settings and the dataset's
//! recorded `evidence.json` / `managed.json`.
//!
//! `evidence.json` always holds the complete canonical object whose SHA-256 digest
//! `managed.json` records, and is written only through [`sparkles::evidence::configure`],
//! which never weakens protection. When `serve --settings` declares evidence settings or
//! locks for a dataset, explicit runtime overrides of unlocked fields live in
//! `evidence-overrides.json` (or `AppState::volatile` for in-memory datasets), keeping
//! the file empty and absent whenever the recorded settings match the declared layer.
//! While the effective settings differ from the recorded ones, typed evidence operations
//! refuse with `409 evidence-settings-drift` without weakening protection.

use super::http::{Op, bad, locked_error_of, plan};
use super::merge::{at, diff, merged, path_string, prune, starts_with};
use super::{Declared, Kind, Providers, Resolved, Scope, resolve};
use crate::http::ApiResult;
use crate::state::{AppState, Dataset};
use axum::http::StatusCode;
use serde_json::{Map, Value, json};
use sparkles::evidence::{Settings, SettingsStatus};
use sparkles::managed::{ManagedError, ManagedErrorKind};
use sparkles::task::Control;
use std::sync::Arc;

/// The kind's name in `serve --settings` and `/$/settings/{ds}/evidence`.
pub const NAME: &str = "evidence";

/// The sparse C19 runtime override file kept beside `evidence.json` when declared
/// settings exist and an unlocked field is overridden at runtime.
pub const OVERRIDES_FILE: &str = "evidence-overrides.json";

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
    file: OVERRIDES_FILE,
    members: MEMBERS,
    removable: &["policies", "matchingPolicies"],
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

/// Read the sparse runtime override layer from `evidence-overrides.json`.
fn read_overrides(st: &AppState, ds: &Dataset) -> anyhow::Result<Value> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let v = crate::assist::read_file(st, ds, OVERRIDES_FILE)?;
    Ok(match v {
        None => Value::Object(Map::new()),
        Some(Value::Object(mut m)) => {
            if let Some(f) = sparkles::formats::settings_file(OVERRIDES_FILE) {
                m.remove(f.member);
            }
            Value::Object(m)
        }
        Some(_) => anyhow::bail!("{OVERRIDES_FILE} of {} is not a JSON object", ds.name),
    })
}

/// Store or remove `evidence-overrides.json`.
fn write_overrides(st: &AppState, ds: &Dataset, layer: &Value) -> anyhow::Result<()> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let m = match layer {
        Value::Object(m) => m,
        _ => return crate::assist::remove_file(st, ds, OVERRIDES_FILE),
    };
    if m.is_empty() {
        crate::assist::remove_file(st, ds, OVERRIDES_FILE)
    } else {
        crate::assist::write_file(st, ds, OVERRIDES_FILE, layer)
    }
}

/// The runtime layer of `evidence` for C19 resolution:
/// - When `serve --settings` declares values or locks for this dataset, the sparse
///   overrides in `evidence-overrides.json`.
/// - Otherwise, the diff of the recorded `evidence.json` against built-in defaults.
pub fn runtime_layer(st: &AppState, ds: &Dataset) -> anyhow::Result<Value> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let declared = st.settings.declared();
    if has_declared(&declared, &ds.name) {
        return read_overrides(st, ds);
    }
    let status = sparkles::evidence::settings(&ds.dataset)?;
    if status.digest.is_none() {
        return Ok(Value::Object(Map::new()));
    }
    let recorded = status.settings.canonical(&Control::none())?.value().clone();
    Ok(diff(&defaults_value(), &recorded).unwrap_or_else(|| Value::Object(Map::new())))
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
    inspect_declared(st, ds, &st.settings.declared())
}

fn inspect_declared(
    st: &AppState,
    ds: &Dataset,
    declared: &Declared,
) -> sparkles::Result<EvidenceState> {
    let recorded = sparkles::evidence::settings(&ds.dataset)?;
    if !has_declared(declared, &ds.name) {
        let rt = if recorded.digest.is_some() {
            let rec = recorded.settings.canonical(&Control::none())?;
            diff(&defaults_value(), rec.value()).unwrap_or_else(|| Value::Object(Map::new()))
        } else {
            Value::Object(Map::new())
        };
        let resolved = resolve(&EVIDENCE, declared, &ds.name, rt, Providers::Unchecked);
        return Ok(EvidenceState {
            resolved,
            recorded,
            drift: false,
        });
    }
    let rt = read_overrides(st, ds).map_err(|e| {
        ManagedError::error(
            ManagedErrorKind::Unavailable,
            format!("cannot read {OVERRIDES_FILE} of /{}: {e:#}", ds.name),
        )
    })?;
    let resolved = resolve(&EVIDENCE, declared, &ds.name, rt, Providers::Unchecked);
    let drift = compute_drift(&resolved, &recorded)?;
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
    if has_declared(&declared, &ds.name) && inspect_declared(st, ds, &declared)?.drift {
        return Err(drift_error(&ds.name));
    }
    Ok(())
}

/// The state seen by a typed write. The publication fence checks later declarations
/// against this snapshot without taking a store lock on the committing thread.
#[derive(Clone)]
pub struct DriftFence {
    dataset: String,
    declared_at_start: Arc<Declared>,
    runtime_at_start: Value,
    recorded: Value,
}

impl DriftFence {
    /// Capture the current declared and recorded state after verifying no drift exists.
    pub fn capture(st: &AppState, ds: &Dataset) -> sparkles::Result<Self> {
        let main = main_of(ds);
        let main_ds = main.as_deref().unwrap_or(ds);
        let declared_at_start = st.settings.declared();
        let (runtime_at_start, recorded) = if has_declared(&declared_at_start, &main_ds.name) {
            let state = inspect_declared(st, main_ds, &declared_at_start)?;
            if state.drift {
                return Err(drift_error(&main_ds.name));
            }
            let recorded = state.recorded.settings.canonical(&Control::none())?;
            (state.resolved.runtime, recorded.value().clone())
        } else {
            (Value::Object(Map::new()), Value::Null)
        };
        Ok(Self {
            dataset: main_ds.name.clone(),
            declared_at_start,
            runtime_at_start,
            recorded,
        })
    }

    /// Re-check immediately before publication while holding the declarations guard.
    pub fn check(&self, st: &AppState) -> sparkles::Result<()> {
        let now = st.settings.declared();
        if Arc::ptr_eq(&now, &self.declared_at_start) {
            return Ok(());
        }
        if !has_declared(&now, &self.dataset) {
            return Ok(());
        }
        if !has_declared(&self.declared_at_start, &self.dataset) {
            // A newly declared layer can activate overrides left from an earlier
            // declaration. Re-admit the operation so it reads that layer.
            return Err(drift_error(&self.dataset));
        }
        let resolved = resolve(
            &EVIDENCE,
            &now,
            &self.dataset,
            self.runtime_at_start.clone(),
            Providers::Unchecked,
        );
        if resolved.status.is_err() || resolved.effective != self.recorded {
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
/// is required by a locked role or locked policy field of the effective settings.
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
    let rt = read_overrides(st, ds).map_err(|e| {
        ManagedError::error(
            ManagedErrorKind::Unavailable,
            format!("cannot read {OVERRIDES_FILE} of /{}: {e:#}", ds.name),
        )
    })?;
    let resolved = resolve(&EVIDENCE, &declared, &ds.name, rt, Providers::Unchecked);
    let enabled_locked = locked
        .iter()
        .any(|l| l.is_empty() || (l.len() == 1 && l[0] == "enabled"));
    if !enabled_locked || resolved.base["enabled"] != Value::Bool(true) {
        return Ok(());
    }
    let Ok(eff) = parse_value(&resolved.effective) else {
        return Err(drift_error(&ds.name).into());
    };
    let mut refused: Vec<String> = Vec::new();
    if locked.iter().any(Vec::is_empty) {
        if eff.managed_graphs().iter().any(|g| g == graph) {
            return Err(locked_error_of(
                NAME,
                &[String::new()],
                &format!("/{}", ds.name),
            ));
        }
        return Ok(());
    }
    let role_lists: [(&str, &[String]); 6] = [
        ("sourceRecords", &eff.source_records),
        ("proposals", &eff.proposals),
        ("decisions", &eff.decisions),
        ("supports", &eff.supports),
        ("identity", &eff.identity),
        ("projections", &eff.projections),
    ];
    for (role, graphs) in role_lists {
        if graphs.iter().any(|g| g == graph) {
            let p = [role.to_string()];
            if locked.iter().any(|l| starts_with(&p, l)) {
                refused.push(role.to_string());
            }
        }
    }
    for (name, policy) in &eff.policies {
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
    for (name, policy) in &eff.matching_policies {
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

/// Record `settings` through `sparkles::evidence::configure` and sync the C19 override
/// layer. Must be called while holding `st.settings.write_lock(&main.name, &EVIDENCE)`.
pub fn configure_and_sync(
    st: &AppState,
    ds: &Dataset,
    settings: &Settings,
    control: &Control,
) -> ApiResult<SettingsStatus> {
    let main = main_of(ds);
    let ds = main.as_deref().unwrap_or(ds);
    let canonical = settings.canonical(control).map_err(|e| match e {
        sparkles::Error::Unsupported(message) => ManagedError::error(
            ManagedErrorKind::UnsupportedProfile,
            message
                .strip_prefix("unsupported-profile: ")
                .unwrap_or(&message)
                .to_string(),
        ),
        e => e,
    })?;
    let declared = st.settings.declared();
    check_put_locks(&declared, &ds.name, canonical.value())?;
    let status = sparkles::evidence::configure(&ds.dataset, settings, control)?;
    sync_overrides(st, ds, &declared, canonical.value())?;
    Ok(status)
}

fn sync_overrides(
    st: &AppState,
    ds: &Dataset,
    declared: &Declared,
    recorded_canonical: &Value,
) -> ApiResult<()> {
    let internal =
        |e: anyhow::Error| crate::http::err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
    if has_declared(declared, &ds.name) {
        let cur = resolve(
            &EVIDENCE,
            declared,
            &ds.name,
            Value::Object(Map::new()),
            Providers::Unchecked,
        );
        let base = normalize_value(&cur.base).unwrap_or(cur.base);
        let mut layer =
            diff(&base, recorded_canonical).unwrap_or_else(|| Value::Object(Map::new()));
        prune(&mut layer);
        write_overrides(st, ds, &layer).map_err(internal)?;
    } else {
        write_overrides(st, ds, &Value::Object(Map::new())).map_err(internal)?;
    }
    Ok(())
}

/// Apply a C19 write (`PUT`, `PATCH`, `DELETE`) on `/$/settings/{ds}/evidence`.
/// Always records the resulting effective settings through `sparkles::evidence::configure`
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
    let internal =
        |e: anyhow::Error| crate::http::err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
    let lock = st.settings.write_lock(&ds.name, &EVIDENCE);
    let _g = lock.lock();
    let _declared = st.settings.hold_evidence_declarations();
    let declared = st.settings.declared();
    let rt = runtime_layer(st, ds).map_err(|e| match e.downcast::<sparkles::Error>() {
        Ok(se) => se.into(),
        Err(e) => internal(e),
    })?;
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
    let r = resolve(&EVIDENCE, &declared, &ds.name, new, Providers::Unchecked);
    if let Err(e) = &r.status {
        if e.contains("unsupported-profile") {
            return Err(ManagedError::error(
                ManagedErrorKind::UnsupportedProfile,
                e.strip_prefix("unsupported-profile: ")
                    .unwrap_or(e)
                    .to_string(),
            )
            .into());
        }
        return Err(bad(format!("{NAME}: {e}")));
    }
    let settings = parse_value(&r.effective).map_err(|e| match e {
        sparkles::Error::Invalid(m) => bad(format!("{NAME}: {m}")),
        e => e.into(),
    })?;
    let recorded = sparkles::evidence::configure(&ds.dataset, &settings, &Control::none())?;
    let layer = if has_declared(&declared, &ds.name) {
        r.runtime
    } else {
        Value::Object(Map::new())
    };
    write_overrides(st, ds, &layer).map_err(internal)?;
    let final_rt = runtime_layer(st, ds).map_err(|e| match e.downcast::<sparkles::Error>() {
        Ok(se) => se.into(),
        Err(e) => internal(e),
    })?;
    let resolved = resolve(
        &EVIDENCE,
        &declared,
        &ds.name,
        final_rt,
        Providers::Unchecked,
    );
    let drift = compute_drift(&resolved, &recorded)?;
    Ok(EvidenceState {
        resolved,
        recorded,
        drift,
    })
}
