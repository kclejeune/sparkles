//! Request-owned finite-body timers. No timer owns an engine or an admission share.
use super::{Chunk, copy_error};
use crate::ratelimit::Held;
use futures_util::Stream;
use parking_lot::Mutex;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Timeouts {
    pub idle: Duration,
    pub transfer: Option<Duration>,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(60),
            transfer: None,
        }
    }
}

type Callback = Box<dyn FnOnce() + Send + 'static>;
fn finalize_callback(kind: &'static str, callback: Callback) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)).is_err() {
        // These are the two fixed internal finalizers, not engine work or
        // public pool-waker callbacks. The abort's earlier real cause stays
        // primary; this secondary fault must not skip another finalizer or
        // body wake/notify, including during producer unwinding or late binding.
        tracing::error!(callback = kind, "stream body abort finalizer panicked");
    }
}
#[derive(Default)]
struct SignalState {
    aborted: bool,
    admission_bound: bool,
    observation_bound: bool,
    admission: Option<Callback>,
    observation: Option<Callback>,
}

/// Retains the abort fact for middleware registration races, without retaining a body.
#[derive(Clone, Default)]
pub(crate) struct Abort(Arc<Mutex<SignalState>>);
impl Abort {
    #[cfg(test)]
    pub(crate) fn is_aborted(&self) -> bool {
        self.0.lock().aborted
    }
    pub(crate) fn bind_admission(&self, callback: impl FnOnce() + Send + 'static) {
        self.bind(true, Box::new(callback));
    }
    pub(crate) fn bind_observation(&self, callback: impl FnOnce() + Send + 'static) {
        self.bind(false, Box::new(callback));
    }
    fn bind(&self, admission: bool, callback: Callback) {
        let immediate = {
            let mut state = self.0.lock();
            let bound = if admission {
                &mut state.admission_bound
            } else {
                &mut state.observation_bound
            };
            assert!(!*bound, "body lifetime callback registered twice");
            *bound = true;
            if state.aborted {
                Some(callback)
            } else {
                let slot = if admission {
                    &mut state.admission
                } else {
                    &mut state.observation
                };
                assert!(slot.is_none(), "body lifetime callback registered twice");
                *slot = Some(callback);
                None
            }
        };
        if let Some(callback) = immediate {
            finalize_callback(
                if admission {
                    "admission"
                } else {
                    "observation"
                },
                callback,
            );
        }
    }
    fn fire(&self) {
        let callbacks = {
            let mut state = self.0.lock();
            if state.aborted {
                return;
            }
            state.aborted = true;
            [state.admission.take(), state.observation.take()]
        };
        for (kind, callback) in ["admission", "observation"].into_iter().zip(callbacks) {
            if let Some(callback) = callback {
                finalize_callback(kind, callback);
            }
        }
    }
}

struct State {
    receiver: Option<mpsc::Receiver<Chunk>>,
    idle_at: Option<Instant>,
    transfer_at: Option<Instant>,
    producer_finished: bool,
    terminal_requested: bool,
    quiescent: bool,
    aborted: bool,
    dropped: bool,
    failure: Option<sparkles::Error>,
    waker: Option<Waker>,
}
struct Shared {
    state: Mutex<State>,
    changed: Notify,
    abort: Abort,
    cancelled: Arc<AtomicBool>,
    aborted: AtomicBool,
    timeouts: Timeouts,
}
struct AbortAction {
    receiver: Option<mpsc::Receiver<Chunk>>,
    waker: Option<Waker>,
}

