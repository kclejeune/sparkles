//! Downloads from a mock Hub on a local port: listing, verification, refusal of a
//! wrong digest or size, resume with range requests, and the atomic completion.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use sparkles_modelstore::{
    Digest, Error, HttpClient, HttpResponse, HubSource, ModelStore, SnapshotId, sha256_reader,
};

const REV: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
struct Mock {
    /// path → body
    files: Mutex<HashMap<String, Vec<u8>>>,
    /// path → bytes to send before closing the connection, once
    cut_once: Mutex<HashMap<String, usize>>,
    requests: AtomicUsize,
    ranges: AtomicUsize,
}

fn serve(port: u16, mock: Arc<Mock>) -> u16 {
    let l = TcpListener::bind(("127.0.0.1", port))
        .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
        .unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let m = mock.clone();
            std::thread::spawn(move || handle(s, &m));
        }
    });
    port
}

fn handle(s: TcpStream, m: &Mock) {
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    let path = line.split(' ').nth(1).unwrap_or("").to_string();
    let mut range = None;
    loop {
        let mut h = String::new();
        r.read_line(&mut h).unwrap();
        if h == "\r\n" || h.is_empty() {
            break;
        }
        if let Some(v) = h.to_ascii_lowercase().strip_prefix("range: bytes=") {
            range = v.trim().trim_end_matches('-').parse::<usize>().ok();
        }
    }
    m.requests.fetch_add(1, Ordering::SeqCst);
    let mut w = s;
    let body = m.files.lock().unwrap().get(&path).cloned();
    let Some(body) = body else {
        let _ = w
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    };
    let (status, slice) = match range {
        Some(from) => {
            m.ranges.fetch_add(1, Ordering::SeqCst);
            ("206 Partial Content", body[from.min(body.len())..].to_vec())
        }
        None => ("200 OK", body),
    };
    let cut = m.cut_once.lock().unwrap().remove(&path);
    let _ = write!(
        w,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        slice.len()
    );
    let n = cut.unwrap_or(slice.len()).min(slice.len());
    let _ = w.write_all(&slice[..n]);
}

/// A minimal HTTP/1.1 client over a TCP stream, enough for the mock.
struct Plain;

impl HttpClient for Plain {
    fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, String> {
        let rest = url.strip_prefix("http://").ok_or("not http")?;
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).map_err(|e| e.to_string())?;
        let mut req = format!("GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str("\r\n");
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let mut line = String::new();
        r.read_line(&mut line).map_err(|e| e.to_string())?;
        let status: u16 = line.split(' ').nth(1).unwrap().parse().unwrap();
        loop {
            let mut h = String::new();
            r.read_line(&mut h).map_err(|e| e.to_string())?;
            if h == "\r\n" || h.is_empty() {
                break;
            }
        }
        Ok(HttpResponse {
            status,
            body: Box::new(r),
        })
    }
}

fn sha256_hex(b: &[u8]) -> String {
    let d = sha256_reader(&mut &b[..]).unwrap();
    Digest::Sha256(d).to_tagged()[7..].to_string()
}

fn git_hex(b: &[u8]) -> String {
    let d = sparkles_modelstore::git_blob_reader(&mut &b[..], b.len() as u64).unwrap();
    Digest::GitBlobSha1(d).to_tagged()[9..].to_string()
}

