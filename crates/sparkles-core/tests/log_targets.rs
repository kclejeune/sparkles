//! Log targets of the engine. The engine moved from the package `sparkles` to
//! `sparkles-core`, so the module path of its events changed from `sparkles::…` to
//! `sparkles_core::…`. Every event and span names its target explicitly under the
//! `sparkles::` names, so `RUST_LOG=sparkles::store=debug` and the targets in JSON logs
//! stay as they were.

use sparkles_core::commit::CommitKind;
use sparkles_core::guard::WriteOptions;
use sparkles_core::sparql::QueryOptions;
use sparkles_core::sparql::update::update_as;
use sparkles_core::store::{Store, StoreOptions};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::{EnvFilter, Layer};

/// The targets of the events that pass the filter.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> Layer<S> for Capture {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        self.0
            .lock()
            .unwrap()
            .push(event.metadata().target().to_string());
    }
}

/// A write that bypasses write-time validation, which the store logs as a warning.
fn bypassed_write(filter: &str) -> Vec<String> {
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry()
        .with(EnvFilter::new(filter))
        .with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        let s = Store::in_memory(StoreOptions::default());
        let opts = QueryOptions {
            write: WriteOptions {
                bypass_validation: true,
                ..Default::default()
            },
            ..Default::default()
        };
        update_as(
            &s,
            "INSERT DATA { <urn:s> <urn:p> 1 }",
            &opts,
            CommitKind::Update,
        )
        .unwrap();
    });
    capture.0.lock().unwrap().clone()
}

#[test]
fn a_store_directive_enables_store_events() {
    let seen = bypassed_write("sparkles::store=debug");
    assert!(seen.iter().any(|t| t == "sparkles::store"), "{seen:?}");
    // the crate-wide default filter of `sparkles serve` matches them too
    let seen = bypassed_write("sparkles=info");
    assert!(seen.iter().any(|t| t == "sparkles::store"), "{seen:?}");
    // a directive for another module does not
    let seen = bypassed_write("sparkles::sparql=debug");
    assert!(seen.is_empty(), "{seen:?}");
}

/// The engine's sources, with the offset of each `tracing::…!(` call that does not start
/// with `target: "sparkles::`.
fn untargeted(dir: &Path, out: &mut Vec<String>) {
    const MACROS: [&str; 12] = [
        "trace",
        "debug",
        "info",
        "warn",
        "error",
        "event",
        "span",
        "trace_span",
        "debug_span",
        "info_span",
        "warn_span",
        "error_span",
    ];
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            untargeted(&path, out);
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        for (at, _) in text.match_indices("tracing::") {
            let rest = &text[at + "tracing::".len()..];
            let Some(bang) = rest.find("!(") else {
                continue;
            };
            if !MACROS.contains(&&rest[..bang]) {
                continue;
            }
            let args = rest[bang + 2..].trim_start();
            if !args.starts_with("target: \"sparkles::") {
                let line = text[..at].lines().count() + 1;
                out.push(format!("{}:{line}", path.display()));
            }
        }
    }
}

#[test]
fn every_engine_event_names_a_sparkles_target() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut missing = Vec::new();
    untargeted(&src, &mut missing);
    assert!(
        missing.is_empty(),
        "these tracing calls need `target: \"sparkles::<module>\"`, the module path the \
         event had in the `sparkles` package:\n{}",
        missing.join("\n")
    );
}