impl Shared {
    fn remember(state: &mut State, error: &sparkles::Error) {
        if state.failure.is_none() {
            state.failure = Some(copy_error(error));
        }
    }
    fn abort_locked(state: &mut State, error: &sparkles::Error) -> Option<AbortAction> {
        Self::remember(state, error);
        if state.aborted {
            return None;
        }
        state.aborted = true;
        Some(AbortAction {
            receiver: state.receiver.take(),
            waker: state.waker.take(),
        })
    }
    fn complete_abort(&self, action: Option<AbortAction>) {
        if let Some(action) = action {
            // Buffer ownership ends before body-side admission is released. Neither
            // receiver destruction nor callbacks run while the state is locked.
            drop(action.receiver);
            self.aborted.store(true, Ordering::Release);
            self.cancelled.store(true, Ordering::Relaxed);
            self.abort.fire();
            if let Some(waker) = action.waker {
                waker.wake();
            }
            self.changed.notify_one();
        }
    }
    fn due(state: &State, now: Instant) -> bool {
        state.idle_at.is_some_and(|at| now >= at) || state.transfer_at.is_some_and(|at| now >= at)
    }
    fn expire_locked(state: &mut State, now: Instant) -> Option<AbortAction> {
        if !state.aborted && !state.terminal_requested && Self::due(state, now) {
            let limit = if state.transfer_at.is_some_and(|at| now >= at) {
                "transfer"
            } else {
                "idle"
            };
            tracing::debug!(limit, "streamed response deadline expired");
            Self::abort_locked(state, &sparkles::Error::Timeout)
        } else {
            None
        }
    }
    fn arm(&self, state: &mut State, now: Instant) -> Option<AbortAction> {
        let ready =
            state.producer_finished || state.receiver.as_ref().is_some_and(|r| !r.is_empty());
        if ready && state.idle_at.is_none() {
            match now.checked_add(self.timeouts.idle) {
                Some(at) => state.idle_at = Some(at),
                None => {
                    return Self::abort_locked(
                        state,
                        &sparkles::Error::Io(io::Error::other(
                            "response idle deadline is not representable",
                        )),
                    );
                }
            }
        } else if !ready {
            state.idle_at = None;
        }
        None
    }
    fn failure_io(state: &State) -> io::Error {
        let kind = match state.failure.as_ref() {
            Some(sparkles::Error::Timeout) => io::ErrorKind::TimedOut,
            Some(sparkles::Error::Cancelled) => io::ErrorKind::ConnectionAborted,
            _ => io::ErrorKind::Other,
        };
        io::Error::new(
            kind,
            state
                .failure
                .as_ref()
                .map_or_else(|| "response body ended".into(), ToString::to_string),
        )
    }
    fn abandoned(&self) {
        let action = {
            let mut state = self.state.lock();
            state.dropped = true;
            Self::abort_locked(&mut state, &sparkles::Error::Cancelled)
        };
        self.complete_abort(action);
    }
    async fn timer(self: Arc<Self>) {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (end, next, action) = {
                let mut state = self.state.lock();
                let end = state.aborted || state.dropped || state.terminal_requested;
                let action = if end {
                    None
                } else {
                    Self::expire_locked(&mut state, Instant::now())
                };
                (
                    end || state.aborted,
                    [state.idle_at, state.transfer_at]
                        .into_iter()
                        .flatten()
                        .min(),
                    action,
                )
            };
            self.complete_abort(action);
            if end {
                return;
            }
            match next {
                Some(at) => {
                    tokio::select! { _ = &mut notified => {}, _ = tokio::time::sleep_until(at) => {} }
                }
                None => notified.await,
            }
        }
    }
}

