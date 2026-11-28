#![cfg(feature = "s3")]

mod common;

use aws_lc_rs::hmac;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use common::*;
use sha2::{Digest, Sha256};
use sparkles_backup::{
    Code, CopyOptions, Credentials, Ctl, FileKind, GcOptions, ObjectLock, ObjectLockMode, OpenEnv,
    RepoConfig, Repository, RepositoryOptions, RestoreOptions, S3WritePolicy, StorageClass, layout,
};
use sparkles_core::outbound::{OutboundPolicy, Resolver};
use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const ROOT: &str = "test-lane";

// Verify the wire request independently of object_store's authorizer.
fn verify_signature(
    method: &str,
    path: &str,
    query: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
    required: &[&str],
) {
    let auth = headers["authorization"]
        .strip_prefix("AWS4-HMAC-SHA256 Credential=test-access/")
        .unwrap();
    let (scope, rest) = auth.split_once(", SignedHeaders=").unwrap();
    let (signed, signature) = rest.split_once(", Signature=").unwrap();
    let time = &headers["x-amz-date"];
    let day = &time[..8];
    assert_eq!(scope, format!("{day}/us-east-1/s3/aws4_request"));
    assert_eq!(headers["x-amz-security-token"], "test-session-token");
    for required in required.iter().copied().chain([
        "host",
        "x-amz-date",
        "x-amz-content-sha256",
        "x-amz-security-token",
    ]) {
        assert!(signed.split(';').any(|name| name == required), "{required}");
    }
    let payload = sparkles_backup::blob::hex(&Sha256::digest(body));
    assert_eq!(headers["x-amz-content-sha256"], payload);
    let canonical_headers = signed
        .split(';')
        .map(|name| {
            let value = headers[name]
                .split_ascii_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            format!("{name}:{value}\n")
        })
        .collect::<String>();
    let encode = |value: &str| {
        use std::fmt::Write;
        let mut encoded = String::new();
        for byte in percent_encoding::percent_decode_str(value) {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                write!(encoded, "%{byte:02X}").unwrap();
            }
        }
        encoded
    };
    let mut params = query
        .split('&')
        .filter(|param| !param.is_empty())
        .map(|param| {
            let (name, value) = param.split_once('=').unwrap_or((param, ""));
            (encode(name), encode(value))
        })
        .collect::<Vec<_>>();
    params.sort_unstable();
    let canonical_query = params
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical =
        format!("{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed}\n{payload}");
    let hashed = sparkles_backup::blob::hex(&Sha256::digest(canonical));
    let sign = |key: &[u8], message: &str| {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message.as_bytes())
    };
    let mut key = sign(b"AWS4test-secret", day);
    for part in ["us-east-1", "s3", "aws4_request"] {
        key = sign(key.as_ref(), part);
    }
    let to_sign = format!("AWS4-HMAC-SHA256\n{time}\n{scope}\n{hashed}");
    assert_eq!(
        signature,
        sparkles_backup::blob::hex(sign(key.as_ref(), &to_sign).as_ref())
    );
}

#[derive(Default)]
struct TestDns {
    deny: AtomicBool,
}

impl Resolver for TestDns {
    fn resolve(&self, host: &str) -> std::io::Result<Vec<IpAddr>> {
        assert_eq!(host, "s3-controls.test");
        Ok(vec![if self.deny.load(Ordering::SeqCst) {
            Ipv4Addr::new(169, 254, 1, 1).into()
        } else {
            Ipv4Addr::LOCALHOST.into()
        }])
    }
}

struct Stored {
    data: Vec<u8>,
    version: String,
    modified: chrono::DateTime<Utc>,
    headers: HashMap<String, String>,
}

#[derive(Default)]
struct Backend {
    objects: BTreeMap<String, Stored>,
    next_version: u64,
    expire_retention: bool,
    legal_hold: Option<String>,
    deny_delete: Option<String>,
    retry_delete: Option<String>,
    restored: bool,
    clock_offset_hours: i64,
    clock_offset_minutes: i64,
    override_date: Option<String>,
    omit_date: bool,
    block_after_head: Option<(String, usize, Arc<TestDns>)>,
    break_lease_after_head: Option<String>,
    concurrent_retention: Option<String>,
    blob_gets: usize,
    extensions: usize,
    immutable_deletes: usize,
}

