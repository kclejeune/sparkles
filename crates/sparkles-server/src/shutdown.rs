//! Shutdown with a grace period (`serve --shutdown-grace`).
//!
//! On SIGTERM or SIGINT the server reports `draining` on `/$/ready`, stops accepting
//! connections and lets the requests in flight finish. Requests still running when the
//! grace period ends are cancelled: shutting the runtime down drops their handler
//! futures, whose guards set the requests' cancellation flags. A cancelled query stops
//! at its next check. A cancelled write stops before its commit and commits nothing; a
//! write already committing finishes its commit. The runtime waits [`CANCEL_WAIT`] for
//! that, and the server then flushes what it keeps in memory and exits.

use std::future::Future;
use std::time::Duration;

/// How long requests cancelled at the end of the grace period have to stop.
pub const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// How the requests in flight at shutdown ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Drained {
    /// every request finished (or the server stopped without a signal)
    Finished,
    /// the grace period ended with requests still running; they are cancelled when the
    /// runtime shuts down
    GraceElapsed,
}

/// Run `serve`, a server future that shuts down gracefully once its shutdown signal
/// fires, until it ends. `draining` fires when that signal does; from then on the
/// requests in flight have `grace` to finish.
pub async fn drain<S>(
    serve: S,
    draining: tokio::sync::oneshot::Receiver<()>,
    grace: Duration,
) -> std::io::Result<Drained>
where
    S: Future<Output = std::io::Result<()>>,
{
    tokio::pin!(serve);
    let mut draining = draining;
    tokio::select! {
        r = &mut serve => return r.map(|()| Drained::Finished),
        signalled = &mut draining => {
            if signalled.is_err() {
                // no signal will come: serve as long as the server runs
                return serve.await.map(|()| Drained::Finished);
            }
        }
    }
    tokio::select! {
        r = &mut serve => r.map(|()| Drained::Finished),
        () = tokio::time::sleep(grace) => Ok(Drained::GraceElapsed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AppState, DbType};
    use sparkles::io::Source;
    use sparkles::store::StoreOptions;
    use std::sync::Arc;
    use std::time::Instant;

    /// A server on a loopback port with a dataset `chain` on which a path query runs for
    /// seconds, and the shutdown plumbing of `serve`.
    struct Running {
        _dir: tempfile::TempDir,
        rt: tokio::runtime::Runtime,
        state: Arc<AppState>,
        url: String,
        signal: tokio::sync::oneshot::Sender<()>,
        serve: std::pin::Pin<Box<dyn Future<Output = std::io::Result<Drained>> + Send>>,
    }

    fn start(grace: Duration, nodes: usize) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let state = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(60)).unwrap(),
        );
        let chain = state.attach("chain", DbType::Mem, None).unwrap();
        let mut nt = String::new();
        for i in 0..nodes {
            nt.push_str(&format!("<urn:n{i}> <urn:next> <urn:n{}> .\n", i + 1));
        }
        chain
            .store
            .load(&[Source::from_bytes(
                nt.into_bytes(),
                oxrdfio::RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (signal, signalled) = tokio::sync::oneshot::channel::<()>();
        let (tx, draining) = tokio::sync::oneshot::channel();
        let st = state.clone();
        let shutdown = async move {
            let _ = signalled.await;
            st.set_phase(crate::obs::Phase::Draining);
            let _ = tx.send(());
        };
        let app = crate::http::router(state.clone())
            .into_make_service_with_connect_info::<crate::auth::Peer>();
        let serve = Box::pin(drain(
            std::future::IntoFuture::into_future(
                axum::serve(listener, app).with_graceful_shutdown(shutdown),
            ),
            draining,
            grace,
        ));
        Running {
            _dir: dir,
            rt,
            state,
            url,
            signal,
            serve,
        }
    }

    /// Send `body` to `path` from another thread; the result says whether a response
    /// came back, and with which status.
    fn send(url: &str, path: &str, ct: &str, body: &str) -> std::thread::JoinHandle<Option<u16>> {
        let (url, path, ct, body) = (
            url.to_string(),
            path.to_string(),
            ct.to_string(),
            body.to_string(),
        );
        std::thread::spawn(move || {
            reqwest::blocking::Client::new()
                .post(format!("{url}{path}"))
                .header("content-type", ct)
                .body(body)
                .timeout(Duration::from_secs(60))
                .send()
                .ok()
                .map(|r| r.status().as_u16())
        })
    }

    /// Wait until `n` requests of the operation are in flight.
    fn wait_active(state: &AppState, op: crate::obs::Op, n: i64) {
        let t0 = Instant::now();
        while state.metrics.active(op) != n {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "requests never started"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    const HEAVY_UPDATE: &str = "INSERT { <urn:result> <urn:pairs> ?n } WHERE { \
        SELECT (COUNT(*) AS ?n) WHERE { ?x <urn:next>* ?y } }";

    #[test]
    fn requests_past_the_grace_period_are_cancelled_and_commit_nothing() {
        let r = start(Duration::from_millis(100), 3000);
        let before = r.state.get("chain").unwrap().store.snapshot().len();
        let head = r.state.get("chain").unwrap().store.head_commit().seq;
        // a write whose WHERE clause runs for seconds
        let write = send(
            &r.url,
            "/chain/update",
            "application/sparql-update",
            HEAVY_UPDATE,
        );
        let Running {
            rt,
            state,
            signal,
            serve,
            _dir,
            ..
        } = r;
        let serving = rt.spawn(serve);
        wait_active(&state, crate::obs::Op::Update, 1);
        signal.send(()).unwrap();
        let t0 = Instant::now();
        let drained = rt.block_on(serving).unwrap().unwrap();
        assert_eq!(drained, Drained::GraceElapsed);
        assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
        // the runtime's shutdown cancels the write, which stops well within the wait
        let t1 = Instant::now();
        rt.shutdown_timeout(CANCEL_WAIT);
        assert!(t1.elapsed() < CANCEL_WAIT, "{:?}", t1.elapsed());
        assert_eq!(write.join().unwrap(), None, "the client got no response");
        let ds = state.get("chain").unwrap();
        assert_eq!(ds.store.snapshot().len(), before, "nothing was committed");
        assert_eq!(ds.store.head_commit().seq, head);
        assert_eq!(state.phase(), crate::obs::Phase::Draining);
    }

    #[test]
    fn requests_within_the_grace_period_finish() {
        let r = start(Duration::from_secs(30), 800);
        let Running {
            rt,
            state,
            signal,
            serve,
            url,
            _dir,
        } = r;
        let serving = rt.spawn(serve);
        // a query that runs for a while
        let query = send(
            &url,
            "/chain/sparql",
            "application/sparql-query",
            "SELECT (COUNT(*) AS ?n) WHERE { ?x <urn:next>* ?y }",
        );
        wait_active(&state, crate::obs::Op::Query, 1);
        signal.send(()).unwrap();
        let drained = rt.block_on(serving).unwrap().unwrap();
        assert_eq!(drained, Drained::Finished);
        assert_eq!(query.join().unwrap(), Some(200));
        assert_eq!(state.metrics.active(crate::obs::Op::Query), 0);
        // no new connections once draining
        let late = send(&url, "/chain/sparql", "application/sparql-query", "ASK {}");
        assert_eq!(late.join().unwrap(), None);
        rt.shutdown_timeout(CANCEL_WAIT);
    }
}