/// Exists only on the blocking serializer thread. Owns and joins the one timer.
pub(super) struct Producer {
    shared: Arc<Shared>,
    task: Option<JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
    held: Option<Held>,
    wait_ns: Option<Arc<AtomicU64>>,
    published: bool,
}
impl Producer {
    pub(super) fn new(
        receiver: mpsc::Receiver<Chunk>,
        timeouts: Timeouts,
        cancelled: Arc<AtomicBool>,
        held: Option<Held>,
        wait_ns: Option<Arc<AtomicU64>>,
    ) -> (Self, Receiver) {
        let now = Instant::now();
        let transfer_at = timeouts.transfer.and_then(|t| now.checked_add(t));
        let state = State {
            receiver: Some(receiver),
            idle_at: None,
            transfer_at,
            producer_finished: false,
            terminal_requested: false,
            quiescent: false,
            aborted: false,
            dropped: false,
            failure: None,
            waker: None,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            changed: Notify::new(),
            abort: Abort::default(),
            cancelled,
            aborted: AtomicBool::new(false),
            timeouts,
        });
        let runtime = tokio::runtime::Handle::current();
        let task = runtime.spawn(shared.clone().timer());
        let producer = Self {
            shared: shared.clone(),
            task: Some(task),
            runtime,
            held,
            wait_ns,
            published: false,
        };
        if timeouts.transfer.is_some() && transfer_at.is_none() {
            let action = Shared::abort_locked(
                &mut shared.state.lock(),
                &sparkles::Error::Io(io::Error::other(
                    "response transfer deadline is not representable",
                )),
            );
            shared.complete_abort(action);
        }
        (
            producer,
            Receiver {
                shared,
                done: false,
            },
        )
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if !self.shared.aborted.load(Ordering::Acquire) {
            return Ok(());
        }
        let state = self.shared.state.lock();
        if state.aborted {
            Err(Shared::failure_io(&state))
        } else {
            Ok(())
        }
    }
    pub(super) fn try_send(
        &self,
        tx: &mpsc::Sender<Chunk>,
        item: Chunk,
    ) -> io::Result<Result<(), mpsc::error::TrySendError<Chunk>>> {
        let mut state = self.shared.state.lock();
        let action = Shared::expire_locked(&mut state, Instant::now());
        if state.aborted {
            let error = Shared::failure_io(&state);
            drop(state);
            self.shared.complete_abort(action);
            return Err(error);
        }
        let result = tx.try_send(item);
        let action = if result.is_ok() {
            self.shared.arm(&mut state, Instant::now())
        } else {
            None
        };
        drop(state);
        self.shared.complete_abort(action);
        self.shared.changed.notify_one();
        if result.is_ok() {
            self.check()?;
        }
        Ok(result)
    }
    pub(super) fn permit_send(
        &self,
        permit: mpsc::Permit<'_, Chunk>,
        item: Chunk,
    ) -> io::Result<()> {
        let mut state = self.shared.state.lock();
        let action = Shared::expire_locked(&mut state, Instant::now());
        if state.aborted {
            let error = Shared::failure_io(&state);
            drop(state);
            self.shared.complete_abort(action);
            return Err(error);
        }
        permit.send(item);
        let action = self.shared.arm(&mut state, Instant::now());
        drop(state);
        self.shared.complete_abort(action);
        self.shared.changed.notify_one();
        self.check()
    }
    pub(super) fn record_error(&self, error: &sparkles::Error) {
        Shared::remember(&mut self.shared.state.lock(), error);
    }
    pub(super) fn finish(&mut self, result: sparkles::Result<()>) -> sparkles::Result<()> {
        {
            let mut state = self.shared.state.lock();
            if let Err(error) = &result {
                Shared::remember(&mut state, error);
            }
            state.producer_finished = true;
            let waker = state.waker.take();
            let action = self.shared.arm(&mut state, Instant::now());
            drop(state);
            self.shared.complete_abort(action);
            if let Some(waker) = waker {
                waker.wake();
            }
        }
        self.shared.changed.notify_one();
        let began = std::time::Instant::now();
        if let Some(task) = self.task.take() {
            if let Err(error) = self.runtime.block_on(task) {
                let action = Shared::abort_locked(
                    &mut self.shared.state.lock(),
                    &sparkles::Error::Io(io::Error::other(format!(
                        "response timer stopped unexpectedly: {error}"
                    ))),
                );
                self.shared.complete_abort(action);
            }
        }
        if let Some(wait_ns) = &self.wait_ns {
            let ns = began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            let _ = wait_ns.try_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                Some(n.saturating_add(ns))
            });
        }
        self.shared
            .state
            .lock()
            .failure
            .as_ref()
            .map_or(Ok(()), |e| Err(copy_error(e)))
    }
    pub(super) fn disconnected(&self) -> bool {
        self.shared.state.lock().dropped
    }
    pub(super) fn release_permit(&mut self) {
        drop(self.held.take());
    }
    pub(super) fn publish(&mut self) {
        let (waker, action) = {
            let mut state = self.shared.state.lock();
            state.quiescent = true;
            let action = if state.terminal_requested {
                state
                    .failure
                    .as_ref()
                    .map(copy_error)
                    .and_then(|error| Shared::abort_locked(&mut state, &error))
            } else {
                None
            };
            (state.waker.take(), action)
        };
        self.published = true;
        self.shared.complete_abort(action);
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}
impl Drop for Producer {
    fn drop(&mut self) {
        if !self.published {
            let action = Shared::abort_locked(
                &mut self.shared.state.lock(),
                &sparkles::Error::Io(io::Error::other("response producer stopped unexpectedly")),
            );
            self.shared.complete_abort(action);
            self.shared.changed.notify_one();
            if let Some(task) = self.task.take() {
                // This guard never leaves its blocking producer. Do not spawn a
                // cleanup task or detach a still-running timer from Drop.
                let joined = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    self.runtime.block_on(task)
                }));
                if !matches!(joined, Ok(Ok(()))) {
                    // The already-latched producer failure remains primary. This
                    // branch cannot publish successful EOF after a failed join.
                    tracing::error!("response timer join failed during producer cleanup");
                }
            }
            self.release_permit();
            self.publish();
        }
    }
}

