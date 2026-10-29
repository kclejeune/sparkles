//! Exact private binding adapters, without a language runtime or new engine hooks.
#[path = "../../sparkles-ffi/src/mapping_control.rs"]
mod ffi_mapping;
#[path = "../../sparkles-node/src/admin/mapping_control.rs"]
mod node_mapping;

use sparkles::guard::{Precondition, WriteOptions};
use sparkles::mapping::{Inputs, MappingKind, RunOptions};
use sparkles::task::Control;
use sparkles::{Dataset, Error};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

type Runner = fn(
    &Dataset,
    &str,
    Option<MappingKind>,
    &Inputs,
    &RunOptions,
    Option<&str>,
    &WriteOptions,
) -> sparkles::Result<sparkles::handles::MappingRun>;
const RUNNERS: [(&str, Runner); 2] = [("ffi", ffi_mapping::run), ("node", node_mapping::run)];
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

fn options(control: &Control) -> (RunOptions, WriteOptions) {
    let check = control.clone();
    (
        RunOptions {
            check: Some(Arc::new(move || check.check())),
            ..Default::default()
        },
        WriteOptions {
            cancel: Some(control.cancel.flag()),
            deadline: control.deadline,
            ..Default::default()
        },
    )
}

#[test]
fn private_adapters_preserve_fixed_output_and_default_only_graph_override() {
    let (_dir, inputs) = input();
    for (name, run) in RUNNERS {
        for named in [false, true] {
            let text = if named {
                MAPPING.replace("    po:", "    graphs: ex:original\n    po:")
            } else {
                MAPPING.to_owned()
            };
            let ds = Dataset::memory();
            let original = Dataset::memory();
            let result = run(
                &ds,
                &text,
                None,
                &inputs,
                &RunOptions::default(),
                Some("urn:override"),
                &WriteOptions::default(),
            )
            .unwrap();
            let legacy = original
                .run_mapping(
                    &text,
                    None,
                    &inputs,
                    &RunOptions::default(),
                    Some("urn:override"),
                )
                .unwrap();
            assert_eq!((result.stats.rows, result.stats.quads), (2, 2), "{name}");
            assert_eq!((legacy.stats.rows, legacy.stats.quads), (2, 2));
            assert!(result.receipt.committed);
            assert_eq!(ds.len(), 2);
            let graph = if named {
                "http://ex.org/original"
            } else {
                "urn:override"
            };
            let query = format!(
                "ASK {{ GRAPH <{graph}> {{ <http://ex.org/person/1> <http://ex.org/name> \"Ann\" . <http://ex.org/person/2> <http://ex.org/name> \"Bo\" }} }}"
            );
            assert!(ds.ask(&query).unwrap(), "{name}, named={named}");
            assert!(original.ask(&query).unwrap());
            assert!(!ds.ask("ASK { ?s ?p ?o }").unwrap());
        }
    }
}

#[test]
fn private_adapters_refuse_invalid_input_and_pre_cancel_without_a_commit() {
    let (_dir, inputs) = input();
    for (name, run) in RUNNERS {
        let ds = Dataset::memory();
        let head = ds.head_commit().seq;
        for (text, kind, graph) in [
            ("unrecognized", None, None),
            (MAPPING, Some(MappingKind::Csvw), None),
            (MAPPING, Some(MappingKind::Template), None),
            ("mappings: [unfinished", Some(MappingKind::Yarrrml), None),
            (MAPPING, None, Some("relative graph")),
        ] {
            assert!(
                run(
                    &ds,
                    text,
                    kind,
                    &inputs,
                    &RunOptions::default(),
                    graph,
                    &WriteOptions::default()
                )
                .is_err(),
                "{name}"
            );
            assert_eq!(ds.head_commit().seq, head);
            assert_eq!(ds.len(), 0);
        }
        assert!(
            run(
                &ds,
                MAPPING,
                None,
                &Inputs::default(),
                &RunOptions::default(),
                None,
                &WriteOptions::default()
            )
            .is_err(),
            "{name}: missing input must refuse"
        );
        assert!(
            matches!(
                run(
                    &ds,
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
            ),
            "{name}: conversion limit must survive"
        );
        let control = Control::none();
        control.cancel.cancel();
        let (options, write) = options(&control);
        assert!(
            matches!(
                run(&ds, MAPPING, None, &inputs, &options, None, &write),
                Err(Error::Cancelled)
            ),
            "{name}"
        );
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), 0);
    }
}

