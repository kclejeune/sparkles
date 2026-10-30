//! Native direct mapping controls at real conversion, writer and publication phases.
use sparkles::guard::{
    Candidate, CommitGuard, GuardMode, GuardStatus, Severity, ValidationSummary, WriteOptions,
};
use sparkles::mapping::{Inputs, MappingKind, RunOptions};
use sparkles::task::Cancel;
#[cfg(feature = "failpoints")]
use sparkles::task::Cancellation;
use sparkles::{Dataset, Error};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

const MAPPING: &str = "prefixes:\n  ex: http://ex.org/\nmappings:\n  person:\n    sources:\n      - [people.csv~csv]\n    s: ex:person/$(id)\n    po:\n      - [ex:name, $(name)]\n";
fn input() -> (tempfile::TempDir, Inputs) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("people.csv"), "id,name\n1,Ann\n2,Bo\n").unwrap();
    let inputs = Inputs {
        dir: Some(dir.path().into()),
        ..Default::default()
    };
    (dir, inputs)
}
fn dataset(seeded: bool) -> Dataset {
    let ds = Dataset::memory();
    if seeded {
        ds.load_str(
            "<urn:old> <urn:predicate> <urn:value> .",
            oxrdfio::RdfFormat::NTriples,
        )
        .unwrap();
    }
    ds
}
#[derive(Default)]
struct Gate {
    state: Mutex<(bool, bool)>,
    changed: Condvar,
}
impl Gate {
    fn pause(&self) {
        let mut state = self.state.lock().unwrap();
        state.0 = true;
        self.changed.notify_all();
        while !state.1 {
            state = self.changed.wait(state).unwrap();
        }
    }
    fn reached(&self) -> bool {
        let (state, _) = self
            .changed
            .wait_timeout_while(self.state.lock().unwrap(), Duration::from_secs(10), |s| {
                !s.0
            })
            .unwrap();
        state.0
    }
    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        state.1 = true;
        self.changed.notify_all();
    }
}
#[cfg(feature = "failpoints")]
#[derive(Clone, Copy, Debug)]
enum Stop {
    Cancel,
    Composite,
    Deadline,
}
fn wait_for_deadline(deadline: Instant) {
    while Instant::now() < deadline {
        std::thread::park_timeout(deadline.saturating_duration_since(Instant::now()));
    }
}

#[cfg(feature = "failpoints")]
fn writer_refusal(stop: Stop) {
    let (_dir, inputs) = input();
    for seeded in [false, true] {
        let ds = dataset(seeded);
        let before = ds.store().snapshot();
        let head = before.commit;
        let len = before.len();
        let held = ds
            .store()
            .try_write_with(sparkles::commit::CommitKind::Load, WriteOptions::default())
            .unwrap();
        let primary = Cancel::new();
        let independent = Cancel::new();
        let deadline =
            matches!(stop, Stop::Deadline).then(|| Instant::now() + Duration::from_secs(3));
        let write = WriteOptions {
            cancel: Some(primary.flag()),
            cancellation: Some(Cancellation::from_flags([
                primary.flag(),
                independent.flag(),
            ])),
            deadline,
            ..Default::default()
        };
        let gate = Arc::new(Gate::default());
        let callback = gate.clone();
        ds.store().set_failpoint(
            "writer-acquiring",
            Some(Arc::new(move |_| callback.pause())),
        );
        let worker_ds = ds.clone();
        let worker_inputs = inputs.clone();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = worker_ds.run_mapping_with(
                MAPPING,
                None,
                &worker_inputs,
                &RunOptions::default(),
                None,
                &write,
            );
            sender.send(result).ok();
        });
        let entered = gate.reached();
        let actual_wait = ds.store().writers_waiting() > 0;
        // Let the actual lock acquisition proceed against the held writer before
        // requesting a stop. The failpoint proves phase entry, not a fake wait.
        gate.release();
        match stop {
            Stop::Cancel => primary.cancel(),
            Stop::Composite => independent.cancel(),
            Stop::Deadline => wait_for_deadline(deadline.unwrap()),
        }
        // Keep the writer held while the controlled operation must settle.
        let settled = receiver.recv_timeout(Duration::from_secs(1));
        let unchanged = ds.store().snapshot().commit == head && ds.len() == len;
        drop(held);
        ds.store().set_failpoint("writer-acquiring", None);
        let while_held = match settled {
            Ok(result) => Some(result),
            Err(_) => {
                let _ = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
                None
            }
        };
        worker.join().unwrap();
        assert!(
            entered && actual_wait,
            "final writer phase not latched: {stop:?}, seeded={seeded}"
        );
        assert!(unchanged, "pending mapping changed committed state");
        let error = while_held
            .expect("mapping did not settle before writer release")
            .unwrap_err();
        assert!(
            if matches!(stop, Stop::Deadline) {
                matches!(error, Error::Timeout)
            } else {
                matches!(error, Error::Cancelled)
            },
            "{stop:?}: {error}"
        );
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), len);
    }
}
#[cfg(feature = "failpoints")]
#[test]
fn regression_final_writer_cancellation_settles_before_release() {
    writer_refusal(Stop::Cancel);
}
#[cfg(feature = "failpoints")]
#[test]
fn regression_final_writer_deadline_settles_before_release() {
    writer_refusal(Stop::Deadline);
}
#[cfg(feature = "failpoints")]
#[test]
fn independent_cancellation_member_survives_final_writer_wait() {
    writer_refusal(Stop::Composite);
}