/// A mock repo `org/tiny` with a small config (Git blob id) and a larger weights file
/// (LFS SHA-256). `lie` replaces the weights' listed hash.
fn setup(port: u16, lie: bool) -> (Arc<Mock>, HubSource, Vec<u8>) {
    let config = br#"{"model_type":"bert"}"#.to_vec();
    let weights: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let wsha = if lie {
        sha256_hex(b"something else")
    } else {
        sha256_hex(&weights)
    };
    let listing = format!(
        r#"{{"id":"org/tiny","sha":"{REV}","siblings":[
            {{"rfilename":"config.json","size":{},"blobId":"{}"}},
            {{"rfilename":"model.safetensors","size":{},"blobId":"x","lfs":{{"sha256":"{wsha}","size":{},"pointerSize":134}}}},
            {{"rfilename":"onnx/model.onnx","size":3,"blobId":"{}"}}
        ]}}"#,
        config.len(),
        git_hex(&config),
        weights.len(),
        weights.len(),
        git_hex(b"abc"),
    );
    let mock = Arc::new(Mock::default());
    {
        let mut f = mock.files.lock().unwrap();
        f.insert(
            format!("/api/models/org/tiny/revision/{REV}?blobs=true"),
            listing.into_bytes(),
        );
        f.insert(format!("/org/tiny/resolve/{REV}/config.json"), config);
        f.insert(
            format!("/org/tiny/resolve/{REV}/model.safetensors"),
            weights.clone(),
        );
    }
    let port = serve(port, mock.clone());
    let hub = HubSource {
        endpoint: format!("http://127.0.0.1:{port}"),
        token: None,
    };
    (mock, hub, weights)
}

fn select(p: &str) -> bool {
    !p.starts_with("onnx/")
}

#[test]
fn downloads_and_verifies() {
    let (mock, hub, weights) = setup(48001, false);
    let plan = hub.plan(&Plain, "org/tiny", REV, false, &select).unwrap();
    assert_eq!(plan.files.len(), 2);
    let tmp = tempfile::tempdir().unwrap();
    let store = ModelStore::new(tmp.path());
    let mut seen = 0;
    let snap = store
        .fetch(&Plain, &plan, &mut |p| seen = p.files_done)
        .unwrap();
    assert_eq!(seen, 2);
    assert_eq!(
        std::fs::read(snap.path("model.safetensors")).unwrap(),
        weights
    );
    assert!(
        tmp.path()
            .join("org/tiny")
            .join(REV)
            .join("sparkles-manifest.json")
            .is_file()
    );
    assert!(
        !tmp.path()
            .join("org/tiny")
            .join(format!(".{REV}.partial"))
            .exists()
    );
    store.verify(&plan.id).unwrap();
    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, plan.id);
    assert_eq!(listed[0].files.len(), 2);
    assert_eq!(
        listed[0]
            .files
            .iter()
            .find(|f| f.path == "model.safetensors")
            .unwrap()
            .sha256,
        sha256_hex(&weights)
    );
    // a second fetch makes no request
    let before = mock.requests.load(Ordering::SeqCst);
    store.fetch(&Plain, &plan, &mut |_| {}).unwrap();
    assert_eq!(mock.requests.load(Ordering::SeqCst), before);
    // tampering shows in verify
    std::fs::write(snap.path("config.json"), b"{}").unwrap();
    assert!(matches!(
        store.verify(&plan.id),
        Err(Error::DigestMismatch { .. })
    ));
    assert!(store.remove(&plan.id).unwrap());
    assert!(store.list().unwrap().is_empty());
    assert!(!tmp.path().join("org").exists());
    assert!(!store.remove(&plan.id).unwrap());
}

#[test]
fn refuses_a_wrong_hash() {
    let (_mock, hub, _) = setup(48002, true);
    let plan = hub.plan(&Plain, "org/tiny", REV, false, &select).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = ModelStore::new(tmp.path());
    let err = store.fetch(&Plain, &plan, &mut |_| {}).unwrap_err();
    assert!(
        matches!(err, Error::DigestMismatch { ref path, .. } if path == "model.safetensors"),
        "{err}"
    );
    // nothing complete, and the bad part file is gone
    let id = SnapshotId {
        repo: "org/tiny".into(),
        revision: REV.into(),
    };
    assert!(store.get(&id).unwrap().is_none());
    let part = tmp
        .path()
        .join("org/tiny")
        .join(format!(".{REV}.partial"))
        .join("model.safetensors.part");
    assert!(!part.exists());
}

