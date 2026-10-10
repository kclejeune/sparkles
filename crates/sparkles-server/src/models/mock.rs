//! A mock model provider for tests: an HTTP endpoint on a loopback port that records
//! every request and answers with a handler's status and JSON. It speaks no protocol of
//! its own, so one mock stands for any kind, and the handler decides.

use parking_lot::Mutex;
use serde_json::Value;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One request the mock received.
#[derive(Clone, Debug)]
pub struct Received {
    pub path: String,
    /// lower-case header names
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl Received {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

type Handler = dyn Fn(&Received, usize) -> (u16, Value) + Send + Sync;

/// A running mock; it stops when dropped.
pub struct MockModel {
    port: u16,
    pub log: Arc<Mutex<Vec<Received>>>,
    stop: Arc<AtomicBool>,
}

impl MockModel {
    /// Start a mock whose answers come from `handler`, called with each request and
    /// its 0-based number.
    pub fn start(
        handler: impl Fn(&Received, usize) -> (u16, Value) + Send + Sync + 'static,
    ) -> MockModel {
        let l = TcpListener::bind("127.0.0.1:0").expect("binding the mock model");
        let port = l.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<Received>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let (lg, sp) = (log.clone(), stop.clone());
        std::thread::spawn(move || {
            for c in l.incoming() {
                if sp.load(Ordering::SeqCst) {
                    return;
                }
                if let Ok(c) = c {
                    let (lg, h) = (lg.clone(), handler.clone());
                    std::thread::spawn(move || serve(c, &lg, &*h));
                }
            }
        });
        MockModel { port, log, stop }
    }

    /// The base URL (an endpoint for any kind).
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    pub fn requests(&self) -> Vec<Received> {
        self.log.lock().clone()
    }
}

impl Drop for MockModel {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(c: TcpStream, log: &Mutex<Vec<Received>>, handler: &Handler) {
    let mut r = BufReader::new(match c.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    });
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut len = 0usize;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).is_err() || h.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let k = k.trim().to_ascii_lowercase();
            if k == "content-length" {
                len = v.trim().parse().unwrap_or(0);
            }
            headers.push((k, v.trim().to_string()));
        }
    }
    let mut body = vec![0u8; len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let rec = Received {
        path,
        headers,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    };
    let n = {
        let mut l = log.lock();
        l.push(rec.clone());
        l.len() - 1
    };
    let (status, out) = handler(&rec, n);
    let out = out.to_string();
    let resp = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n",
        out.len()
    );
    let mut c = c;
    let _ = c.write_all(resp.as_bytes());
    let _ = c.write_all(out.as_bytes());
    let _ = c.flush();
}

/// The answer of each kind holding `text` (with token counts 10 in, 5 out).
pub fn ollama(text: &str) -> Value {
    serde_json::json!({ "message": { "role": "assistant", "content": text }, "done_reason": "stop", "prompt_eval_count": 10, "eval_count": 5 })
}

pub fn openai(text: &str) -> Value {
    serde_json::json!({ "choices": [{ "message": { "role": "assistant", "content": text }, "finish_reason": "stop" }], "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
}

pub fn anthropic(text: &str) -> Value {
    serde_json::json!({ "content": [{ "type": "text", "text": text }], "stop_reason": "end_turn", "usage": { "input_tokens": 10, "output_tokens": 5 } })
}

/// The answer of `kind` holding `text`.
pub fn answer(kind: super::Kind, text: &str) -> Value {
    match kind {
        super::Kind::Ollama => ollama(text),
        super::Kind::Openai => openai(text),
        super::Kind::Anthropic => anthropic(text),
    }
}

/// The text of the last user message of a request of any kind.
pub fn prompt_of(r: &Received) -> String {
    r.body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_string()
}