#[test]
fn old_exhaustive_options_and_method_signature_keep_fixed_output() {
    let (_dir, inputs) = input();
    let ds = Dataset::memory();
    // Every original public field is spelled out, without struct-update syntax.
    let options = RunOptions {
        check: None,
        max_decompressed: None,
        max_json_bytes: sparkles::mapping::DEFAULT_MAX_JSON_BYTES,
        memory_budget: None,
        base: None,
        max_output_bytes: None,
        only_graph: None,
    };
    type OldRun = fn(
        &Dataset,
        &str,
        Option<MappingKind>,
        &Inputs,
        &RunOptions,
        Option<&str>,
    ) -> sparkles::Result<sparkles::handles::MappingRun>;
    let old: OldRun = Dataset::run_mapping;
    let result = old(&ds, MAPPING, None, &inputs, &options, Some("urn:override")).unwrap();
    assert!(result.receipt.committed);
    assert_eq!((result.stats.rows, result.stats.quads), (2, 2));
    assert_eq!(ds.len(), 2);
    assert!(ds.ask("ASK { GRAPH <urn:override> { <http://ex.org/person/1> <http://ex.org/name> \"Ann\" . <http://ex.org/person/2> <http://ex.org/name> \"Bo\" } }").unwrap());
}

struct Published {
    gate: Arc<Gate>,
    seq: AtomicU64,
}
impl CommitGuard for Published {
    fn check(&self, _: &Candidate<'_>) -> sparkles::Result<ValidationSummary> {
        Ok(ValidationSummary::empty(
            GuardStatus::Passed,
            GuardMode::Off,
            Severity::Violation,
        ))
    }
    fn committed(&self, seq: u64) {
        self.seq.store(seq, Ordering::SeqCst);
        self.gate.pause();
    }
    fn describe(&self) -> String {
        "publication latch".into()
    }
}
#[test]
fn publication_wins_over_cancellation_or_expiry_after_last_commit() {
    let (_dir, inputs) = input();
    for seeded in [false, true] {
        for expired in [false, true] {
            let ds = dataset(seeded);
            let head = ds.head_commit().seq;
            let len = ds.len();
            let cancel = Cancel::new();
            let deadline = expired.then(|| Instant::now() + Duration::from_secs(3));
            let write = WriteOptions {
                cancel: Some(cancel.flag()),
                deadline,
                ..Default::default()
            };
            let gate = Arc::new(Gate::default());
            let published = Arc::new(Published {
                gate: gate.clone(),
                seq: AtomicU64::new(0),
            });
            ds.store().set_guard(Some(published.clone()));
            let worker_ds = ds.clone();
            let worker_inputs = inputs.clone();
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                sender
                    .send(worker_ds.run_mapping_with(
                        MAPPING,
                        None,
                        &worker_inputs,
                        &RunOptions::default(),
                        None,
                        &write,
                    ))
                    .ok();
            });
            let entered = gate.reached();
            let seq = published.seq.load(Ordering::SeqCst);
            let actually_published =
                seq == head + 1 && ds.store().snapshot().commit == seq && ds.len() == len + 2;
            if let Some(deadline) = deadline {
                wait_for_deadline(deadline);
            } else {
                cancel.cancel();
            }
            gate.release();
            let result = receiver.recv_timeout(Duration::from_secs(10));
            worker.join().unwrap();
            ds.store().set_guard(None);
            assert!(
                entered && actually_published,
                "actual publication was not latched"
            );
            let result = result
                .unwrap()
                .expect("published mapping must remain successful");
            assert!(result.receipt.committed);
            assert_eq!(result.receipt.commit.seq, seq);
            assert_eq!(ds.head_commit().seq, seq);
            assert_eq!(ds.len(), len + 2);
        }
    }
}