#[derive(Default)]
struct Response {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    head: bool,
}

impl Response {
    fn status(status: u16) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }

    fn xml(status: u16, body: String) -> Self {
        Self {
            status,
            body: body.into_bytes(),
            ..Default::default()
        }
    }
}

impl Backend {
    fn respond(
        &mut self,
        method: &str,
        target: &str,
        headers: HashMap<String, String>,
        body: Vec<u8>,
    ) -> Response {
        let clock = Utc::now()
            + chrono::Duration::hours(self.clock_offset_hours)
            + chrono::Duration::minutes(self.clock_offset_minutes);
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let key = path
            .strip_prefix("/bucket/")
            .unwrap_or(&path[1..])
            .to_string();
        let version = query
            .split('&')
            .find_map(|value| value.strip_prefix("versionId="))
            .map(|value| {
                percent_encoding::percent_decode_str(value)
                    .decode_utf8()
                    .unwrap()
            });
        let is_blob = key.starts_with(&format!("{ROOT}/blobs/"));
        let immutable = is_blob || key.starts_with(&format!("{ROOT}/backups/"));
        let checked_body = || {
            let mut required = vec!["x-amz-sdk-checksum-algorithm", "x-amz-checksum-sha256"];
            required.extend(
                [
                    "x-amz-storage-class",
                    "x-amz-object-lock-mode",
                    "x-amz-object-lock-retain-until-date",
                    "x-amz-object-lock-legal-hold",
                    "if-none-match",
                ]
                .into_iter()
                .filter(|name| headers.contains_key(*name)),
            );
            verify_signature(method, path, query, &headers, &body, &required);
            assert_eq!(headers["x-amz-sdk-checksum-algorithm"], "SHA256");
            assert_eq!(
                headers["x-amz-checksum-sha256"],
                STANDARD.encode(Sha256::digest(&body))
            );
            assert!(!headers.contains_key("x-amz-bypass-governance-retention"));
        };
        if method == "PUT" && query.contains("retention=") {
            checked_body();
            let object = self.objects.get_mut(&key).unwrap();
            assert_eq!(version.as_deref(), Some(object.version.as_str()));
            let xml = std::str::from_utf8(&body).unwrap();
            let until = xml
                .split_once("<RetainUntilDate>")
                .unwrap()
                .1
                .split_once("</RetainUntilDate>")
                .unwrap()
                .0;
            let previous = &object.headers["x-amz-object-lock-retain-until-date"];
            assert!(until > previous.as_str());
            object
                .headers
                .insert("x-amz-object-lock-retain-until-date".into(), until.into());
            self.extensions += 1;
            if self.concurrent_retention.as_ref() == Some(&key) {
                self.concurrent_retention = None;
                return Response::xml(403, "<Error><Code>AccessDenied</Code></Error>".into());
            }
            return Response::status(200);
        }
        match method {
            "PUT" => {
                if headers.get("if-none-match").is_some_and(|mode| mode == "*")
                    && self.objects.contains_key(&key)
                {
                    return Response::status(412);
                }
                if let Some(expected) = headers.get("if-match")
                    && self
                        .objects
                        .get(&key)
                        .is_none_or(|object| expected != &format!("\"{}\"", object.version))
                {
                    return Response::status(412);
                }
                if headers.contains_key("x-amz-object-lock-mode")
                    || headers.contains_key("x-amz-storage-class")
                {
                    assert!(immutable);
                    checked_body();
                }
                self.next_version += 1;
                let version = format!("v+{}-/=_~", self.next_version);
                self.objects.insert(
                    key,
                    Stored {
                        data: body,
                        version: version.clone(),
                        modified: clock,
                        headers,
                    },
                );
                let mut response = Response::status(200);
                response.headers.push(("etag", format!("\"{version}\"")));
                response.headers.push(("x-amz-version-id", version));
                response
            }
            "HEAD" | "GET" if query.contains("list-type=2") => {
                let prefix = query
                    .split('&')
                    .find_map(|value| value.strip_prefix("prefix="))
                    .unwrap_or("");
                let prefix = percent_encoding::percent_decode_str(prefix)
                    .decode_utf8()
                    .unwrap();
                let mut result = "<ListBucketResult><IsTruncated>false</IsTruncated>".to_string();
                for (key, object) in &self.objects {
                    if key.starts_with(prefix.as_ref()) {
                        use std::fmt::Write;
                        let modified = object.modified.to_rfc3339();
                        write!(result,
                            "<Contents><Key>{key}</Key><LastModified>{modified}</LastModified><ETag>\"{}\"</ETag><Size>{}</Size><StorageClass>STANDARD</StorageClass></Contents>",
                            object.version, object.data.len(),
                        ).unwrap();
                    }
                }
                result.push_str("</ListBucketResult>");
                Response::xml(200, result)
            }
            "HEAD" | "GET" => {
                if method == "GET" && is_blob {
                    self.blob_gets += 1;
                }
                if method == "HEAD" && self.break_lease_after_head.as_ref() == Some(&key) {
                    self.break_lease_after_head = None;
                    self.objects.retain(|key, _| !key.contains("/locks/"));
                }
                if method == "HEAD"
                    && let Some((blob, remaining, dns)) = &mut self.block_after_head
                    && blob == &key
                {
                    *remaining -= 1;
                    if *remaining == 0 {
                        dns.deny.store(true, Ordering::SeqCst);
                        self.block_after_head = None;
                    }
                }
                let Some(object) = self.objects.get(&key) else {
                    return Response::status(404);
                };
                if method == "GET"
                    && !self.restored
                    && object
                        .headers
                        .get("x-amz-storage-class")
                        .is_some_and(|class| class == "DEEP_ARCHIVE")
                {
                    return Response::xml(
                        403,
                        "<Error><Code>InvalidObjectState</Code></Error>".into(),
                    );
                }
                let mut response = Response::status(200);
                response.head = method == "HEAD";
                response
                    .headers
                    .push(("etag", format!("\"{}\"", object.version)));
                response
                    .headers
                    .push(("x-amz-version-id", object.version.clone()));
                response.headers.push((
                    "last-modified",
                    object.modified.format("%a, %d %b %Y %T GMT").to_string(),
                ));
                let dates = if immutable && self.expire_retention {
                    clock + chrono::Duration::days(30)
                } else {
                    clock
                };
                if !(self.omit_date && immutable) {
                    let date = if immutable {
                        self.override_date.clone()
                    } else {
                        None
                    };
                    response.headers.push((
                        "date",
                        date.unwrap_or_else(|| dates.format("%a, %d %b %Y %T GMT").to_string()),
                    ));
                }
                for field in [
                    "x-amz-object-lock-mode",
                    "x-amz-object-lock-retain-until-date",
                    "x-amz-object-lock-legal-hold",
                ] {
                    if let Some(value) = object.headers.get(field) {
                        response.headers.push((field, value.clone()));
                    }
                }
                if self.legal_hold.as_ref() == Some(&key) {
                    response
                        .headers
                        .push(("x-amz-object-lock-legal-hold", "ON".into()));
                }
                response.body.clone_from(&object.data);
                response
            }
            "POST" if query.starts_with("delete") => {
                let mut response = "<DeleteResult>".to_string();
                for field in std::str::from_utf8(&body).unwrap().split("<Key>").skip(1) {
                    let key = field.split_once("</Key>").unwrap().0;
                    assert!(!key.contains("/blobs/") && !key.contains("/backups/"));
                    self.objects.remove(key);
                    response.push_str(&format!("<Deleted><Key>{key}</Key></Deleted>"));
                }
                response.push_str("</DeleteResult>");
                Response::xml(200, response)
            }
            "DELETE" => {
                if immutable {
                    assert!(!headers.contains_key("x-amz-bypass-governance-retention"));
                    verify_signature(method, path, query, &headers, &body, &[]);
                    let object = &self.objects[&key];
                    assert_eq!(version.as_deref(), Some(object.version.as_str()));
                    self.immutable_deletes += 1;
                    if self.retry_delete.as_ref() == Some(&key) {
                        self.retry_delete = None;
                        return Response::status(503);
                    }
                    if !self.expire_retention
                        || self.legal_hold.as_ref() == Some(&key)
                        || self.deny_delete.as_ref() == Some(&key)
                    {
                        return Response::xml(403, "<Error><Code>AccessDenied</Code><Message>private response</Message></Error>".into());
                    }
                }
                self.objects.remove(&key);
                Response::status(204)
            }
            _ => panic!("unexpected S3 request {method} {target}"),
        }
    }
}

