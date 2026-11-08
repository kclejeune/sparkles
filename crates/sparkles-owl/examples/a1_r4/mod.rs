//! Private diagnostic shared by the matched direct-mimalloc and allocation examples.
//! No production observer, formula or semantic expectation is changed here.
use serde_json::{Value, json};
use sparkles_owl::classify::{Classification, Classifier, Consistency, Error, Limits, Stats};
use sparkles_owl::{Axiom, model::Class, ofn_read};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BUDGETS: [usize; 6] = [65536, 262144, 1048576, 4194304, 16777216, 67108864];
// This bounds diagnostic receipts, not engine memory. Exhaustion is a harness
// failure, never a budget refusal or a successful row. Storage is reserved before
// the allocator/timing window and must never grow during a charge callback.
const MAX_GRANTS: usize = 1_000_000;

pub trait Meter {
    const NAME: &'static str;
    fn begin();
    fn snapshot() -> Value;
    fn end() -> Value;
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value[key]
        .as_str()
        .ok_or_else(|| format!("missing string {key}"))
}

fn rss() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find(|l| l.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

fn limits(deadline: Instant) -> Limits {
    Limits {
        threads: 1,
        deadline: Some(deadline),
        partial: false,
        ..Limits::default()
    }
}

fn stats(s: &Stats) -> Value {
    json!({"retained_estimated_bytes": s.memory, "peak_estimated_bytes": s.peak_memory,
        "taxonomy_estimated_bytes": s.taxonomy_memory, "contexts": s.contexts,
        "conclusions": s.conclusions, "threads": s.threads, "tier": s.tier.as_str(),
        "horn_over_budget": s.horn_over_budget, "dl_over_budget": s.dl_over_budget,
        "dl_counting_over": s.dl_counting_over, "incremental": s.incremental})
}

fn fallback(s: &Stats) -> bool {
    s.horn_over_budget || s.dl_over_budget || s.dl_counting_over
}

fn error(e: &Error) -> Value {
    match e {
        Error::Memory { limit, used } => json!({"kind": "memory", "limit": limit, "used": used}),
        Error::Timeout => json!({"kind": "timeout"}),
        Error::Cancelled => json!({"kind": "cancelled"}),
    }
}

fn taxonomy(out: &Classification) -> Value {
    let mut nodes: Vec<_> = out
        .taxonomy
        .nodes()
        .iter()
        .filter(|n| !n.classes.is_empty())
        .map(|n| {
            let mut classes: Vec<_> = n.classes.iter().map(|c| c.as_str().to_owned()).collect();
            classes.sort();
            let mut parents: Vec<Vec<String>> = n
                .parents
                .iter()
                .map(|&id| {
                    let mut group: Vec<_> = out
                        .taxonomy
                        .node(id)
                        .classes
                        .iter()
                        .map(|c| c.as_str().to_owned())
                        .collect();
                    group.sort();
                    group
                })
                .collect();
            parents.sort();
            (classes, parents)
        })
        .collect();
    nodes.sort();
    Value::Array(
        nodes
            .into_iter()
            .map(|(classes, parents)| json!({"classes": classes, "parents": parents}))
            .collect(),
    )
}

// Separate untimed full classification; Saturated alone cannot certify completeness.
fn semantics(k: &mut Classifier, case: &Value, deadline: Instant) -> Value {
    match k.classify(&limits(deadline)) {
        Err(e) => json!({"admitted": false, "error": error(&e)}),
        Ok(out) => {
            let signature: BTreeSet<_> = out
                .taxonomy
                .nodes()
                .iter()
                .flat_map(|n| n.classes.iter().map(|c| c.as_str().to_owned()))
                .collect();
            let signature: Vec<_> = signature.into_iter().collect();
            let unsatisfiable: Vec<_> = out
                .taxonomy
                .unsatisfiable()
                .map(|c| c.as_str().to_owned())
                .collect();
            let actual_taxonomy = taxonomy(&out);
            let consistency = match out.consistency {
                Consistency::Consistent => "consistent",
                Consistency::Inconsistent => "inconsistent",
                Consistency::Unknown => "unknown",
            };
            let exclusions: Vec<_> = k.excluded().iter().map(|e| format!("{e:?}")).collect();
            let controls: Vec<_> = case["controls"].as_array().expect("sealed controls").iter().map(|c| {
                let a = Class::new(c[0].as_str().expect("class"));
                let b = Class::new(c[1].as_str().expect("class"));
                let actual = out.taxonomy.is_subclass(&a, &b);
                json!({"sub": a.as_str(), "sup": b.as_str(), "wanted": c[2], "actual": actual, "pass": Some(actual) == c[2].as_bool()})
            }).collect();
            let expected = &case["expected"];
            let checks = json!({"complete": out.complete && out.incomplete.is_empty(),
                "known_consistency": out.consistency != Consistency::Unknown,
                "consistent": Some(out.consistent) == expected["consistent"].as_bool(),
                "consistency_agrees": out.consistency == if out.consistent { Consistency::Consistent } else { Consistency::Inconsistent },
                "signature": json!(signature) == expected["signature"],
                "unsatisfiable": json!(unsatisfiable) == expected["unsatisfiable"],
                "whole_taxonomy": actual_taxonomy == expected["taxonomy_nodes"],
                "empty_exclusions": exclusions.is_empty(), "no_fallback": !fallback(&out.stats),
                "controls": controls.iter().all(|c| c["pass"] == true)});
            let admitted = checks
                .as_object()
                .expect("checks")
                .values()
                .all(|v| v == true);
            json!({"admitted": admitted, "checks": checks, "consistent": out.consistent,
                "consistency": consistency, "complete": out.complete,
                "incomplete": out.incomplete.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                "signature": signature, "unsatisfiable": unsatisfiable, "taxonomy_nodes": actual_taxonomy,
                "exclusions": exclusions, "controls": controls, "stats": stats(&out.stats)})
        }
    }
}

#[derive(Clone, Copy)]
struct Grant {
    requested: usize,
    before: usize,
    after: usize,
    accepted: bool,
}
struct Pool {
    grants: Vec<Grant>,
    total: usize,
    overflow: bool,
}

fn admission<M: Meter>(axioms: &[Axiom], case: &Value, row: &Value, deadline: Instant) -> Value {
    let pool = Arc::new(Mutex::new(Pool {
        grants: Vec::with_capacity(MAX_GRANTS),
        total: 0,
        overflow: false,
    }));
    let observed = pool.clone();
    let kind = row["kind"].as_str().expect("sealed kind");
    let budget = row["budget"].as_u64().map(|v| v as usize);
    let pool_limit = if kind == "pool" { budget } else { None };
    let charge = Arc::new(move |requested: usize| {
        let mut p = observed.lock().expect("charge mutex");
        if p.grants.len() == MAX_GRANTS {
            p.overflow = true;
            return false;
        }
        let before = p.total;
        let sum = before.checked_add(requested);
        let accepted = sum.is_some_and(|n| pool_limit.is_none_or(|cap| n <= cap));
        let after = if accepted {
            sum.expect("checked grant")
        } else {
            before
        };
        p.total = after;
        p.grants.push(Grant {
            requested,
            before,
            after,
            accepted,
        });
        accepted
    });
    let mut bounded = limits(deadline);
    bounded.max_memory = if kind == "hard" { budget } else { None };
    bounded.charge = Some(charge);
    // Allocation pass includes classifier construction; timing includes saturate only.
    // Parse, receipt storage, callback closure and limits are outside both windows.
    M::begin();
    let mut classifier = Classifier::of(axioms);
    let allocation_after_construction = M::snapshot();
    let t = Instant::now();
    let result = classifier.saturate(&bounded);
    let elapsed = t.elapsed().as_secs_f64();
    let allocations = M::end();
    let exclusions: Vec<_> = classifier
        .excluded()
        .iter()
        .map(|e| format!("{e:?}"))
        .collect();
    let (outcome, saturation, resource_ok) = match &result {
        Ok(out) => {
            let under_hard =
                kind != "hard" || out.stats.peak_memory <= budget.expect("hard budget");
            let clean = !fallback(&out.stats) && exclusions.is_empty() && under_hard;
            (
                if clean {
                    "saturated"
                } else {
                    "fallback-or-resource-failure"
                },
                json!({"consistent": out.consistent, "stats": stats(&out.stats)}),
                clean,
            )
        }
        Err(Error::Memory { limit, used }) => {
            let proper = if kind == "hard" {
                Some(*limit) == budget && used > limit
            } else {
                kind == "pool"
            };
            (
                "refused",
                json!({"error": error(result.as_ref().err().expect("error")), "retained_estimated_bytes": Value::Null,
                "peak_estimated_bytes": Value::Null, "estimate_unavailable_reason": "public error result has no Stats"}),
                proper,
            )
        }
        Err(e) => ("engine-error", json!({"error": error(e)}), false),
    };
    // Drop this run's callback owner before unrestricted same-object reuse. A stale
    // callback retained internally remains observable through the independent pool.
    drop(bounded);
    let p = pool.lock().expect("pool");
    let grants_before_reuse = p.grants.len();
    let total_before_reuse = p.total;
    let refused_grant = p.grants.iter().any(|g| !g.accepted);
    let overflow = p.overflow;
    let grants: Vec<_> = p.grants.iter().map(|g| json!({"requested": g.requested, "before": g.before, "after": g.after, "accepted": g.accepted})).collect();
    drop(p);
    let reuse = semantics(&mut classifier, case, deadline);
    let p = pool.lock().expect("pool");
    let grants_after_reuse = p.grants.len();
    let total_after_reuse = p.total;
    let overflow_after_reuse = p.overflow;
    let no_stale_charge = grants_after_reuse == grants_before_reuse
        && total_after_reuse == total_before_reuse
        && overflow_after_reuse == overflow;
    drop(p);
    let fresh = semantics(&mut Classifier::of(axioms), case, deadline);
    let same_answer = reuse["taxonomy_nodes"] == fresh["taxonomy_nodes"]
        && reuse["consistency"] == fresh["consistency"]
        && reuse["complete"] == fresh["complete"]
        && reuse["incomplete"] == fresh["incomplete"]
        && reuse["exclusions"] == fresh["exclusions"];
    let same_retained = reuse["stats"]["retained_estimated_bytes"].is_u64()
        && reuse["stats"]["retained_estimated_bytes"] == fresh["stats"]["retained_estimated_bytes"];
    let consistent_saturation = match &result {
        Ok(out) => Some(out.consistent) == fresh["consistent"].as_bool(),
        Err(_) => true,
    };
    let refusal_obeyed = if refused_grant {
        matches!(result, Err(Error::Memory { .. }))
    } else {
        kind != "pool" || !matches!(result, Err(Error::Memory { .. }))
    };
    let evidence_ok = resource_ok
        && !overflow
        && refusal_obeyed
        && no_stale_charge
        && same_answer
        && same_retained
        && consistent_saturation;
    let semantic_ok = reuse["admitted"] == true && fresh["admitted"] == true;
    json!({"outcome": outcome, "row_completed": true, "resource_evidence_ok": evidence_ok, "semantic_admitted": semantic_ok,
        "admitted": evidence_ok && semantic_ok, "refusal_is_correctness_pass": false,
        "elapsed_seconds": elapsed, "saturation": saturation, "exclusions_after_saturation": exclusions,
        "grants": grants, "cumulative_granted_bytes": total_before_reuse, "refused_grant": refused_grant,
        "grant_count_after_reuse": grants_after_reuse, "cumulative_granted_bytes_after_reuse": total_after_reuse,
        "grant_receipt_overflow_after_reuse": overflow_after_reuse,
        "grant_receipt_overflow": overflow, "refusal_obeyed": refusal_obeyed, "no_stale_charge": no_stale_charge,
        "reuse_matches_fresh": same_answer, "retained_estimate_matches_fresh": same_retained,
        "saturation_consistency_matches_fresh": consistent_saturation,
        "unrestricted_same_object": reuse, "unrestricted_fresh": fresh,
        "allocations_after_construction": allocation_after_construction, "allocations_after_saturate": allocations,
        "allocation_window": "classifier construction plus saturation; parsing/receipt storage/semantic checks excluded",
        "allocation_pass_is_timing_evidence": false})
}

fn throughput(axioms: &[Axiom], case: &Value, deadline: Instant) -> Value {
    let mut samples = Vec::new();
    for index in 0..10 {
        let mut classifier = Classifier::of(axioms);
        let run_limits = limits(deadline);
        let t = Instant::now();
        let result = classifier.saturate(&run_limits);
        let elapsed = t.elapsed().as_secs_f64();
        let exclusions: Vec<_> = classifier
            .excluded()
            .iter()
            .map(|e| format!("{e:?}"))
            .collect();
        let sample = match result {
            Ok(out) => json!({"index": index, "warmup": index < 2, "elapsed_seconds": elapsed,
                "ok": !fallback(&out.stats) && exclusions.is_empty() && Some(out.consistent) == case["expected"]["consistent"].as_bool(),
                "consistent": out.consistent, "stats": stats(&out.stats), "exclusions": exclusions}),
            Err(e) => {
                json!({"index": index, "warmup": index < 2, "elapsed_seconds": elapsed, "ok": false, "error": error(&e)})
            }
        };
        let ok = sample["ok"] == true;
        samples.push(sample);
        if !ok {
            break;
        }
    }
    let semantic = semantics(&mut Classifier::of(axioms), case, deadline);
    let admitted = samples.len() == 10
        && samples.iter().all(|s| s["ok"] == true)
        && semantic["admitted"] == true;
    json!({"admitted": admitted, "samples": samples, "semantic": semantic,
        "warmups_required": 2, "timed_required": 8, "charge_callback": false,
        "precision_ratio_eligible": case["precision"] == false})
}

fn execute<M: Meter>(options: &Value) -> Result<Value, String> {
    let cases_path = Path::new(field(options, "--cases")?);
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(cases_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if manifest["schema"] != "a1-r4-cases/v1"
        || manifest["cases"].as_array().map(Vec::len) != Some(48)
        || manifest["rows"].as_array().map(Vec::len) != Some(624)
    {
        return Err("wrong fixed matrix".into());
    }
    let index: usize = field(options, "--row")?
        .parse()
        .map_err(|_| "invalid row")?;
    let row = manifest["rows"].get(index).ok_or("row outside 0..624")?;
    if row["row"].as_u64() != Some(index as u64) {
        return Err("wrong row identity".into());
    }
    let kind = if index < 48 {
        "unbounded"
    } else if index < 336 {
        "hard"
    } else {
        "pool"
    };
    let budget = if index < 48 {
        Value::Null
    } else {
        json!(BUDGETS[(index - 48) / 48 % 6])
    };
    if row["kind"] != kind
        || row["budget"] != budget
        || row["case"] != manifest["cases"][index % 48]["id"]
    {
        return Err("altered matrix row".into());
    }
    let case = &manifest["cases"][index % 48];
    let relative = Path::new(field(case, "input")?);
    if relative.is_absolute()
        || relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return Err("nonlocal input".into());
    }
    let input = cases_path.parent().ok_or("cases parent")?.join(relative);
    let t = Instant::now();
    let text = std::fs::read_to_string(&input).map_err(|e| e.to_string())?;
    let doc = ofn_read::parse(&text).map_err(|e| e.to_string())?;
    if !doc.header.imports.is_empty()
        || doc.rules != 0
        || Some(doc.axioms.len() as u64) != case["axiom_count"].as_u64()
    {
        return Err("wrong axioms/imports/rules".into());
    }
    let read_seconds = t.elapsed().as_secs_f64();
    let mode = field(options, "--mode")?;
    let role = field(options, "--role")?;
    if !["baseline", "candidate"].contains(&role) {
        return Err("wrong source role".into());
    }
    let round: usize = field(options, "--round")?
        .parse()
        .map_err(|_| "invalid round")?;
    if round > 3 {
        return Err("round outside 0..4".into());
    }
    let deadline_seconds = if mode == "admission" { 8 } else { 2700 };
    let deadline = Instant::now() + Duration::from_secs(deadline_seconds);
    let mut evidence = match mode {
        "admission" => admission::<M>(&doc.axioms, case, row, deadline),
        "throughput" if M::NAME == "uninstrumented-mimalloc" && index < 48 => {
            throughput(&doc.axioms, case, deadline)
        }
        _ => return Err("invalid mode, instrumented timing, or bounded throughput row".into()),
    };
    // Baseline precision disagreement remains an explicit unresolved capability
    // limitation. It is never admitted, comparable or included as a speed ratio.
    if role == "baseline" && case["precision"] == true && evidence["admitted"] != true {
        evidence["baseline_precision_unresolved"] = json!(true);
    }
    Ok(
        json!({"schema": "a1-r4-row/v1", "role": role, "mode": mode, "round": round,
        "row": row, "case": case["id"], "precision": case["precision"], "metrics": case["metrics"],
        "input": input, "declared_input_sha256": case["input_sha256"], "read_seconds": read_seconds,
        "allocator": M::NAME, "process_peak_rss_kib": rss(), "rss_is_requested_allocation": false,
        "engine_deadline_seconds": deadline_seconds,
        "external_complete_row_cap_required_seconds": if mode == "admission" { 10 } else { 2700 },
        "evidence": evidence}),
    )
}

pub fn main<M: Meter>() {
    let mut options = json!({});
    let mut args = std::env::args().skip(1);
    while let Some(key) = args.next() {
        if ![
            "--cases", "--mode", "--round", "--output", "--row", "--role",
        ]
        .contains(&key.as_str())
            || !options[&key].is_null()
        {
            eprintln!("unknown/duplicate argument {key}");
            std::process::exit(2);
        }
        let Some(value) = args.next() else {
            eprintln!("missing {key} value");
            std::process::exit(2);
        };
        options[&key] = json!(value);
    }
    let output = field(&options, "--output").expect("--output required");
    // Reserve output exclusively before parsing or touching an engine.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .expect("fresh output file");
    let result = execute::<M>(&options);
    let record = match result {
        Ok(record) => record,
        Err(message) => {
            json!({"schema": "a1-r4-row/v1", "harness_error": message, "options": options})
        }
    };
    serde_json::to_writer(&mut file, &record).expect("write row");
    writeln!(file).expect("write newline");
    file.sync_all().expect("sync row");
    if record["evidence"]["admitted"] != true {
        std::process::exit(1);
    }
}
