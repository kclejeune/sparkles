//! Snapshot copies for entailment diffs, previews and justification oracles.

use oxrdf::{Literal, NamedNode, NamedOrBlankNode, Term, Triple};
use sparkles_core::Error;
use sparkles_core::commit::CommitKind;
use sparkles_core::guard::WriteOptions;
use sparkles_core::id::{Id, Tag, bnode_label};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::preview::{self, DryRun};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn state(snapshot: &Snapshot) -> BTreeSet<String> {
    let mut rows = BTreeSet::new();
    snapshot
        .for_each_quad(|quad| {
            rows.insert(snapshot.quad_to_terms(quad).unwrap().to_string());
            Ok(())
        })
        .unwrap();
    rows
}

fn load(store: &Store, text: &str) {
    store
        .load(&[Source::from_bytes(
            text.as_bytes().to_vec(),
            RdfFormat::TriG,
            None,
        )])
        .unwrap();
}

#[test]
fn stored_blank_identity_and_graphs_survive_a_filtered_copy() {
    let source = Store::in_memory(Default::default());
    load(
        &source,
        "@prefix ex: <urn:ex:> . ex:s ex:p _:x . _:x ex:q 1 . ex:g { ex:s ex:r <<( _:x ex:p 1 )>> } _:graph { _:x ex:q _:graph }",
    );
    let snapshot = source.snapshot();
    let head = source.head_commit();
    let copy = source
        .copy_view_to_memory(&snapshot, Default::default(), &Default::default(), |_| {
            Ok(true)
        })
        .unwrap();
    assert_eq!(state(&snapshot), state(&copy.snapshot()));
    let mut transaction = copy.write();
    let new = Term::BlankNode(
        oxrdf::BlankNode::new(bnode_label(transaction.new_bnode().payload())).unwrap(),
    );
    assert!(
        !state(&snapshot)
            .iter()
            .any(|row| row.contains(&new.to_string()))
    );
    drop(transaction);
    let graph = snapshot.lookup_iri("urn:ex:g").unwrap();
    let filtered = source
        .copy_view_to_memory(&snapshot, Default::default(), &Default::default(), |quad| {
            Ok(quad[3] == graph)
        })
        .unwrap();
    assert_eq!(filtered.snapshot().len(), 1);
    assert_eq!(source.head_commit().seq, head.seq);
    assert_eq!(state(&source.snapshot()), state(&snapshot));
}

#[test]
fn unpublished_nested_blank_ids_are_reserved_before_new_allocations() {
    for cdt in [false, true] {
        let source = Store::in_memory(Default::default());
        let mut transaction = source.write();
        let node = transaction.new_bnode();
        let blank = oxrdf::BlankNode::new(bnode_label(node.payload())).unwrap();
        let object = if cdt {
            Term::Literal(Literal::new_typed_literal(
                format!("[_:{}]", blank.as_str()),
                NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
            ))
        } else {
            Term::Triple(Box::new(Triple::new(
                NamedOrBlankNode::BlankNode(blank),
                NamedNode::new("urn:inner").unwrap(),
                Literal::from(1),
            )))
        };
        let subject = transaction
            .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
            .unwrap();
        let predicate = transaction
            .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let object = transaction.intern(&object).unwrap();
        transaction
            .insert([subject, predicate, object, Id::DEFAULT_GRAPH])
            .unwrap();
        let view = transaction.read_view().unwrap();
        drop(transaction);
        assert_eq!(source.snapshot().len(), 0);
        let copied = source
            .copy_view_to_memory(&view, Default::default(), &Default::default(), |_| Ok(true))
            .unwrap();
        assert_eq!(state(&view), state(&copied.snapshot()));
        let mut write = copied.write();
        assert_ne!(
            write.new_bnode(),
            node,
            "only nested references still reserve unpublished ids"
        );
    }
}