async fn connection(stream: TcpStream, state: Arc<Mutex<Backend>>) {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).await.unwrap() == 0 {
        return;
    }
    let mut request = line.split_ascii_whitespace();
    let method = request.next().unwrap().to_string();
    let target = request.next().unwrap().to_string();
    let mut headers = HashMap::new();
    loop {
        line.clear();
        stream.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    let mut body = vec![
        0;
        headers
            .get("content-length")
            .map_or(0, |n| n.parse().unwrap())
    ];
    stream.read_exact(&mut body).await.unwrap();
    let response = state
        .lock()
        .unwrap()
        .respond(&method, &target, headers, body);
    let mut header = format!(
        "HTTP/1.1 {} test\r\nConnection: close\r\nContent-Length: {}\r\n",
        response.status,
        response.body.len()
    );
    for (name, value) in &response.headers {
        header.push_str(&format!("{name}: {value}\r\n"));
    }
    header.push_str("\r\n");
    stream.write_all(header.as_bytes()).await.unwrap();
    if !response.head && method != "HEAD" {
        stream.write_all(&response.body).await.unwrap();
    }
}

async fn open(policy: S3WritePolicy) -> (tempfile::TempDir, Repository, Arc<Mutex<Backend>>) {
    open_with(policy, None).await
}