pub(super) struct Receiver {
    shared: Arc<Shared>,
    done: bool,
}
impl Receiver {
    pub(super) fn abort_handle(&self) -> Abort {
        self.shared.abort.clone()
    }
}
impl Stream for Receiver {
    type Item = Chunk;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        let mut state = this.shared.state.lock();
        state.waker = Some(cx.waker().clone());
        let action = Shared::expire_locked(&mut state, Instant::now());
        let result = if state.aborted {
            Poll::Ready(Some(Err(Shared::failure_io(&state))))
        } else {
            match state
                .receiver
                .as_mut()
                .expect("live response receiver")
                .poll_recv(cx)
            {
                Poll::Ready(Some(item)) => {
                    if item.as_ref().is_ok_and(|bytes| !bytes.is_empty()) {
                        state.idle_at = None;
                    }
                    let action = this.shared.arm(&mut state, Instant::now());
                    drop(state);
                    this.shared.complete_abort(action);
                    this.shared.changed.notify_one();
                    return Poll::Ready(Some(item));
                }
                Poll::Ready(None) if state.producer_finished => {
                    state.terminal_requested = true;
                    if state.quiescent {
                        match state.failure.as_ref() {
                            Some(error) => {
                                Poll::Ready(Some(Err(io::Error::other(error.to_string()))))
                            }
                            None => Poll::Ready(None),
                        }
                    } else {
                        Poll::Pending
                    }
                }
                Poll::Ready(None) => Poll::Pending,
                other => other,
            }
        };
        if matches!(result, Poll::Ready(None) | Poll::Ready(Some(Err(_)))) {
            this.done = true;
        }
        drop(state);
        this.shared.complete_abort(action);
        this.shared.changed.notify_one();
        result
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        if !self.done {
            self.shared.abandoned();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn shared(timeouts: Timeouts) -> (Arc<Shared>, mpsc::Sender<Chunk>) {
        let (tx, rx) = mpsc::channel(4);
        let now = Instant::now();
        (
            Arc::new(Shared {
                state: Mutex::new(State {
                    receiver: Some(rx),
                    idle_at: None,
                    transfer_at: timeouts.transfer.map(|d| now + d),
                    producer_finished: false,
                    terminal_requested: false,
                    quiescent: false,
                    aborted: false,
                    dropped: false,
                    failure: None,
                    waker: None,
                }),
                changed: Notify::new(),
                abort: Abort::default(),
                cancelled: Arc::new(AtomicBool::new(false)),
                aborted: AtomicBool::new(false),
                timeouts,
            }),
            tx,
        )
    }
    fn timeouts() -> Timeouts {
        Timeouts {
            idle: Duration::from_secs(10),
            transfer: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn late_binding_after_timer_expiry_contains_both_fixed_finalizer_panics() {
        let mut facts = Vec::new();
        for panic_admission in [true, false] {
            let (shared, tx) = shared(timeouts());
            tx.try_send(Ok(axum::body::Bytes::from_static(b"queued")))
                .unwrap();
            let action = shared.arm(&mut shared.state.lock(), Instant::now());
            shared.complete_abort(action);
            let timer = tokio::spawn(shared.clone().timer());
            tokio::time::advance(Duration::from_secs(10)).await;
            timer.await.unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let escaped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                for admission in [true, false] {
                    let s = shared.clone();
                    let calls = calls.clone();
                    shared.abort.bind(
                        admission,
                        Box::new(move || {
                            assert!(s.state.try_lock().unwrap().receiver.is_none());
                            assert!(s.abort.0.try_lock().is_some());
                            calls.fetch_add(1, Ordering::Relaxed);
                            assert_ne!(
                                admission, panic_admission,
                                "injected late fixed finalizer panic"
                            );
                        }),
                    );
                }
            }))
            .is_err();
            shared.abort.fire();
            let duplicates: Vec<_> = [true, false]
                .into_iter()
                .map(|admission| {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        shared.abort.bind(admission, Box::new(|| {}));
                    }))
                    .is_err()
                })
                .collect();
            facts.push((
                escaped,
                calls.load(Ordering::Relaxed),
                matches!(shared.state.lock().failure, Some(sparkles::Error::Timeout)),
                tx.is_closed(),
                duplicates,
            ));
        }
        // Duplicate registration must also keep refusing before an abort.
        let pending = Abort::default();
        for admission in [true, false] {
            pending.bind(admission, Box::new(|| {}));
            assert!(
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    pending.bind(admission, Box::new(|| {}));
                }))
                .is_err()
            );
        }
        pending.fire();
        eprintln!("late-timer-binding-facts={facts:?}");
        assert_eq!(
            facts,
            vec![(false, 2, true, true, vec![true, true]); 2],
            "late fixed finalizer panic must remain secondary after real timer expiry"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_binding_preserves_real_budget_deferred_report_and_finalization() {
        use axum::body::Body;
        use axum::http::Request;
        use axum::response::Response;
        use futures_util::StreamExt;
        use std::task::Wake;
        use tower::ServiceExt;

        struct CountWake(Arc<AtomicUsize>);
        impl Wake for CountWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        let mut facts = Vec::new();
        for admission_first in [true, false] {
            let (mut receiver, shared, wakes, end) = tokio::time::timeout(
                Duration::from_secs(5),
                tokio::task::spawn_blocking(|| {
                    let (tx, rx) = mpsc::channel(4);
                    let (mut producer, receiver) = Producer::new(
                        rx,
                        Timeouts {
                            idle: Duration::from_millis(10),
                            transfer: None,
                        },
                        Arc::new(AtomicBool::new(false)),
                        None,
                        None,
                    );
                    producer
                        .try_send(&tx, Ok(axum::body::Bytes::from_static(b"queued")))
                        .unwrap()
                        .unwrap();
                    producer.record_error(&sparkles::Error::BudgetExceeded(sparkles::Budget {
                        kind: sparkles::BudgetKind::Memory,
                        limit: 1,
                        requested: 2,
                    }));
                    let wakes = Arc::new(AtomicUsize::new(0));
                    producer.shared.state.lock().waker =
                        Some(Waker::from(Arc::new(CountWake(wakes.clone()))));
                    // finish joins the real owned timer. The unread queued tail
                    // expires, preserving the earlier real producer budget cause.
                    let error = producer.finish(Ok(())).unwrap_err();
                    assert!(producer.task.is_none());
                    producer.release_permit();
                    producer.publish();
                    let shared = producer.shared.clone();
                    (
                        receiver,
                        shared,
                        wakes,
                        super::super::StreamEnd {
                            bytes: 6,
                            serialize_ms: 0.0,
                            error: Some(error),
                            disconnected: false,
                        },
                    )
                }),
            )
            .await
            .unwrap()
            .unwrap();
            let dir = tempfile::tempdir().unwrap();
            let state = Arc::new(
                crate::state::AppState::new(
                    dir.path(),
                    sparkles::store::StoreOptions::default(),
                    Duration::from_secs(30),
                )
                .unwrap(),
            );
            let deferred = crate::obs::DeferredReport::default();
            let (outcome, budget) = end.outcome();
            deferred.producer_done(crate::obs::RequestReport {
                outcome,
                budget,
                ..Default::default()
            });
            let abort = shared.abort.clone();
            let app = axum::Router::new()
                .route(
                    "/{ds}/sparql",
                    axum::routing::get(move || {
                        let deferred = deferred.clone();
                        let abort = abort.clone();
                        async move {
                            let mut response = Response::new(Body::empty());
                            response.extensions_mut().insert(deferred);
                            response.extensions_mut().insert(abort);
                            response
                        }
                    }),
                )
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    crate::obs::observe,
                ));
            let calls = Arc::new(AtomicUsize::new(0));
            let bind_admission = || {
                let s = shared.clone();
                let calls = calls.clone();
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    shared.abort.bind_admission(move || {
                        assert!(s.state.try_lock().unwrap().receiver.is_none());
                        assert!(s.abort.0.try_lock().is_some());
                        calls.fetch_add(1, Ordering::Relaxed);
                        panic!("injected late admission panic with real deferred report");
                    });
                }))
                .is_err()
            };
            let before = if admission_first {
                Some(bind_admission())
            } else {
                None
            };
            // This is the actual observer middleware's late registration, not a
            // synthetic observation callback or a manually attached Pending.
            let response = app
                .oneshot(Request::get("/ds/sparql").body(Body::empty()).unwrap())
                .await
                .unwrap();
            let escaped = before.unwrap_or_else(bind_admission);
            let error_body = receiver.next().await.unwrap().is_err();
            let notified = shared.changed.notified();
            tokio::pin!(notified);
            let notified = futures_util::poll!(notified).is_ready();
            shared.abort.fire();
            let metrics = crate::obs::render_prometheus(&state);
            let sample = |outcome| {
                metrics.contains(&format!(
                    "sparkles_requests_total{{dataset=\"$none\",operation=\"query\",outcome=\"{outcome}\"}} 1\n"
                ))
            };
            let first_budget = matches!(
                &shared.state.lock().failure,
                Some(sparkles::Error::BudgetExceeded(b))
                    if b.kind == sparkles::BudgetKind::Memory && b.limit == 1 && b.requested == 2
            );
            let both_bound = {
                let signal = shared.abort.0.lock();
                signal.admission_bound && signal.observation_bound
            };
            facts.push((
                escaped,
                calls.load(Ordering::Relaxed),
                wakes.load(Ordering::Relaxed),
                notified,
                first_budget,
                error_body,
                shared.cancelled.load(Ordering::Relaxed),
                both_bound,
                sample("budget") && !sample("error") && !sample("cancelled") && !sample("ok"),
                metrics.contains("sparkles_requests_active{operation=\"query\"} 0\n"),
            ));
            drop(response);
        }
        eprintln!("late-deferred-binding-facts={facts:?}");
        assert_eq!(
            facts,
            vec![(false, 1, 1, true, true, true, true, true, true, true); 2],
            "late panic must stay secondary to the real budget and deferred completion"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn callback_panic_does_not_skip_other_callback_wake_or_notify() {
        use std::task::Wake;
        struct CountWake(Arc<AtomicUsize>);
        impl Wake for CountWake {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let mut facts = Vec::new();
        for panic_admission in [true, false] {
            let (shared, tx) = shared(timeouts());
            tx.try_send(Ok(axum::body::Bytes::from_static(b"queued")))
                .unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let wakes = Arc::new(AtomicUsize::new(0));
            shared.state.lock().waker = Some(Waker::from(Arc::new(CountWake(wakes.clone()))));
            for admission in [true, false] {
                let s = shared.clone();
                let calls = calls.clone();
                shared.abort.bind(
                    admission,
                    Box::new(move || {
                        assert!(s.state.try_lock().unwrap().receiver.is_none());
                        assert!(s.abort.0.try_lock().is_some());
                        calls.fetch_add(1, Ordering::Relaxed);
                        assert_ne!(admission, panic_admission, "injected fixed callback panic");
                    }),
                );
            }
            let notified = shared.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let original = sparkles::Error::Io(io::Error::other("original producer failure"));
            let action = Shared::abort_locked(&mut shared.state.lock(), &original);
            // Only the test contains the old function's unwind, after its receiver
            // was already dropped. No live worker or timer exists in this fixture.
            let escaped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                shared.complete_abort(action)
            }))
            .is_err();
            let notified = futures_util::poll!(notified).is_ready();
            shared.abort.fire();
            facts.push((
                escaped,
                calls.load(Ordering::Relaxed),
                wakes.load(Ordering::Relaxed),
                notified,
                tx.is_closed(),
                shared.cancelled.load(Ordering::Relaxed),
                shared
                    .state
                    .lock()
                    .failure
                    .as_ref()
                    .unwrap()
                    .to_string()
                    .contains("original producer failure"),
            ));
        }
        eprintln!("fixed-callback-finalization-facts={facts:?}");
        assert_eq!(
            facts,
            vec![(false, 2, 1, true, true, true, true); 2],
            "fixed callback panic must not skip the other callback, moved wake or notify"
        );
    }

    #[cfg(unix)]
    #[test]
    fn callback_panic_during_producer_unwind_is_contained_in_isolated_child() {
        const CHILD: &str = "SPARKLES_ITEM7_CALLBACK_UNWIND_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                use futures_util::StreamExt;
                let calls = Arc::new(AtomicUsize::new(0));
                let observed = calls.clone();
                let (send, receive) = tokio::sync::oneshot::channel();
                let worker = tokio::task::spawn_blocking(move || {
                    let (_tx, rx) = mpsc::channel(4);
                    let (producer, receiver) =
                        Producer::new(rx, timeouts(), Arc::new(AtomicBool::new(false)), None, None);
                    producer.record_error(&sparkles::Error::Io(io::Error::other(
                        "first known producer error",
                    )));
                    let abort = receiver.abort_handle();
                    abort.bind_admission(|| {
                        panic!("injected fixed admission callback panic during producer unwind")
                    });
                    abort.bind_observation(move || {
                        observed.fetch_add(1, Ordering::Relaxed);
                    });
                    assert!(send.send(receiver).is_ok());
                    // Producer::Drop now runs while this real worker is unwinding.
                    panic!("injected original producer unwind");
                });
                let mut receiver = receive.await.unwrap();
                assert!(worker.await.unwrap_err().is_panic());
                assert!(receiver.shared.state.lock().quiescent);
                assert_eq!(calls.load(Ordering::Relaxed), 1);
                assert!(
                    receiver
                        .shared
                        .state
                        .lock()
                        .failure
                        .as_ref()
                        .unwrap()
                        .to_string()
                        .contains("first known producer error")
                );
                assert!(receiver.next().await.unwrap().is_err());
            });
            println!("item7-callback-double-unwind-contained");
            return;
        }
        // The old destructor's double panic aborts this child. Disable core dumps
        // without unsafe code or changing any host/global setting.
        let result = std::process::Command::new("bash")
            .args(["-c", "ulimit -c 0; exec \"$@\"", "item7-callback-child"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "http::stream::lifetime::tests::callback_panic_during_producer_unwind_is_contained_in_isolated_child", "--nocapture"])
            .env(CHILD, "1").output().unwrap();
        assert!(
            result.status.success(),
            "callback panic must not double-unwind the producer process: status={} stderr={}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout)
                .contains("item7-callback-double-unwind-contained")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn failed_timer_join_is_body_failure_and_never_clean_eof() {
        use futures_util::StreamExt;
        let (mut receiver, failed, joined) = tokio::task::spawn_blocking(|| {
            let (tx, rx) = mpsc::channel(4);
            let (mut producer, receiver) =
                Producer::new(rx, timeouts(), Arc::new(AtomicBool::new(false)), None, None);
            // Join the original owned timer before installing an injected panic.
            // No production fault switch or detached test task is needed.
            let timer = producer.task.take().unwrap();
            timer.abort();
            assert!(producer.runtime.block_on(timer).unwrap_err().is_cancelled());
            producer.task = Some(
                producer
                    .runtime
                    .spawn(async { panic!("injected timer failure") }),
            );
            drop(tx);
            let failed = producer.finish(Ok(())).is_err();
            let joined = producer.task.is_none();
            producer.release_permit();
            producer.publish();
            (receiver, failed, joined)
        })
        .await
        .unwrap();
        assert!(failed && joined);
        assert!(receiver.next().await.unwrap().is_err());
        assert!(receiver.next().await.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn producer_guard_unwind_joins_timer_and_publishes_failure() {
        use futures_util::StreamExt;
        let (send, receive) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let (_tx, rx) = mpsc::channel(4);
            let (_producer, receiver) =
                Producer::new(rx, timeouts(), Arc::new(AtomicBool::new(false)), None, None);
            assert!(send.send(receiver).is_ok());
            panic!("injected producer guard unwind");
        });
        let mut receiver = receive.await.unwrap();
        let panic = worker.await.unwrap_err().is_panic();
        assert!(panic);
        assert!(receiver.shared.state.lock().quiescent);
        assert!(receiver.next().await.unwrap().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn unrepresentable_idle_deadline_refuses_without_wrapping() {
        let (shared, tx) = shared(Timeouts {
            idle: Duration::MAX,
            transfer: None,
        });
        tx.try_send(Ok(axum::body::Bytes::from_static(b"ready")))
            .unwrap();
        let action = shared.arm(&mut shared.state.lock(), Instant::now());
        shared.complete_abort(action);
        assert!(shared.state.lock().aborted);
        assert!(matches!(
            shared.state.lock().failure,
            Some(sparkles::Error::Io(_))
        ));
        assert!(tx.is_closed());
    }

    #[test]
    fn runtime_shutdown_cancels_timer_and_real_blocking_owner_joins_it() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        runtime.spawn_blocking(move || {
            let (tx, rx) = mpsc::channel(4);
            let (mut producer, receiver) = Producer::new(
                rx,
                Timeouts::default(),
                Arc::new(AtomicBool::new(false)),
                None,
                None,
            );
            assert!(ready_tx.send(receiver).is_ok());
            drop(tx);
            let failed = producer.finish(Ok(())).is_err();
            let joined = producer.task.is_none();
            producer.release_permit();
            producer.publish();
            done_tx.send((failed, joined)).unwrap();
        });
        let receiver = ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        runtime.shutdown_timeout(Duration::from_secs(5));
        let (failed, joined) = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            failed && joined,
            "shutdown must never turn timer loss into clean EOF"
        );
        assert!(receiver.shared.state.lock().aborted);
        assert!(receiver.shared.state.lock().quiescent);
    }

    #[tokio::test(start_paused = true)]
    async fn independent_timer_aborts_queued_data_without_body_polling() {
        let (shared, tx) = shared(timeouts());
        tx.try_send(Ok(axum::body::Bytes::from_static(b"ready")))
            .unwrap();
        let action = shared.arm(&mut shared.state.lock(), Instant::now());
        shared.complete_abort(action);
        let timer = tokio::spawn(shared.clone().timer());
        tokio::time::advance(Duration::from_secs(10)).await;
        timer.await.unwrap();
        assert!(shared.cancelled.load(Ordering::Relaxed));
        assert!(shared.state.lock().receiver.is_none());
        assert!(matches!(
            shared.state.lock().failure,
            Some(sparkles::Error::Timeout)
        ));
        assert!(tx.is_closed());
    }

    #[tokio::test(start_paused = true)]
    async fn empty_computing_queue_disarms_idle_but_total_still_expires() {
        let (shared, _tx) = shared(Timeouts {
            transfer: Some(Duration::from_secs(30)),
            ..timeouts()
        });
        let timer = tokio::spawn(shared.clone().timer());
        tokio::time::advance(Duration::from_secs(11)).await;
        tokio::task::yield_now().await;
        let premature = shared.state.lock().aborted;
        tokio::time::advance(Duration::from_secs(19)).await;
        timer.await.unwrap();
        assert!(!premature);
        assert!(shared.state.lock().aborted);
    }

    #[tokio::test(start_paused = true)]
    async fn final_unread_eof_is_ready_and_remains_timed() {
        let (shared, tx) = shared(timeouts());
        drop(tx);
        shared.state.lock().producer_finished = true;
        let action = shared.arm(&mut shared.state.lock(), Instant::now());
        shared.complete_abort(action);
        let timer = tokio::spawn(shared.clone().timer());
        tokio::time::advance(Duration::from_secs(10)).await;
        timer.await.unwrap();
        assert!(shared.state.lock().aborted);
    }

    #[tokio::test(start_paused = true)]
    async fn terminal_request_stops_timer_without_publishing_early_eof() {
        use futures_util::StreamExt;
        let (shared, tx) = shared(timeouts());
        drop(tx);
        shared.state.lock().producer_finished = true;
        let timer = tokio::spawn(shared.clone().timer());
        let mut receiver = Receiver {
            shared: shared.clone(),
            done: false,
        };
        let before = futures_util::poll!(receiver.next());
        shared.changed.notify_one();
        timer.await.unwrap();
        let unpublished = !shared.state.lock().quiescent;
        shared.state.lock().quiescent = true;
        let end = receiver.next().await;
        assert!(before.is_pending());
        assert!(unpublished);
        assert!(end.is_none());
        assert!(!shared.state.lock().aborted);
    }

    #[tokio::test(start_paused = true)]
    async fn expiry_wins_at_deadline_and_empty_chunks_do_not_reset_idle() {
        use futures_util::StreamExt;
        let (shared, tx) = shared(timeouts());
        tx.try_send(Ok(axum::body::Bytes::new())).unwrap();
        tx.try_send(Ok(axum::body::Bytes::from_static(b"ready")))
            .unwrap();
        let start = Instant::now();
        assert!(shared.arm(&mut shared.state.lock(), start).is_none());
        let mut receiver = Receiver {
            shared: shared.clone(),
            done: false,
        };
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(receiver.next().await.unwrap().unwrap().is_empty());
        assert_eq!(
            shared.state.lock().idle_at,
            Some(start + Duration::from_secs(10))
        );
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            receiver.next().await.unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[tokio::test(start_paused = true)]
    async fn progress_resets_idle_but_never_total() {
        use futures_util::StreamExt;
        let (shared, tx) = shared(Timeouts {
            transfer: Some(Duration::from_secs(25)),
            ..timeouts()
        });
        let total = shared.state.lock().transfer_at;
        let mut receiver = Receiver {
            shared: shared.clone(),
            done: false,
        };
        for _ in 0..3 {
            tx.try_send(Ok(axum::body::Bytes::from_static(b"ready")))
                .unwrap();
            assert!(
                shared
                    .arm(&mut shared.state.lock(), Instant::now())
                    .is_none()
            );
            tokio::time::advance(Duration::from_secs(8)).await;
            assert!(receiver.next().await.unwrap().is_ok());
            assert_eq!(shared.state.lock().transfer_at, total);
        }
        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(receiver.next().await.unwrap().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn earlier_failure_and_late_callbacks_are_preserved_once_outside_locks() {
        let (shared, tx) = shared(timeouts());
        let early = sparkles::Error::Io(io::Error::other("original failure"));
        Shared::remember(&mut shared.state.lock(), &early);
        let action = Shared::abort_locked(&mut shared.state.lock(), &sparkles::Error::Timeout);
        shared.complete_abort(action);
        let calls = Arc::new(AtomicUsize::new(0));
        for admission in [true, false] {
            let s = shared.clone();
            let calls = calls.clone();
            shared.abort.bind(
                admission,
                Box::new(move || {
                    assert!(s.state.try_lock().unwrap().receiver.is_none());
                    assert!(s.abort.0.try_lock().is_some());
                    calls.fetch_add(1, Ordering::Relaxed);
                }),
            );
        }
        shared.abort.fire();
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(
            shared
                .state
                .lock()
                .failure
                .as_ref()
                .unwrap()
                .to_string()
                .contains("original failure")
        );
        assert!(tx.is_closed());
    }
}