#[test]
fn branch_and_inherited_blank_ranges_remain_disjoint_after_copy() {
    let source = Store::in_memory(Default::default());
    load(&source, "_:base <urn:p> 1 .");
    source.create_branch("work", &Default::default()).unwrap();
    let branch = source.branch("work").unwrap();
    load(&branch, "_:own <urn:p> 2 .");
    source.create_branch("later", &Default::default()).unwrap();
    let later = source.branch("later").unwrap();
    load(&later, "_:later <urn:p> 4 .");
    source.merge("later", "work", &Default::default()).unwrap();
    let mut write = branch.write();
    let pending = write.new_bnode();
    let p = write
        .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
        .unwrap();
    write
        .insert([pending, p, Id::from_i64(3).unwrap(), Id::DEFAULT_GRAPH])
        .unwrap();
    let view = write.read_view().unwrap();
    drop(write);
    let copied = branch
        .copy_view_to_memory(&view, Default::default(), &Default::default(), |_| Ok(true))
        .unwrap();
    assert_eq!(state(&view), state(&copied.snapshot()));
    let mut write = copied.write();
    let fresh = write.new_bnode();
    assert_ne!(fresh, pending);
    drop(write);
    let future = copied.create_branch("future", &Default::default()).unwrap();
    assert!(
        future.ordinal > 2,
        "copied branch ranges cannot be handed out again"
    );
    let future_store = copied.branch("future").unwrap();
    let mut write = future_store.write();
    let future_node = write.new_bnode();
    assert_ne!(future_node, pending);
    assert_ne!(future_node, fresh);
    view.for_each_quad(|quad| {
        assert_ne!(quad[0], fresh);
        Ok(())
    })
    .unwrap();
    assert_eq!(branch.snapshot().len(), 3);
    assert_eq!(source.snapshot().len(), 1);
}

#[test]
fn copies_stop_on_cancellation_and_refuse_an_insufficient_memory_grant() {
    let source = Store::in_memory(Default::default());
    load(&source, "<urn:a> <urn:p> 1,2,3 .");
    let before = state(&source.snapshot());
    let cancelled = Arc::new(AtomicBool::new(false));
    let control = WriteOptions {
        cancel: Some(cancelled.clone()),
        ..Default::default()
    };
    let result =
        source.copy_view_to_memory(&source.snapshot(), Default::default(), &control, |_| {
            cancelled.store(true, Ordering::Relaxed);
            Ok(true)
        });
    assert!(matches!(result, Err(Error::Cancelled)));
    let result = source.copy_view_to_memory(
        &source.snapshot(),
        StoreOptions {
            max_memory_bytes: Some(1),
            ..Default::default()
        },
        &Default::default(),
        |_| Ok(true),
    );
    assert!(matches!(result, Err(Error::StorageFull(_))));
    assert_eq!(state(&source.snapshot()), before);
}