#[test]
fn declared_digest_must_agree() {
    let (_mock, hub, _) = setup(48003, false);
    let mut plan = hub.plan(&Plain, "org/tiny", REV, false, &select).unwrap();
    let mut declared = HashMap::new();
    declared.insert(
        "model.safetensors".to_string(),
        Digest::sha256_hex(&sha256_hex(b"nope")).unwrap(),
    );
    assert!(matches!(
        plan.apply_declared(&declared),
        Err(Error::DigestMismatch { .. })
    ));
    // a declared SHA-256 for a Git-blob file replaces the blob id, and a wrong one fails
    let mut declared = HashMap::new();
    declared.insert(
        "config.json".to_string(),
        Digest::sha256_hex(&sha256_hex(b"nope")).unwrap(),
    );
    plan.apply_declared(&declared).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let err = ModelStore::new(tmp.path())
        .fetch(&Plain, &plan, &mut |_| {})
        .unwrap_err();
    assert!(matches!(err, Error::DigestMismatch { ref path, .. } if path == "config.json"));
}

#[test]
fn resumes_an_interrupted_download() {
    let (mock, hub, weights) = setup(48004, false);
    mock.cut_once
        .lock()
        .unwrap()
        .insert(format!("/org/tiny/resolve/{REV}/model.safetensors"), 70_000);
    let plan = hub.plan(&Plain, "org/tiny", REV, false, &select).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let store = ModelStore::new(tmp.path());
    let err = store.fetch(&Plain, &plan, &mut |_| {}).unwrap_err();
    assert!(
        matches!(err, Error::SizeMismatch { found: 70_000, .. }),
        "{err}"
    );
    assert!(store.get(&plan.id).unwrap().is_none());
    let part = tmp
        .path()
        .join("org/tiny")
        .join(format!(".{REV}.partial"))
        .join("model.safetensors.part");
    assert_eq!(std::fs::metadata(&part).unwrap().len(), 70_000);
    let snap = store.fetch(&Plain, &plan, &mut |_| {}).unwrap();
    assert_eq!(mock.ranges.load(Ordering::SeqCst), 1);
    assert_eq!(
        std::fs::read(snap.path("model.safetensors")).unwrap(),
        weights
    );
}

#[test]
fn unpinned_revisions() {
    let (mock, hub, _) = setup(48005, false);
    let err = hub
        .plan(&Plain, "org/tiny", "main", false, &select)
        .unwrap_err();
    assert!(matches!(err, Error::Unpinned(_)));
    // allowed explicitly, `main` resolves to the listed commit
    let listing = mock.files.lock().unwrap()
        [&format!("/api/models/org/tiny/revision/{REV}?blobs=true")]
        .clone();
    mock.files.lock().unwrap().insert(
        "/api/models/org/tiny/revision/main?blobs=true".into(),
        listing,
    );
    let plan = hub.plan(&Plain, "org/tiny", "main", true, &select).unwrap();
    assert_eq!(plan.id.revision, REV);
    assert!(plan.files[0].url.contains(REV));
}

#[test]
fn a_mismatched_commit_is_refused() {
    let (_mock, hub, _) = setup(48006, false);
    let other = "1111111111111111111111111111111111111111";
    // the mock has no listing for `other`
    assert!(matches!(
        hub.plan(&Plain, "org/tiny", other, false, &select),
        Err(Error::Status { status: 404, .. })
    ));
}

#[test]
fn local_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.json"), b"{}").unwrap();
    let ok = sparkles_modelstore::ExpectedFile {
        path: "config.json".into(),
        digest: Digest::sha256_hex(&sha256_hex(b"{}")),
    };
    sparkles_modelstore::LocalSnapshot::open(tmp.path(), &[ok]).unwrap();
    let bad = sparkles_modelstore::ExpectedFile {
        path: "config.json".into(),
        digest: Digest::sha256_hex(&sha256_hex(b"[]")),
    };
    assert!(sparkles_modelstore::LocalSnapshot::open(tmp.path(), &[bad]).is_err());
    let missing = sparkles_modelstore::ExpectedFile {
        path: "tokenizer.json".into(),
        digest: None,
    };
    assert!(matches!(
        sparkles_modelstore::LocalSnapshot::open(tmp.path(), &[missing]),
        Err(Error::Missing(_))
    ));
}
