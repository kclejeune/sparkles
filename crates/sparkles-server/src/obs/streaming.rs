//! Complete a cursor request once both its producer and response body terminate.

use super::{Outcome, Pending, RequestReport};
use axum::body::{Body, Bytes};
use http_body::{Body as _, Frame};
use parking_lot::Mutex;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

#[derive(Clone, Default)]
pub(crate) struct DeferredReport(Arc<Mutex<State>>);

#[derive(Default)]
struct State {
    pending: Option<Pending>,
    report: Option<RequestReport>,
    status: u16,
    /// a HEAD request, whose body the server drops on purpose
    head: bool,
    body: Option<BodyEnd>,
}

#[derive(Clone, Copy)]
enum BodyEnd {
    Complete,
    Failed,
    Dropped,
}

impl DeferredReport {
    pub(crate) fn producer_done(&self, report: RequestReport) {
        self.0.lock().report = Some(report);
        self.complete();
    }

    #[cfg(test)]
    pub(crate) fn snapshot_outcome(
        &self,
    ) -> Option<(Option<Outcome>, Option<sparkles::BudgetKind>)> {
        self.0.lock().report.as_ref().map(|r| (r.outcome, r.budget))
    }

    pub(super) fn observe(&self, pending: Pending, status: u16, head: bool) {
        let mut state = self.0.lock();
        state.pending = Some(pending);
        state.status = status;
        state.head = head;
        drop(state);
        self.complete();
    }

    pub(crate) fn abort_body(&self) {
        self.body_done(BodyEnd::Failed);
    }

    fn body_done(&self, end: BodyEnd) {
        self.0.lock().body.get_or_insert(end);
        self.complete();
    }

    fn complete(&self) {
        let ready = {
            let mut state = self.0.lock();
            if state.pending.is_none() || state.report.is_none() || state.body.is_none() {
                return;
            }
            let pending = state.pending.take().expect("pending report");
            let mut report = state.report.take().expect("producer report");
            let mut status = state.status;
            if state.head {
                // The server drops the body of a HEAD response, so the producer sees a
                // disconnect and the body never ends. Neither is the client's doing, and
                // other outcomes such as a timeout stand.
                if report.outcome == Some(Outcome::Cancelled) {
                    report.outcome = None;
                }
                state.body = Some(BodyEnd::Complete);
            }
            match state.body.expect("body end") {
                BodyEnd::Complete => (),
                BodyEnd::Failed => {
                    report.outcome.get_or_insert(Outcome::Error);
                }
                BodyEnd::Dropped => {
                    report.outcome.get_or_insert(Outcome::Cancelled);
                    status = 499;
                }
            }
            (pending, status, report)
        };
        ready.0.complete(ready.1, &ready.2);
    }

    pub(super) fn wrap(&self, body: Body) -> Body {
        Body::new(ObservedBody {
            remaining: body.size_hint().exact(),
            inner: body,
            report: self.clone(),
            ended: false,
        })
    }
}

struct ObservedBody {
    inner: Body,
    report: DeferredReport,
    /// The data bytes remaining when the inner body advertises an exact length.
    remaining: Option<u64>,
    ended: bool,
}

impl http_body::Body for ObservedBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        let result = Pin::new(&mut this.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(remaining), Some(data)) = (this.remaining, frame.data_ref()) {
                    this.remaining = remaining.checked_sub(data.len() as u64);
                }
                if this.inner.is_end_stream() {
                    this.ended = true;
                    this.report.body_done(BodyEnd::Complete);
                }
            }
            Poll::Ready(None) => {
                this.ended = true;
                this.report.body_done(BodyEnd::Complete);
            }
            Poll::Ready(Some(Err(_))) => {
                this.ended = true;
                this.report.body_done(BodyEnd::Failed);
            }
            _ => (),
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.ended || self.inner.is_end_stream()
    }

    fn size_hint(&self) -> http_body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for ObservedBody {
    fn drop(&mut self) {
        if !self.ended {
            // Hyper can skip polling an empty body or stop once it has taken all
            // bytes advertised by a fixed-length body.
            let end = if self.inner.is_end_stream() || self.remaining == Some(0) {
                BodyEnd::Complete
            } else {
                BodyEnd::Dropped
            };
            self.report.body_done(end);
        }
    }
}