#[test]
fn private_adapters_keep_cancellation_and_deadline_during_final_writer_wait() {
    let (_dir, inputs) = input();
    for (name, run) in RUNNERS {
        for deadline in [false, true] {
            let ds = Dataset::memory();
            let head = ds.head_commit().seq;
            let held = ds
                .store()
                .try_write_with(sparkles::commit::CommitKind::Load, WriteOptions::default())
                .unwrap();
            let control = Control {
                deadline: deadline.then(|| Instant::now() + Duration::from_secs(2)),
                ..Control::none()
            };
            let (options, write) = options(&control);
            let worker_ds = ds.clone();
            let worker_inputs = inputs.clone();
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = run(
                    &worker_ds,
                    MAPPING,
                    None,
                    &worker_inputs,
                    &options,
                    None,
                    &write,
                );
                sender.send(result).ok();
            });
            let observe_until = Instant::now() + Duration::from_secs(1);
            while ds.store().writers_waiting() == 0 && Instant::now() < observe_until {
                std::thread::sleep(Duration::from_millis(1));
            }
            let observed_writer_wait = ds.store().writers_waiting() > 0;
            if !deadline {
                control.cancel.cancel();
            }
            // Always release the held writer and join before asserting, even on failure.
            let while_held = receiver.recv_timeout(Duration::from_secs(3));
            let unchanged_head = ds.store().snapshot().commit == head;
            let unchanged_data = ds.is_empty();
            drop(held);
            let result = match while_held {
                Ok(result) => Some(result),
                Err(_) => {
                    let _ = receiver.recv_timeout(Duration::from_secs(3));
                    None
                }
            };
            worker.join().unwrap();
            assert!(
                observed_writer_wait,
                "{name}, deadline={deadline}: final writer phase was not observed"
            );
            assert!(
                unchanged_head && unchanged_data,
                "{name}: pending writer mutated state"
            );
            let error = result
                .expect("mapping did not stop while the writer remained held")
                .unwrap_err();
            assert!(
                if deadline {
                    matches!(error, Error::Timeout)
                } else {
                    matches!(error, Error::Cancelled)
                },
                "{name}, deadline={deadline}: {error}"
            );
            assert_eq!(ds.head_commit().seq, head);
            assert_eq!(ds.len(), 0);
        }
    }
}

#[test]
fn private_adapters_keep_existing_no_wait_and_precondition_refusals() {
    let (_dir, inputs) = input();
    for (name, run) in RUNNERS {
        let ds = Dataset::memory();
        let head = ds.head_commit().seq;
        let held = ds
            .store()
            .try_write_with(sparkles::commit::CommitKind::Load, WriteOptions::default())
            .unwrap();
        let write = WriteOptions {
            no_wait: true,
            ..Default::default()
        };
        let result = run(
            &ds,
            MAPPING,
            None,
            &inputs,
            &RunOptions::default(),
            None,
            &write,
        );
        drop(held);
        assert!(matches!(result, Err(Error::WriterBusy)), "{name}");
        let write = WriteOptions {
            precondition: Some(Precondition::new(|_| {
                Err(Error::PreconditionFailed("fixed control".into()))
            })),
            ..Default::default()
        };
        assert!(
            matches!(
                run(
                    &ds,
                    MAPPING,
                    None,
                    &inputs,
                    &RunOptions::default(),
                    None,
                    &write
                ),
                Err(Error::PreconditionFailed(_))
            ),
            "{name}"
        );
        assert_eq!(ds.head_commit().seq, head);
        assert_eq!(ds.len(), 0);
    }
}