#[test]
fn controlled_output_keeps_named_graphs_and_default_only_override() {
    let (_dir, inputs) = input();
    for named in [false, true] {
        let text = if named {
            MAPPING.replace("    po:", "    graphs: ex:original\n    po:")
        } else {
            MAPPING.to_owned()
        };
        let ds = Dataset::memory();
        let result = ds
            .run_mapping_with(
                &text,
                None,
                &inputs,
                &RunOptions::default(),
                Some("urn:override"),
                &WriteOptions::default(),
            )
            .unwrap();
        let graph = if named {
            "http://ex.org/original"
        } else {
            "urn:override"
        };
        assert!(ds.ask(&format!("ASK {{ GRAPH <{graph}> {{ <http://ex.org/person/1> <http://ex.org/name> \"Ann\" . <http://ex.org/person/2> <http://ex.org/name> \"Bo\" }} }}")).unwrap());
        assert!(!ds.ask("ASK { ?s ?p ?o }").unwrap());
        assert_eq!((result.stats.rows, result.stats.quads, ds.len()), (2, 2, 2));
        assert!(result.receipt.committed);
    }
}

#[test]
fn conversion_refusals_and_entry_controls_leave_data_and_head_unchanged() {
    let (_dir, inputs) = input();
    let ds = dataset(true);
    let head = ds.head_commit().seq;
    for completion in [false, true] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let options = RunOptions {
            check: Some(Arc::new(move || {
                let count = seen.fetch_add(1, Ordering::SeqCst);
                if !completion || count == 1 {
                    Err(Error::invalid("independent conversion refusal"))
                } else {
                    Ok(())
                }
            })),
            ..Default::default()
        };
        let error = ds
            .run_mapping_with(
                MAPPING,
                None,
                &inputs,
                &options,
                None,
                &WriteOptions::default(),
            )
            .unwrap_err();
        assert!(
            matches!(error, Error::Invalid(ref text) if text == "independent conversion refusal")
        );
        assert_eq!(calls.load(Ordering::SeqCst), if completion { 2 } else { 1 });
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), 1);
    }
    assert!(matches!(
        ds.run_mapping_with(
            MAPPING,
            None,
            &inputs,
            &RunOptions {
                max_output_bytes: Some(1),
                ..Default::default()
            },
            None,
            &WriteOptions::default()
        ),
        Err(Error::BudgetExceeded(_))
    ));
    let cancel = Cancel::new();
    cancel.cancel();
    for write in [
        WriteOptions {
            cancel: Some(cancel.flag()),
            ..Default::default()
        },
        WriteOptions {
            deadline: Some(Instant::now()),
            ..Default::default()
        },
    ] {
        let timed = write.deadline.is_some();
        let error = ds
            .run_mapping_with(
                "unrecognized",
                None,
                &Inputs::default(),
                &RunOptions::default(),
                None,
                &write,
            )
            .unwrap_err();
        assert!(if timed {
            matches!(error, Error::Timeout)
        } else {
            matches!(error, Error::Cancelled)
        });
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), 1);
    }
    assert!(
        ds.ask("ASK { <urn:old> <urn:predicate> <urn:value> }")
            .unwrap()
    );
}

