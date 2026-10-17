//! A Sparkles server that fails after a streamed response has started aborts the
//! transfer: the connection closes without the final chunk. The rows before the cut are
//! well formed, so the client must report the transfer error rather than a shorter answer.

use sparkles_client::{Client, Execution, QueryOptions};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Answer every request with `content_type`, one chunk holding `body`, and then a closed
/// connection. Each request's head is kept in `seen`.
async fn cutting_server(
    content_type: &'static str,
    body: &'static str,
) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<String>>> = Default::default();
    let heads = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let heads = heads.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = socket.read(&mut buf).await else {
                        return;
                    };
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                }
                heads
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\n\
                     transfer-encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                // no terminating zero-length chunk: the transfer is cut off
            });
        }
    });
    (base, seen)
}

#[tokio::test]
async fn a_cut_off_graph_answer_is_an_error() {
    let (base, _) = cutting_server(
        "application/n-triples",
        "<urn:s> <urn:p> <urn:o> .\n<urn:s> <urn:p> <urn:o2> .\n",
    )
    .await;
    let client = Client::new(&base).unwrap();
    let mut triples = client
        .dataset("ds")
        .construct("CONSTRUCT {?s ?p ?o} WHERE {?s ?p ?o}")
        .await
        .unwrap();
    let mut read = 0;
    let mut failed = false;
    while let Some(t) = triples.next().await {
        match t {
            Ok(_) => read += 1,
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    assert!(
        failed,
        "a truncated N-Triples body ended cleanly after {read} triples"
    );
}

#[tokio::test]
async fn a_cut_off_select_answer_is_an_error() {
    let (base, seen) = cutting_server(
        "application/sparql-results+json",
        r#"{"head":{"vars":["s"]},"results":{"bindings":[{"s":{"type":"uri","value":"urn:s"}}"#,
    )
    .await;
    let client = Client::new(&base).unwrap();
    let ds = client.dataset("ds");
    let opts = QueryOptions::new().execution(Execution::Eager);
    let result = ds.query_with("SELECT ?s WHERE {?s ?p ?o}", &opts).await;
    let failed = match result.and_then(|r| r.into_solutions()) {
        Err(_) => true,
        Ok(mut solutions) => {
            let mut failed = false;
            while let Some(s) = solutions.next().await {
                if s.is_err() {
                    failed = true;
                    break;
                }
            }
            failed
        }
    };
    assert!(failed, "a truncated JSON body ended cleanly");
    // the opt-out reaches the server
    assert!(seen.lock().unwrap()[0].contains("execution=eager"));
}
