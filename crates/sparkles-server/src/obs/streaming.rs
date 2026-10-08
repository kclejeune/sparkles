//! Complete a cursor request once both its producer and response body terminate.

use super::{Outcome, Pending, RequestReport};
use axum::body::{Body, BodyDataStream, Bytes};
use futures_util::Stream;
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

    pub(super) fn observe(&self, pending: Pending, status: u16) {
        let mut state = self.0.lock();
        state.pending = Some(pending);
        state.status = status;
        drop(state);
        self.complete();
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
        Body::from_stream(ObservedBody {
            inner: body.into_data_stream(),
            report: self.clone(),
            ended: false,
        })
    }
}

struct ObservedBody {
    inner: BodyDataStream,
    report: DeferredReport,
    ended: bool,
}

impl Stream for ObservedBody {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.ended {
            return Poll::Ready(None);
        }
        let result = Pin::new(&mut this.inner).poll_next(cx);
        match &result {
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
}

impl Drop for ObservedBody {
    fn drop(&mut self) {
        if !self.ended {
            self.report.body_done(BodyEnd::Dropped);
        }
    }
}