#[test]
fn conversion_preserves_structured_cancellation_and_independent_checkpoints() {
    use std::io::Write;
    let (dir, inputs) = input();
    let mut csv =
        std::io::BufWriter::new(std::fs::File::create(dir.path().join("people.csv")).unwrap());
    writeln!(csv, "id,name").unwrap();
    for id in 0..133_000 {
        writeln!(csv, "{id},person").unwrap();
    }
    csv.flush().unwrap();
    drop(csv);
    let ds = dataset(true);
    let head = ds.head_commit().seq;
    let cancel = Cancel::new();
    let gate = Arc::new(Gate::default());
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let callback = gate.clone();
    let options = RunOptions {
        check: Some(Arc::new(move || {
            // One entry check, then the actual 65,536-row reader checkpoint.
            if seen.fetch_add(1, Ordering::SeqCst) == 1 {
                callback.pause();
            }
            Ok(())
        })),
        max_output_bytes: Some(64 << 20),
        ..Default::default()
    };
    let write = WriteOptions {
        cancel: Some(cancel.flag()),
        ..Default::default()
    };
    let worker_ds = ds.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        sender
            .send(worker_ds.run_mapping_with(MAPPING, None, &inputs, &options, None, &write))
            .ok();
    });
    let entered = gate.reached();
    cancel.cancel();
    gate.release();
    let result = receiver.recv_timeout(Duration::from_secs(20));
    worker.join().unwrap();
    assert!(entered, "actual conversion checkpoint was not latched");
    assert!(matches!(result.unwrap(), Err(Error::Cancelled)));
    // The next structured check refuses before invoking the independent callback.
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(ds.head_commit().seq, head);
    assert_eq!(ds.len(), 1);
    assert!(
        ds.ask("ASK { <urn:old> <urn:predicate> <urn:value> }")
            .unwrap()
    );
}

#[test]
fn structured_write_authority_preconditions_preview_and_metadata_survive() {
    use sparkles::access::{GraphAccess, Graphs};
    use sparkles::guard::Precondition;
    for seeded in [false, true] {
        let (_dir, inputs) = input();
        let ds = dataset(seeded);
        let head = ds.head_commit().seq;
        let len = ds.len();
        let held = ds
            .store()
            .try_write_with(sparkles::commit::CommitKind::Load, WriteOptions::default())
            .unwrap();
        let result = ds.run_mapping_with(
            MAPPING,
            None,
            &inputs,
            &RunOptions::default(),
            None,
            &WriteOptions {
                no_wait: true,
                ..Default::default()
            },
        );
        drop(held);
        assert!(matches!(result, Err(Error::WriterBusy)));
        let seen = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observe = seen.clone();
        let write = WriteOptions {
            precondition: Some(Precondition::new(move |snapshot| {
                observe.store(snapshot.commit == head, Ordering::SeqCst);
                Err(Error::PreconditionFailed(
                    "native mapping precondition".into(),
                ))
            })),
            ..Default::default()
        };
        assert!(
            matches!(ds.run_mapping_with(MAPPING, None, &inputs, &RunOptions::default(), None, &write), Err(Error::PreconditionFailed(ref text)) if text == "native mapping precondition")
        );
        assert!(seen.load(Ordering::SeqCst));
        let denied = WriteOptions {
            graphs: Some(Arc::new(GraphAccess::graphs(Graphs::All, Graphs::none()))),
            ..Default::default()
        };
        assert!(matches!(
            ds.run_mapping_with(
                MAPPING,
                None,
                &inputs,
                &RunOptions::default(),
                None,
                &denied
            ),
            Err(Error::NotPermitted(_))
        ));
        let preview = WriteOptions {
            dry_run: Some(Default::default()),
            ..Default::default()
        };
        assert!(matches!(
            ds.run_mapping_with(
                MAPPING,
                None,
                &inputs,
                &RunOptions::default(),
                None,
                &preview
            ),
            Err(Error::DryRun(_))
        ));
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), len);
        let message = WriteOptions {
            message: Some("native mapping message".into()),
            ..Default::default()
        };
        let result = ds
            .run_mapping_with(
                MAPPING,
                None,
                &inputs,
                &RunOptions::default(),
                None,
                &message,
            )
            .unwrap();
        assert!(result.receipt.committed);
        assert_eq!(
            result.receipt.annotation.message.as_deref(),
            Some("native mapping message")
        );
        assert_eq!(ds.head_commit().seq, head + 1);
        assert_eq!(ds.len(), len + 2);
    }
}