#[test]
fn retained_dry_run_nested_blank_ids_are_reserved_before_new_allocations() {
    for case in ["triple", "cdt-list", "cdt-map", "cdt-quoted-plus-unicode"] {
        let source = Store::in_memory(Default::default());
        let options = WriteOptions {
            dry_run: Some(DryRun {
                retain_states: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut transaction = source.write_with(CommitKind::Load, options);
        let node = transaction.new_bnode();
        let blank = oxrdf::BlankNode::new(bnode_label(node.payload())).unwrap();
        let object = match case {
            "triple" => Term::Triple(Box::new(Triple::new(
                NamedOrBlankNode::BlankNode(blank),
                NamedNode::new("urn:inner").unwrap(),
                Literal::from(1),
            ))),
            "cdt-list" => Term::Literal(Literal::new_typed_literal(
                format!("[_:{}]", blank.as_str()),
                NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
            )),
            "cdt-map" => Term::Literal(Literal::new_typed_literal(
                format!("{{ \"k\" : _:{} }}", blank.as_str()),
                NamedNode::new(sparkles_core::sparql::cdt::MAP).unwrap(),
            )),
            "cdt-quoted-plus-unicode" => {
                let suffix = blank
                    .as_str()
                    .strip_prefix('b')
                    .expect("stored blank node label starts with 'b'");
                Term::Literal(Literal::new_typed_literal(
                    format!(
                        "[\"[_:\\u+062{suffix}]\"^^<{}>]",
                        sparkles_core::sparql::cdt::LIST
                    ),
                    NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
                ))
            }
            _ => unreachable!(),
        };
        let subject = transaction
            .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
            .unwrap();
        let predicate = transaction
            .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let object = transaction.intern(&object).unwrap();
        transaction
            .insert([subject, predicate, object, Id::DEFAULT_GRAPH])
            .unwrap();
        let preview = preview::catch(transaction.commit()).unwrap();
        let retained = preview.states.unwrap().after;
        assert_eq!(source.snapshot().len(), 0);
        let copied = source
            .copy_view_to_memory(&retained, Default::default(), &Default::default(), |_| {
                Ok(true)
            })
            .unwrap();
        assert_eq!(state(&retained), state(&copied.snapshot()));
        let mut write = copied.write();
        assert_ne!(
            write.new_bnode(),
            node,
            "case {case}: blank referenced only inside retained dry-run nested term must be reserved"
        );
    }
}

#[test]
fn compacted_base_vocab_copy_accounts_for_live_scratch_and_entry_overhead() {
    let payload = "x".repeat(64 * 1024);
    let source = Store::in_memory(Default::default());
    load(&source, &format!("<urn:s> <urn:p> \"{payload}\" ."));
    source.compact().unwrap();
    let snapshot = source.snapshot();
    let mut saw_vocab = false;
    snapshot
        .for_each_quad(|quad| {
            if quad[2].tag() == Tag::Vocab {
                saw_vocab = true;
            }
            Ok(())
        })
        .unwrap();
    assert!(saw_vocab, "expected compacted literal in base vocabulary");

    // Single-copy cap (96 KiB) admits either scratch (~64 KiB) or destination (~64 KiB)
    // alone, but must refuse before allocating both simultaneously (~128 KiB).
    let single_copy_cap = StoreOptions {
        max_memory_bytes: Some(96 * 1024),
        ..Default::default()
    };
    let refused =
        source.copy_view_to_memory(&snapshot, single_copy_cap, &Default::default(), |_| {
            Ok(true)
        });
    assert!(
        matches!(refused, Err(Error::StorageFull(_))),
        "expected StorageFull when live scratch and destination copy overlap, got: {:?}",
        refused.err()
    );

    // Boundary excluding AppendVocab's 48-byte entry overhead:
    // <urn:s> (6 + 48 = 54) + <urn:p> (6 + 48 = 54) = 108 base bytes;
    // payload key = payload.len() + 2 (leading '"' + trailing KEY_SEP).
    let key_len = payload.len() as u64 + 2;
    let without_entry_overhead = StoreOptions {
        max_memory_bytes: Some(108 + key_len * 2),
        ..Default::default()
    };
    let refused_overhead = source.copy_view_to_memory(
        &snapshot,
        without_entry_overhead,
        &Default::default(),
        |_| Ok(true),
    );
    assert!(
        matches!(refused_overhead, Err(Error::StorageFull(_))),
        "expected StorageFull when AppendVocab 48-byte entry overhead exceeds cap, got: {:?}",
        refused_overhead.err()
    );

    // Sufficient cap admits live scratch + destination copy + metadata.
    let enough_cap = StoreOptions {
        max_memory_bytes: Some(160 * 1024),
        ..Default::default()
    };
    let copied = source
        .copy_view_to_memory(&snapshot, enough_cap, &Default::default(), |_| Ok(true))
        .unwrap();
    assert_eq!(state(&snapshot), state(&copied.snapshot()));

    // Compacted CDT literal containing a blank node must scan without allocating
    // a decoded Term or parsed CDT AST while live vocab scratch is held.
    // Leave the source allocator at 0 so the no-reuse assertion directly verifies
    // CDT reservation rather than inherited source counter advancement.
    let cdt_source = Store::in_memory(Default::default());
    let mut tx = cdt_source.write();
    let node = Id::bnode(0);
    let cdt_lex = format!("[\"{payload}\", _:{}]", bnode_label(node.payload()));
    let cdt_object = Term::Literal(Literal::new_typed_literal(
        cdt_lex.clone(),
        NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
    ));
    let subject = tx
        .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
        .unwrap();
    let predicate = tx
        .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
        .unwrap();
    let object = tx.intern(&cdt_object).unwrap();
    tx.insert([subject, predicate, object, Id::DEFAULT_GRAPH])
        .unwrap();
    tx.commit().unwrap();
    cdt_source.compact().unwrap();
    let cdt_snap = cdt_source.snapshot();
    let mut saw_cdt_vocab = false;
    cdt_snap
        .for_each_quad(|quad| {
            if quad[2].tag() == Tag::Vocab {
                saw_cdt_vocab = true;
            }
            Ok(())
        })
        .unwrap();
    assert!(
        saw_cdt_vocab,
        "expected compacted CDT literal in base vocabulary"
    );

    let cdt_refused = cdt_source.copy_view_to_memory(
        &cdt_snap,
        StoreOptions {
            max_memory_bytes: Some(96 * 1024),
            ..Default::default()
        },
        &Default::default(),
        |_| Ok(true),
    );
    assert!(
        matches!(cdt_refused, Err(Error::StorageFull(_))),
        "expected StorageFull for compacted CDT literal under single-copy cap, got: {:?}",
        cdt_refused.err()
    );

    // Exact boundary excluding and including AppendVocab's 48-byte entry overhead:
    // '"' + lex + KEY_SEP + '^' + LIST.len().
    // Leaving zero headroom for key_to_term or CDT AST parsing proves blank-node
    // reservation runs in O(1) heap while scratch and destination overlap.
    let cdt_key_len = cdt_lex.len() as u64 + 3 + sparkles_core::sparql::cdt::LIST.len() as u64;
    let cdt_without_entry_overhead = cdt_source.copy_view_to_memory(
        &cdt_snap,
        StoreOptions {
            max_memory_bytes: Some(108 + cdt_key_len * 2),
            ..Default::default()
        },
        &Default::default(),
        |_| Ok(true),
    );
    assert!(
        matches!(cdt_without_entry_overhead, Err(Error::StorageFull(_))),
        "expected StorageFull when compacted CDT AppendVocab entry overhead exceeds cap, got: {:?}",
        cdt_without_entry_overhead.err()
    );

    let cdt_tight_cap = StoreOptions {
        max_memory_bytes: Some(108 + cdt_key_len * 2 + 48),
        ..Default::default()
    };
    let cdt_copied = cdt_source
        .copy_view_to_memory(&cdt_snap, cdt_tight_cap, &Default::default(), |_| Ok(true))
        .unwrap();
    assert_eq!(state(&cdt_snap), state(&cdt_copied.snapshot()));
    let mut cdt_write = cdt_copied.write();
    assert_ne!(
        cdt_write.new_bnode(),
        node,
        "compacted CDT blank node must be reserved under tight memory cap"
    );
    drop(cdt_write);
    let mut cdt_src_check = cdt_source.write();
    assert_eq!(
        cdt_src_check.new_bnode(),
        node,
        "source allocator counter must still be at the unallocated target label"
    );
    drop(cdt_src_check);

    // Same tight-cap check for an escaped quoted nested CDT literal holding a 64 KiB string.
    let quoted_source = Store::in_memory(Default::default());
    let mut qtx = quoted_source.write();
    let qnode = Id::bnode(0);
    let quoted_lex = format!(
        "[\"[\\\"{payload}\\\", _:{}]\"^^<{}>]",
        bnode_label(qnode.payload()),
        sparkles_core::sparql::cdt::LIST
    );
    let quoted_object = Term::Literal(Literal::new_typed_literal(
        quoted_lex.clone(),
        NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
    ));
    let qs = qtx
        .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
        .unwrap();
    let qp = qtx
        .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
        .unwrap();
    let qo = qtx.intern(&quoted_object).unwrap();
    qtx.insert([qs, qp, qo, Id::DEFAULT_GRAPH]).unwrap();
    qtx.commit().unwrap();
    quoted_source.compact().unwrap();
    let quoted_snap = quoted_source.snapshot();
    let quoted_key_len =
        quoted_lex.len() as u64 + 3 + sparkles_core::sparql::cdt::LIST.len() as u64;
    let quoted_refused = quoted_source.copy_view_to_memory(
        &quoted_snap,
        StoreOptions {
            max_memory_bytes: Some(108 + quoted_key_len * 2),
            ..Default::default()
        },
        &Default::default(),
        |_| Ok(true),
    );
    assert!(
        matches!(quoted_refused, Err(Error::StorageFull(_))),
        "expected StorageFull when quoted CDT AppendVocab entry overhead exceeds cap, got: {:?}",
        quoted_refused.err()
    );
    let quoted_copied = quoted_source
        .copy_view_to_memory(
            &quoted_snap,
            StoreOptions {
                max_memory_bytes: Some(108 + quoted_key_len * 2 + 48),
                ..Default::default()
            },
            &Default::default(),
            |_| Ok(true),
        )
        .unwrap();
    assert_eq!(state(&quoted_snap), state(&quoted_copied.snapshot()));
    let mut qwrite = quoted_copied.write();
    assert_ne!(
        qwrite.new_bnode(),
        qnode,
        "blank node inside escaped quoted nested CDT must be reserved under tight cap"
    );
    drop(qwrite);
    let mut qsrc_check = quoted_source.write();
    assert_eq!(
        qsrc_check.new_bnode(),
        qnode,
        "source allocator counter must still be at the unallocated quoted target label"
    );
}

#[test]
fn ill_typed_cdt_literals_copy_without_exhausting_or_advancing_blank_allocator() {
    let cases = [
        (
            "trailing-comma-max-bnode",
            "[_:b7ffffffffff,]",
            sparkles_core::sparql::cdt::LIST,
        ),
        (
            "trailing-comma-b0",
            "[_:b0,]",
            sparkles_core::sparql::cdt::LIST,
        ),
        (
            "duplicate-map-key",
            r#"{"a": _:b7ffffffffff, "a": 1}"#,
            sparkles_core::sparql::cdt::MAP,
        ),
        (
            "canonical-duplicate-map-key",
            r#"{1: _:b7ffffffffff, "1"^^<http://www.w3.org/2001/XMLSchema#integer>: 2}"#,
            sparkles_core::sparql::cdt::MAP,
        ),
        (
            "bnode-map-key",
            "{_:b7ffffffffff : 1}",
            sparkles_core::sparql::cdt::MAP,
        ),
        (
            "map-syntax-in-list-datatype",
            r#"{"k": _:b7ffffffffff}"#,
            sparkles_core::sparql::cdt::LIST,
        ),
    ];
    for (name, lex, dt) in cases {
        let source = Store::in_memory(Default::default());
        let mut tx = source.write();
        let subject = tx
            .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
            .unwrap();
        let predicate = tx
            .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
            .unwrap();
        let object = tx
            .intern(&Term::Literal(Literal::new_typed_literal(
                lex,
                NamedNode::new(dt).unwrap(),
            )))
            .unwrap();
        tx.insert([subject, predicate, object, Id::DEFAULT_GRAPH])
            .unwrap();
        tx.commit().unwrap();
        let snap = source.snapshot();
        let copied = source
            .copy_view_to_memory(&snap, Default::default(), &Default::default(), |_| Ok(true))
            .unwrap_or_else(|e| panic!("case {name}: ill-typed CDT copy must succeed, got: {e}"));
        assert_eq!(state(&snap), state(&copied.snapshot()), "case {name}");
        let mut write = copied.write();
        assert_eq!(
            write.new_bnode(),
            Id::bnode(0),
            "case {name}: ill-typed CDT literal must not advance destination allocator"
        );
    }

    // Valid outer list containing both a real blank node and an ill-typed quoted nested CDT
    // must copy cleanly, reserve the valid blank node, and ignore the ill-typed inner literal.
    let source = Store::in_memory(Default::default());
    let mut tx = source.write();
    let subject = tx
        .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
        .unwrap();
    let predicate = tx
        .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
        .unwrap();
    let mixed = Term::Literal(Literal::new_typed_literal(
        format!(
            "[_:b0, \"[_:b7ffffffffff,]\"^^<{}>]",
            sparkles_core::sparql::cdt::LIST
        ),
        NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
    ));
    let object = tx.intern(&mixed).unwrap();
    tx.insert([subject, predicate, object, Id::DEFAULT_GRAPH])
        .unwrap();
    tx.commit().unwrap();
    let snap = source.snapshot();
    let copied = source
        .copy_view_to_memory(&snap, Default::default(), &Default::default(), |_| Ok(true))
        .unwrap();
    assert_eq!(state(&snap), state(&copied.snapshot()));
    let mut write = copied.write();
    assert_ne!(
        write.new_bnode(),
        Id::bnode(0),
        "valid blank node beside ill-typed quoted nested CDT must still be reserved"
    );
}

#[test]
fn deeply_nested_cdt_blank_ids_are_reserved_without_stack_overflow() {
    let handle = std::thread::Builder::new()
        .name("deep-cdt-sim-copy".into())
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for case in ["deep-list", "deep-map", "deep-triple-cdt"] {
                let source = Store::in_memory(Default::default());
                let options = WriteOptions {
                    dry_run: Some(DryRun {
                        retain_states: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let mut tx = source.write_with(CommitKind::Load, options);
                let node = tx.new_bnode();
                let label = bnode_label(node.payload());
                let object = match case {
                    "deep-list" => {
                        let depth = 20_000;
                        let mut lex = String::with_capacity(depth * 2 + label.len() + 2);
                        for _ in 0..depth {
                            lex.push('[');
                        }
                        lex.push_str("_:");
                        lex.push_str(&label);
                        for _ in 0..depth {
                            lex.push(']');
                        }
                        Term::Literal(Literal::new_typed_literal(
                            lex,
                            NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
                        ))
                    }
                    "deep-map" => {
                        let depth = 10_000;
                        let mut lex = String::with_capacity(depth * 6 + label.len() + 2);
                        for _ in 0..depth {
                            lex.push_str("{\"k\":");
                        }
                        lex.push_str("_:");
                        lex.push_str(&label);
                        for _ in 0..depth {
                            lex.push('}');
                        }
                        Term::Literal(Literal::new_typed_literal(
                            lex,
                            NamedNode::new(sparkles_core::sparql::cdt::MAP).unwrap(),
                        ))
                    }
                    "deep-triple-cdt" => {
                        let depth = 20_000;
                        let mut lex = String::with_capacity(depth * 2 + label.len() + 2);
                        for _ in 0..depth {
                            lex.push('[');
                        }
                        lex.push_str("_:");
                        lex.push_str(&label);
                        for _ in 0..depth {
                            lex.push(']');
                        }
                        Term::Triple(Box::new(Triple::new(
                            NamedOrBlankNode::NamedNode(NamedNode::new("urn:inner-s").unwrap()),
                            NamedNode::new("urn:inner-p").unwrap(),
                            Term::Literal(Literal::new_typed_literal(
                                lex,
                                NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
                            )),
                        )))
                    }
                    _ => unreachable!(),
                };
                let subject = tx
                    .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
                    .unwrap();
                let predicate = tx
                    .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
                    .unwrap();
                let object = tx.intern(&object).unwrap();
                tx.insert([subject, predicate, object, Id::DEFAULT_GRAPH])
                    .unwrap();
                let preview = preview::catch(tx.commit()).unwrap();
                let retained = preview.states.unwrap().after;

                // Both uncapped and capped copies must succeed without overflowing the stack.
                let copied = source
                    .copy_view_to_memory(&retained, Default::default(), &Default::default(), |_| {
                        Ok(true)
                    })
                    .unwrap();
                assert_eq!(state(&retained), state(&copied.snapshot()));
                let mut write = copied.write();
                assert_ne!(
                    write.new_bnode(),
                    node,
                    "case {case}: deeply nested CDT blank node must be reserved"
                );
                drop(write);

                let capped = source
                    .copy_view_to_memory(
                        &retained,
                        StoreOptions {
                            max_memory_bytes: Some(512 * 1024),
                            ..Default::default()
                        },
                        &Default::default(),
                        |_| Ok(true),
                    )
                    .unwrap();
                assert_eq!(state(&retained), state(&capped.snapshot()));
                let mut capped_write = capped.write();
                assert_ne!(
                    capped_write.new_bnode(),
                    node,
                    "case {case}: deeply nested CDT blank node must be reserved under memory cap"
                );
            }

            // Plain strings and IRIs inside CDT literals that look like blank-node labels
            // must not be mistaken for blank nodes during reservation.
            let source = Store::in_memory(Default::default());
            let options = WriteOptions {
                dry_run: Some(DryRun {
                    retain_states: true,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut tx = source.write_with(CommitKind::Load, options);
            let node = tx.new_bnode();
            let label = bnode_label(node.payload());
            let decoy = Term::Literal(Literal::new_typed_literal(
                format!("[\"[_:{label}]\", <urn:_:{label}>, \"[_:{label}]\"^^<urn:not-cdt>]"),
                NamedNode::new(sparkles_core::sparql::cdt::LIST).unwrap(),
            ));
            let subject = tx
                .intern(&Term::NamedNode(NamedNode::new("urn:s").unwrap()))
                .unwrap();
            let predicate = tx
                .intern(&Term::NamedNode(NamedNode::new("urn:p").unwrap()))
                .unwrap();
            let object = tx.intern(&decoy).unwrap();
            tx.insert([subject, predicate, object, Id::DEFAULT_GRAPH])
                .unwrap();
            let preview = preview::catch(tx.commit()).unwrap();
            let retained = preview.states.unwrap().after;
            let copied = source
                .copy_view_to_memory(&retained, Default::default(), &Default::default(), |_| {
                    Ok(true)
                })
                .unwrap();
            let mut write = copied.write();
            assert_eq!(
                write.new_bnode(),
                node,
                "blank-node-like text inside plain strings or IRIs must not be reserved"
            );
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn an_unrelated_dataset_view_is_refused() {
    let first = Store::in_memory(Default::default());
    let second = Store::in_memory(Default::default());
    assert!(
        first
            .copy_view_to_memory(
                &second.snapshot(),
                Default::default(),
                &Default::default(),
                |_| Ok(true)
            )
            .is_err()
    );
}
