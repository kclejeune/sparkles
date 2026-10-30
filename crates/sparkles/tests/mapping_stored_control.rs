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
fn stored_run_stopped_during_conversion_commits_nothing() {
    let (_dir, inputs) = input(200_000);
    let ds = stored(MAPPING);
    let flag = Arc::new(AtomicBool::new(false));
    let (opts, calls) = counting();
    let trip = flag.clone();
    let seen = calls.clone();
    let opts = RunOptions {
        check: Some(Arc::new(move || {
            // Stop through the write options once conversion has begun.
            if seen.load(Ordering::SeqCst) >= 1 {
                trip.store(true, Ordering::SeqCst);
            }
            opts.check.as_ref().unwrap()()
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
