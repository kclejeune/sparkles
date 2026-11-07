//! Deferred completion must keep its request's log destination across threads.
use super::*;
use std::io;
use tracing_subscriber::layer::SubscriberExt;

#[derive(Clone, Default)]
struct Buffer(Arc<std::sync::Mutex<Vec<u8>>>);

impl io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Buffer;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Buffer {
    fn access(&self) -> Vec<J> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<J>(line).unwrap())
            .filter(|line| line["target"] == "sparkles::access")
            .collect()
    }
}

fn dispatch(buffer: Buffer, filter_spans: bool) -> tracing::Dispatch {
    let layer = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(true)
        .with_span_list(false)
        .with_writer(buffer);
    tracing::Dispatch::new(
        tracing_subscriber::registry()
            .with(tracing_subscriber::filter::filter_fn(move |metadata| {
                !filter_spans || metadata.is_event()
            }))
            .with(layer),
    )
}

fn completed_on_foreign_thread(body_first: bool, filter_spans: bool) {
    // Both dispatchers stay live, so tracing cannot use a single-subscriber
    // interest-cache shortcut. No global subscriber or timing wait is needed.
    let origin = Buffer::default();
    let foreign = Buffer::default();
    let origin_dispatch = dispatch(origin.clone(), filter_spans);
    let foreign_dispatch = dispatch(foreign.clone(), false);
    let dir = tempfile::tempdir().unwrap();
    let mut state = AppState::new(
        dir.path(),
        sparkles::store::StoreOptions::default(),
        Duration::from_secs(30),
    )
    .unwrap();
    state.access_log = true;
    state.metrics = Metrics::new(true, 8);
    let state = Arc::new(state);
    let deferred = DeferredReport::default();
    tracing::dispatcher::with_default(&origin_dispatch, || {
        let span = tracing::info_span!(
            "request",
            route = "/{ds}/sparql",
            request_id = "deferred-dispatch"
        );
        assert_eq!(span.is_disabled(), filter_spans);
        deferred.observe(
            Pending::new(state.clone(), span, Op::Query, None, false),
            200,
            false,
        );
    });
    let completion = deferred.clone();
    std::thread::spawn(move || {
        tracing::dispatcher::with_default(&foreign_dispatch, || {
            let report = RequestReport {
                operation: Some(Op::Query),
                outcome: Some(Outcome::Budget),
                budget: Some(BudgetKind::ResultBytes),
                rows: Some(7),
                response_bytes: Some(123),
                ..Default::default()
            };
            if body_first {
                completion.abort_body();
                completion.producer_done(report);
            } else {
                completion.producer_done(report);
                completion.abort_body();
            }
            // Logical abort and eventual body destruction may both finalize it.
            completion.abort_body();
        });
    })
    .join()
    .unwrap();
    let (_, series) = state.metrics.series(None);
    let query = &series.ops[Op::Query.index()];
    assert_eq!(
        query.outcomes[Outcome::Budget.index()].load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        query.outcomes[Outcome::Ok.index()].load(Ordering::Relaxed),
        0
    );
    assert_eq!(query.response_bytes.load(Ordering::Relaxed), 123);
    assert_eq!(series.result_rows.load(Ordering::Relaxed), 7);
    assert_eq!(
        series.budget[budget_index(BudgetKind::ResultBytes)].load(Ordering::Relaxed),
        1
    );
    assert_eq!(
        state.metrics.active[Op::Query.index()].load(Ordering::Relaxed),
        0
    );
    let access = origin.access();
    assert_eq!(
        access.len(),
        1,
        "deferred completion lost its originating access dispatcher"
    );
    let fields = &access[0]["fields"];
    assert_eq!(fields["status"], 200);
    assert_eq!(fields["outcome"], "budget");
    assert_eq!(fields["rows"], 7);
    assert_eq!(fields["response_bytes"], 123);
    assert!(
        foreign.access().is_empty(),
        "another request's dispatcher received the report"
    );
    drop(deferred);
    assert_eq!(origin.access().len(), 1, "final Drop must not report twice");
}

#[test]
fn producer_first_deferred_abort_keeps_originating_dispatcher() {
    completed_on_foreign_thread(false, false);
}

#[test]
fn body_first_deferred_abort_keeps_originating_dispatcher() {
    completed_on_foreign_thread(true, false);
}

#[test]
fn filtered_request_span_still_keeps_originating_access_dispatcher() {
    completed_on_foreign_thread(false, true);
}
