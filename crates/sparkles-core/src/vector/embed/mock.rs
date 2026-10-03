//! A mock OpenAI-compatible embeddings endpoint for tests: `POST /v1/embeddings` on a
//! loopback port, answering a fixed vector per text. It can fail every request with a
//! status, or refuse inputs holding a marker with `400`. Not for production use.

use super::Fnv;
use parking_lot::{Mutex, MutexGuard};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// What the mock received and how it answers.
#[derive(Default)]
pub struct MockState {
    /// the dimension of the vectors it answers
    pub dimension: usize,
    /// answer every request with this status (and `Retry-After: 0`)
    pub fail: Option<u16>,
    /// answer `400` to a request with an input containing this
    pub reject: Option<String>,
    /// every input received, in order
    pub inputs: Vec<String>,
    /// requests received
    pub requests: u64,
    /// the `Authorization` header of each request
    pub auth: Vec<Option<String>>,
    /// the `model` and `dimensions` of each request
    pub models: Vec<(String, Option<u64>)>,
}

/// A running mock endpoint; it stops when dropped.
pub struct MockProvider {
    port: u16,
    state: Arc<Mutex<MockState>>,
    stop: Arc<AtomicBool>,
}

impl MockProvider {
    /// Start a mock answering vectors of `dimension`.
    pub fn start(dimension: usize) -> MockProvider {
        let l = TcpListener::bind("127.0.0.1:0").expect("binding the mock provider");
        let port = l.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(MockState {
            dimension,
            ..Default::default()
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (st, sp) = (state.clone(), stop.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                if sp.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(c) = c {
                    let st = st.clone();
                    std::thread::spawn(move || serve(c, &st));
                }
            }
        });
        MockProvider { port, state, stop }
    }

    /// The endpoint's URL.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/embeddings", self.port)
    }

    pub fn state(&self) -> MutexGuard<'_, MockState> {
        self.state.lock()
    }

    /// The vector the mock answers for `text`.
    pub fn vector(dimension: usize, text: &str) -> Vec<f32> {
        (0..dimension)
            .map(|i| {
                let mut h = Fnv::default();
                h.write(text.as_bytes());
                h.write(&(i as u64).to_le_bytes());
                ((h.0 % 2001) as f32 - 1000.0) / 1000.0
            })
            .collect()
    }
}

impl Drop for MockProvider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(c: TcpStream, state: &Mutex<MockState>) {
    let mut r = BufReader::new(match c.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let (mut len, mut auth) = (0usize, None);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap_or(0),
                "authorization" => auth = Some(v.trim().to_string()),
                _ => {}
            }
        }
    }
    let mut body = vec![0u8; len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let (status, out) = answer(state, &line, &body, auth);
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        _ => "Error",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
        out.len()
    );
    let mut c = c;
    let _ = c.write_all(resp.as_bytes());
    let _ = c.write_all(out.as_bytes());
    let _ = c.flush();
}

fn answer(
    state: &Mutex<MockState>,
    line: &str,
    body: &[u8],
    auth: Option<String>,
) -> (u16, String) {
    let mut st = state.lock();
    st.requests += 1;
    st.auth.push(auth);
    if !line.starts_with("POST /v1/embeddings") {
        return (404, r#"{"error":{"message":"not found"}}"#.into());
    }
    if let Some(s) = st.fail {
        return (s, r#"{"error":{"message":"the mock is failing"}}"#.into());
    }
    let v: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => return (400, r#"{"error":{"message":"not JSON"}}"#.into()),
    };
    let inputs: Vec<String> = match &v["input"] {
        serde_json::Value::String(s) => vec![s.clone()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect(),
        _ => return (400, r#"{"error":{"message":"no input"}}"#.into()),
    };
    st.models.push((
        v["model"].as_str().unwrap_or("").to_string(),
        v["dimensions"].as_u64(),
    ));
    if let Some(m) = &st.reject
        && inputs.iter().any(|i| i.contains(m.as_str()))
    {
        return (400, r#"{"error":{"message":"input rejected"}}"#.into());
    }
    st.inputs.extend(inputs.iter().cloned());
    let dim = v["dimensions"]
        .as_u64()
        .map_or(st.dimension, |d| d as usize);
    let data: Vec<serde_json::Value> = inputs
        .iter()
        .enumerate()
        .map(|(i, t)| serde_json::json!({"object": "embedding", "index": i, "embedding": MockProvider::vector(dim, t)}))
        .collect();
    (
        200,
        serde_json::json!({"object": "list", "data": data, "model": v["model"]}).to_string(),
    )
}
