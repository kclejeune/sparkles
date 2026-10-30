//! Structured write controls stop stored mapping runs before conversion, as they do
//! for direct runs, and a published stored run keeps its receipt and run record.
use sparkles::guard::WriteOptions;
use sparkles::mapping::{Inputs, RunOptions};
use sparkles::stored_mappings::{Change, Definition};
use sparkles::task::Cancellation;
use sparkles::{Dataset, Error};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

const MAPPING: &str = "prefixes:\n  ex: http://ex.org/\nmappings:\n  person:\n    sources:\n      - [people.csv~csv]\n    s: ex:person/$(id)\n    po:\n      - [ex:name, $(name)]\n";

fn input(rows: usize) -> (tempfile::TempDir, Inputs) {
    let dir = tempfile::tempdir().unwrap();
    let mut csv = String::from("id,name\n");
    for i in 0..rows {
        csv.push_str(&format!("{i},n{i}\n"));
    }
    std::fs::write(dir.path().join("people.csv"), csv).unwrap();
    let inputs = Inputs {
        dir: Some(dir.path().into()),
        ..Default::default()
    };
    (dir, inputs)
}

fn stored(text: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.mappings()
        .put(
            "m",
            Definition {
                kind: "yarrrml".into(),
                text: text.into(),
                description: None,
            },
            Change {
                author: None,
                message: None,
                if_version: None,
                dataset_commit: None,
            },
        )
        .unwrap();
    ds
}

/// Conversion options whose callback counts its calls.
fn counting() -> (RunOptions, Arc<AtomicU64>) {
    let calls = Arc::new(AtomicU64::new(0));
    let c = calls.clone();
    (
        RunOptions {
            check: Some(Arc::new(move || {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })),
            ..Default::default()
        },
        calls,
    )
}

/// A named stopped control and the refusal it must produce.
type Stopped = (&'static str, WriteOptions, fn(&Error) -> bool);

fn stopped() -> Vec<Stopped> {
    let past = Instant::now()
        .checked_sub(Duration::from_millis(1))
        .unwrap();
    vec![
        (
            "cancel",
            WriteOptions {
                cancel: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
            |e| matches!(e, Error::Cancelled),
        ),
        (
            "cancellation",
            WriteOptions {
                cancellation: Some(Cancellation::from_flags([Arc::new(AtomicBool::new(true))])),
                ..Default::default()
            },
            |e| matches!(e, Error::Cancelled),
        ),
        (
            "deadline",
            WriteOptions {
                deadline: Some(past),
                ..Default::default()
            },
            |e| matches!(e, Error::Timeout),
        ),
    ]
}

#[test]
fn stopped_stored_run_refuses_before_conversion() {
    // Sources check their callback periodically, so the input spans several checkpoints.
    let (_dir, inputs) = input(150_000);
    let replace = MAPPING.replace("    po:", "    graphs: ex:g\n    po:");
    for (text, mode) in [(MAPPING, false), (replace.as_str(), true)] {
        for (name, write, expected) in stopped() {
            let ds = stored(text);
            let (opts, calls) = counting();
            let error = ds
                .mappings()
                .run("m", None, &inputs, &opts, mode, &write)
                .unwrap_err();
            assert!(expected(&error), "{name} replace={mode}: {error}");
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "{name} replace={mode}: the run converted before refusing"
            );
            assert!(ds.is_empty(), "{name} replace={mode}");
            assert!(
                ds.mappings().last_run("m").is_none(),
                "{name} replace={mode}"
            );
        }
    }
}

#[test]
fn stored_run_stopped_during_conversion_stops_at_the_next_checkpoint() {
    let (_dir, inputs) = input(200_000);
    // An unstopped run of the same input reaches every conversion checkpoint.
    let (baseline, total) = counting();
    stored(MAPPING)
        .mappings()
        .run(
            "m",
            None,
            &inputs,
            &baseline,
            false,
            &WriteOptions::default(),
        )
        .unwrap();
    let total = total.load(Ordering::SeqCst);
    assert!(total >= 2, "the input reaches only {total} checkpoints");

    let ds = stored(MAPPING);
    let flag = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU64::new(0));
    let (trip, seen) = (flag.clone(), calls.clone());
    // The caller's callback sets the write options' flag at its first checkpoint and
    // never refuses itself, so only the write controls can stop conversion.
    let opts = RunOptions {
        check: Some(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
            trip.store(true, Ordering::SeqCst);
            Ok(())
        })),
        ..Default::default()
    };
    let write = WriteOptions {
        cancel: Some(flag),
        ..Default::default()
    };
    let error = ds
        .mappings()
        .run("m", None, &inputs, &opts, false, &write)
        .unwrap_err();
    assert!(matches!(error, Error::Cancelled), "{error}");
    // The next checkpoint refuses before the callback runs again. A run that ignored
    // the write controls during conversion would reach all of them and refuse later.
    assert_eq!(calls.load(Ordering::SeqCst), 1, "of {total} checkpoints");
    assert!(ds.is_empty());
    assert!(ds.mappings().last_run("m").is_none());
}

#[test]
fn unstopped_stored_run_publishes_and_records() {
    let (_dir, inputs) = input(3);
    let ds = stored(MAPPING);
    let (opts, _calls) = counting();
    let write = WriteOptions {
        cancel: Some(Arc::new(AtomicBool::new(false))),
        deadline: Instant::now().checked_add(Duration::from_secs(60)),
        ..Default::default()
    };
    let run = ds
        .mappings()
        .run("m", None, &inputs, &opts, false, &write)
        .unwrap();
    assert!(run.receipt.committed);
    assert_eq!(run.stats.quads, 3);
    assert_eq!(ds.len(), 3);
    assert_eq!(
        ds.mappings().last_run("m").unwrap().commit,
        Some(run.receipt.commit.seq)
    );
}
