//! A SERVICE inside an EXISTS is called once per distinct substitution, not once per row,
//! also when a streamed query's memory budget is nearly spent.

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, ExecutionMode, QueryExecution, QueryOptions, query, query_execution,
};
use sparkles_core::store::{Store, StoreOptions};
use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const ROWS: usize = 60_000;

fn store() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..ROWS {
        nt.push_str(&format!("<urn:s:{i}> <urn:p> {} .\n", i % 10));
    }
    s.load(&[Source::from_bytes(nt.into_bytes(), RdfFormat::Turtle, None)])
        .unwrap();
    s
}

/// An endpoint that counts its calls and answers each with `response`.
fn endpoint(response: &'static [u8]) -> (String, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/sparql", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            counted.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(response);
        }
    });
    (url, calls)
}

const FAILING: &[u8] =
    b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
const EMPTY: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-type: application/sparql-results+json\r\ncontent-length: 49\r\nconnection: close\r\n\r\n{\"head\":{\"vars\":[\"x\"]},\"results\":{\"bindings\":[]}}";

fn options() -> QueryOptions {
    QueryOptions {
        allow_service: true,
        outbound: sparkles_core::outbound::OutboundPolicy {
            allow_private: true,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn rows_with(s: &Store, q: &str, mode: ExecutionMode, o: &QueryOptions) -> usize {
    match query_execution(s.snapshot(), q, o, &CursorOptions::default(), mode).unwrap() {
        QueryExecution::Select(mut c) => {
            let mut n = 0;
            while let Some(b) = c.next_batch().unwrap() {
                n += b.len();
            }
            n
        }
        QueryExecution::Eager(r) => r.into_result().rows().len(),
        _ => unreachable!("a SELECT"),
    }
}

#[test]
fn a_service_in_an_exists_without_shared_variables_is_called_once() {
    let s = store();
    // A budget that a streamed scan of the rows nearly fills, so that a cursor keeps no
    // answer in the EXISTS memo. It used to call the service once per row.
    let tight = Some(1152u64 << 10);
    for (response, found) in [(FAILING, true), (EMPTY, false)] {
        let (url, calls) = endpoint(response);
        for negated in [false, true] {
            for mode in [
                ExecutionMode::Eager,
                ExecutionMode::Streaming,
                ExecutionMode::Auto,
            ] {
                for memory in [None, tight] {
                    let not = if negated { "NOT " } else { "" };
                    let q = format!(
                        "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o FILTER {not}EXISTS {{ SERVICE SILENT <{url}> {{ ?x <urn:b> ?y }} }} }}"
                    );
                    let o = QueryOptions {
                        max_memory_bytes: memory,
                        ..options()
                    };
                    let before = calls.load(Ordering::SeqCst);
                    let n = rows_with(&s, &q, mode, &o);
                    let made = calls.load(Ordering::SeqCst) - before;
                    let what = format!("{q} {mode:?} {memory:?}");
                    // a failed SILENT call is one empty solution, so EXISTS holds
                    assert_eq!(n, if found != negated { ROWS } else { 0 }, "{what}");
                    assert_eq!(made, 1, "{what}: {made} calls for {ROWS} rows");
                }
            }
        }
    }
}

#[test]
fn a_service_in_an_exists_is_called_once_per_distinct_key() {
    let s = store();
    let (url, calls) = endpoint(FAILING);
    for mode in [ExecutionMode::Eager, ExecutionMode::Streaming] {
        for memory in [None, Some(1152u64 << 10)] {
            let q = format!(
                "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o FILTER EXISTS {{ SERVICE SILENT <{url}> {{ ?o <urn:b> ?y }} }} }}"
            );
            let o = QueryOptions {
                max_memory_bytes: memory,
                ..options()
            };
            let before = calls.load(Ordering::SeqCst);
            assert_eq!(rows_with(&s, &q, mode, &o), ROWS, "{mode:?} {memory:?}");
            let made = calls.load(Ordering::SeqCst) - before;
            // ten distinct objects, and at most one more call per thread that raced
            assert!(
                (10..=10 + rayon::current_num_threads()).contains(&made),
                "{mode:?} {memory:?}: {made} calls"
            );
        }
    }
    // the plain eager API too
    let q = format!(
        "ASK {{ ?s <urn:p> ?o FILTER EXISTS {{ SERVICE SILENT <{url}> {{ ?a <urn:b> ?y }} }} }}"
    );
    let before = calls.load(Ordering::SeqCst);
    query(s.snapshot(), &q, &options()).unwrap();
    assert!(calls.load(Ordering::SeqCst) - before <= 1);
}