async fn open_with(
    policy: S3WritePolicy,
    dns: Option<Arc<TestDns>>,
) -> (tempfile::TempDir, Repository, Arc<Mutex<Backend>>) {
    let temp = tempfile::tempdir().unwrap();
    let credentials = temp.path().join("credentials.json");
    std::fs::write(
        &credentials,
        r#"{"accessKeyId":"test-access","secretAccessKey":"test-secret","sessionToken":"test-session-token"}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = if dns.is_some() {
        format!(
            "http://s3-controls.test:{}",
            listener.local_addr().unwrap().port()
        )
    } else {
        format!("http://{}", listener.local_addr().unwrap())
    };
    let state = Arc::new(Mutex::new(Backend::default()));
    let worker = state.clone();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            tokio::spawn(connection(stream, worker.clone()));
        }
    });
    let cfg = RepoConfig {
        endpoint: Some(endpoint),
        allow_http: true,
        region: Some("us-east-1".into()),
        credentials: Credentials::File {
            path: credentials.display().to_string(),
        },
        ..RepoConfig::from_url("test", "s3://bucket/test-lane").unwrap()
    };
    let repo = Repository::open_with_options(
        &cfg,
        &OpenEnv {
            outbound: dns.map(|resolver| OutboundPolicy {
                allow_private: true,
                resolver,
                ..Default::default()
            }),
            ..Default::default()
        },
        &RepositoryOptions {
            s3_policy: Some(policy),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    (temp, repo, state)
}

#[tokio::test]
async fn protects_shared_blobs_and_reports_retained_denied_and_retried_deletes() {
    let (_temp, repo, state) = open(S3WritePolicy {
        storage_class: Some(StorageClass::StandardIa),
        object_lock: Some(ObjectLock {
            mode: ObjectLockMode::Compliance,
            retention_days: 2,
            legal_hold: false,
        }),
    })
    .await;
    let create = |second: bool| {
        let mut files = vec![
            ("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec()),
            ("prefixes.json", FileKind::Meta, b"{}".to_vec()),
        ];
        if !second {
            for (path, byte) in [
                ("gen-0001/spo.dat", 1),
                ("gen-0001/sop.dat", 2),
                ("gen-0001/pos.dat", 3),
            ] {
                files.push((path, FileKind::Immutable, vec![byte]));
            }
        }
        synthetic(files)
    };
    state.lock().unwrap().clock_offset_hours = -2;
    repo.create(create(false), &opts("first", "ds"))
        .await
        .unwrap();
    let first = repo.manifest("first").await.unwrap();
    let key = |path: &str| {
        let id = &first
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap()
            .blobs[0]
            .id;
        format!("{ROOT}/{}", layout::blob_key(id))
    };
    let shared = key("CURRENT");
    let missing = key("prefixes.json");
    let denied = key("gen-0001/spo.dat");
    let held = key("gen-0001/sop.dat");
    let retried = key("gen-0001/pos.dat");
    {
        let mut store = state.lock().unwrap();
        let blob = store.objects.get_mut(&shared).unwrap();
        let until = chrono::DateTime::parse_from_rfc3339(
            &blob.headers["x-amz-object-lock-retain-until-date"],
        )
        .unwrap();
        assert!(until < Utc::now() + chrono::Duration::hours(47));
        blob.headers.insert(
            "x-amz-object-lock-retain-until-date".into(),
            "2026-01-01T00:00:00Z".into(),
        );
        store.objects.remove(&missing).unwrap();
    }
    let mut second = create(true);
    second.dataset_id = first.dataset.id;
    repo.create(second, &opts("second", "ds")).await.unwrap();
    {
        let store = state.lock().unwrap();
        assert!(store.extensions > 0);
        assert!(store.objects.contains_key(&missing));
    }
    assert_eq!(
        repo.delete("first").await.unwrap_err().code(),
        Code::ObjectRetained
    );
    {
        let mut store = state.lock().unwrap();
        assert_eq!(store.immutable_deletes, 0);
        store.expire_retention = true;
        store.legal_hold = Some(held.clone());
        store.retry_delete = Some(retried.clone());
        store.deny_delete = Some(denied.clone());
    }
    assert!(repo.delete("first").await.unwrap());
    let mut options = GcOptions {
        grace: Duration::ZERO,
        dry_run: true,
        ..Default::default()
    };
    let preview = repo.gc(&options).await.unwrap();
    assert_eq!(preview.candidates, 3);
    assert_eq!(preview.kept_locked, 1);
    assert_eq!(preview.deleted, 2);
    options.dry_run = false;
    let error = repo.gc(&options).await.unwrap_err();
    assert_eq!(error.code(), Code::RepositoryUnavailable);
    assert!(error.message().contains("AccessDenied"));
    assert!(!error.message().contains("private response"));
    let report = repo.last_gc().await.unwrap().unwrap().report;
    assert_eq!(report.deleted, 1);
    assert_eq!(report.kept_locked, 1);
    assert_eq!(report.delete_failures, 1);
    {
        let mut store = state.lock().unwrap();
        assert!(store.objects.contains_key(&shared));
        assert!(store.objects.contains_key(&held));
        assert!(store.objects.contains_key(&denied));
        assert!(!store.objects.contains_key(&retried));
        store.deny_delete = None;
        store.legal_hold = None;
    }
    let retry = repo.gc(&options).await.unwrap();
    assert_eq!(retry.deleted, 2);
    assert_eq!(retry.delete_failures, 0);
    assert!(state.lock().unwrap().objects.contains_key(&shared));
}

#[tokio::test]
async fn archived_blobs_refuse_restore_until_thawed() {
    let (temp, repo, state) = open(S3WritePolicy {
        storage_class: Some(StorageClass::DeepArchive),
        object_lock: None,
    })
    .await;
    let db = temp.path().join("db");
    make_db(&db);
    repo.create(closed_source(&db), &opts("archived", "ds"))
        .await
        .unwrap();
    {
        let store = state.lock().unwrap();
        assert!(
            store
                .objects
                .iter()
                .filter(|(key, _)| key.contains("/blobs/"))
                .all(|(_, blob)| blob
                    .headers
                    .get("x-amz-storage-class")
                    .is_some_and(|class| class == "DEEP_ARCHIVE"))
        );
        assert!(
            !store.objects[&format!("{ROOT}/backups/archived.json")]
                .headers
                .contains_key("x-amz-storage-class")
        );
    }
    let failed = temp.path().join("failed");
    let error = repo
        .restore("archived", &failed, &RestoreOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ArchiveRestoreRequired);
    state.lock().unwrap().restored = true;
    let restored = temp.path().join("restored");
    let report = repo
        .restore("archived", &restored, &RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(report.backup.commit.seq, 3);
}

#[tokio::test]
async fn protected_archive_copies_reuse_cold_blobs_and_extend_retention_at_publish() {
    let dns = Arc::new(TestDns::default());
    let (temp, target, state) = open_with(
        S3WritePolicy {
            storage_class: Some(StorageClass::DeepArchive),
            object_lock: Some(ObjectLock {
                mode: ObjectLockMode::Governance,
                retention_days: 2,
                legal_hold: true,
            }),
        },
        Some(dns),
    )
    .await;
    let db = temp.path().join("db");
    make_db(&db);
    let (source, _) = memory_repo().await;
    source
        .create(closed_source(&db), &opts("first", "ds"))
        .await
        .unwrap();

    let state_for_progress = state.clone();
    let options = CopyOptions {
        ctl: Ctl {
            progress: Some(Arc::new(move |_, message| {
                if message == "copying first" {
                    state_for_progress.lock().unwrap().clock_offset_minutes = 10;
                }
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    let first = source.copy_to(&target, &options).await.unwrap();
    assert_eq!(first.copied, ["first"]);
    assert!(first.uploaded_blobs > 0);
    let manifest = target.manifest("first").await.unwrap();
    let shared = format!(
        "{ROOT}/{}",
        layout::blob_key(&manifest.files[0].blobs[0].id)
    );
    let retention = "x-amz-object-lock-retain-until-date";
    let hold = "x-amz-object-lock-legal-hold";
    let mode = "x-amz-object-lock-mode";
    {
        let mut backend = state.lock().unwrap();
        let publish_until =
            backend.objects[&format!("{ROOT}/backups/first.json")].headers[retention].clone();
        let until = chrono::DateTime::parse_from_rfc3339(&publish_until).unwrap();
        assert!(until > Utc::now() + chrono::Duration::minutes(2 * 24 * 60 + 5));
        let blob = backend.objects.get_mut(&shared).unwrap();
        assert_eq!(blob.headers[hold], "ON");
        assert_eq!(blob.headers[mode], "GOVERNANCE");
        assert_eq!(blob.headers[retention], publish_until);
        blob.headers.remove(hold);
    }

    commit(&db, "INSERT DATA { <urn:c> <urn:p> 3 }");
    source
        .create(closed_source(&db), &opts("second", "ds"))
        .await
        .unwrap();
    let second = CopyOptions {
        names: vec!["second".into()],
        ..Default::default()
    };
    let error = source.copy_to(&target, &second).await.unwrap_err();
    assert!(error.message().contains("required legal hold"));
    assert_eq!(
        target.manifest("second").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    {
        let mut backend = state.lock().unwrap();
        let blob = backend.objects.get_mut(&shared).unwrap();
        blob.headers.insert(hold.into(), "ON".into());
        blob.headers
            .insert(retention.into(), "2026-01-01T00:00:00Z".into());
        backend.concurrent_retention = Some(shared.clone());
    }
    let second = source.copy_to(&target, &second).await.unwrap();
    assert_eq!(second.copied, ["second"]);
    let retry = source
        .copy_to(&target, &CopyOptions::default())
        .await
        .unwrap();
    assert_eq!(retry.already_present, ["second", "first"]);
    assert_eq!(retry.uploaded_blobs, 0);
    {
        let backend = state.lock().unwrap();
        assert_eq!(backend.blob_gets, 0);
        assert!(backend.concurrent_retention.is_none());
        assert!(backend.extensions > 0);
    }

    let (other, _) = memory_repo().await;
    let error = target
        .copy_to(&other, &CopyOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::ArchiveRestoreRequired);
    state.lock().unwrap().restored = true;
    target
        .copy_to(&other, &CopyOptions::default())
        .await
        .unwrap();
    target
        .restore(
            "second",
            &temp.path().join("restore"),
            &RestoreOptions::default(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn rejects_weaker_retention_and_enforces_outbound_dns_on_version_requests() {
    let dns = Arc::new(TestDns::default());
    let (_temp, target, state) = open_with(
        S3WritePolicy {
            storage_class: None,
            object_lock: Some(ObjectLock {
                mode: ObjectLockMode::Compliance,
                retention_days: 2,
                legal_hold: false,
            }),
        },
        Some(dns.clone()),
    )
    .await;
    let (source, _) = memory_repo().await;
    let create = || synthetic(vec![("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec())]);
    source.create(create(), &opts("first", "ds")).await.unwrap();
    source
        .copy_to(&target, &CopyOptions::default())
        .await
        .unwrap();
    let manifest = target.manifest("first").await.unwrap();
    let key = format!(
        "{ROOT}/{}",
        layout::blob_key(&manifest.files[0].blobs[0].id)
    );
    let mode = "x-amz-object-lock-mode";
    let date = "x-amz-object-lock-retain-until-date";
    {
        let mut backend = state.lock().unwrap();
        backend
            .objects
            .get_mut(&key)
            .unwrap()
            .headers
            .insert(mode.into(), "GOVERNANCE".into());
    }
    source
        .create(create(), &opts("second", "ds"))
        .await
        .unwrap();
    let options = CopyOptions {
        names: vec!["second".into()],
        ..Default::default()
    };
    let error = source.copy_to(&target, &options).await.unwrap_err();
    assert!(error.message().contains("weaker Object Lock mode"));
    let before_put_errors = target.requests().put.error;
    let extensions = {
        let mut backend = state.lock().unwrap();
        let blob = backend.objects.get_mut(&key).unwrap();
        blob.headers.insert(mode.into(), "COMPLIANCE".into());
        blob.headers
            .insert(date.into(), "2026-01-01T00:00:00Z".into());
        // Copy checks this blob's size, then reads its body, then inspects retention.
        // Flip DNS only after the last HEAD has already returned to the client.
        backend.block_after_head = Some((key.clone(), 2, dns.clone()));
        backend.extensions
    };
    let error = source.copy_to(&target, &options).await.unwrap_err();
    assert_eq!(error.code(), Code::RepositoryUnavailable);
    assert!(dns.deny.load(Ordering::SeqCst));
    assert_eq!(target.requests().put.error - before_put_errors, 3);
    assert_eq!(state.lock().unwrap().extensions, extensions);
    dns.deny.store(false, Ordering::SeqCst);
    assert_eq!(
        target.manifest("second").await.unwrap_err().code(),
        Code::NoSuchBackup
    );
    assert_eq!(
        source.copy_to(&target, &options).await.unwrap().copied,
        ["second"]
    );
    assert!(state.lock().unwrap().extensions > extensions);
}

#[tokio::test]
async fn requires_valid_server_date_and_keeps_the_expiry_second() {
    let (_temp, repo, state) = open(S3WritePolicy {
        storage_class: None,
        object_lock: Some(ObjectLock {
            mode: ObjectLockMode::Compliance,
            retention_days: 1,
            legal_hold: false,
        }),
    })
    .await;
    repo.create(
        synthetic(vec![("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec())]),
        &opts("first", "ds"),
    )
    .await
    .unwrap();
    {
        let mut backend = state.lock().unwrap();
        let until = &backend.objects[&format!("{ROOT}/backups/first.json")].headers["x-amz-object-lock-retain-until-date"];
        let until = chrono::DateTime::parse_from_rfc3339(until).unwrap();
        backend.override_date = Some(until.format("%a, %d %b %Y %T GMT").to_string());
    }
    assert_eq!(
        repo.delete("first").await.unwrap_err().code(),
        Code::ObjectRetained
    );
    for date in [Some("invalid".into()), None] {
        {
            let mut backend = state.lock().unwrap();
            backend.omit_date = date.is_none();
            backend.override_date = date;
        }
        let error = repo.delete("first").await.unwrap_err();
        assert_eq!(error.code(), Code::RepositoryUnavailable);
        assert!(error.message().contains("valid Date header"));
    }
    assert_eq!(state.lock().unwrap().immutable_deletes, 0);
}

#[tokio::test]
async fn versioned_gc_checks_the_lease_after_candidate_head() {
    let (_temp, repo, state) = open(S3WritePolicy {
        storage_class: None,
        object_lock: Some(ObjectLock {
            mode: ObjectLockMode::Governance,
            retention_days: 1,
            legal_hold: false,
        }),
    })
    .await;
    repo.create(
        synthetic(vec![("CURRENT", FileKind::Meta, b"gen-0001\n".to_vec())]),
        &opts("first", "ds"),
    )
    .await
    .unwrap();
    let manifest = repo.manifest("first").await.unwrap();
    let key = format!(
        "{ROOT}/{}",
        layout::blob_key(&manifest.files[0].blobs[0].id)
    );
    state.lock().unwrap().expire_retention = true;
    assert!(repo.delete("first").await.unwrap());
    state.lock().unwrap().break_lease_after_head = Some(key.clone());
    let error = repo
        .gc(&GcOptions {
            grace: Duration::ZERO,
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::RepositoryLocked);
    let backend = state.lock().unwrap();
    assert_eq!(backend.immutable_deletes, 1);
    assert!(backend.objects.contains_key(&key));
}
