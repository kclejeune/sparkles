#![cfg(feature = "s3")]

mod common;

use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use common::*;
use sha2::{Digest, Sha256};
use sparkles_backup::{
    Code, Credentials, FileKind, GcOptions, ObjectLock, ObjectLockMode, OpenEnv, RepoConfig,
    Repository, RepositoryOptions, RestoreOptions, S3WritePolicy, StorageClass, layout,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const ROOT: &str = "test-lane";

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
        let clock = Utc::now() + chrono::Duration::hours(self.clock_offset_hours);
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let key = path.strip_prefix("/bucket/").unwrap_or("").to_string();
        let version = query
            .split('&')
            .find_map(|value| value.strip_prefix("versionId="));
        let is_blob = key.starts_with(&format!("{ROOT}/blobs/"));
        let immutable = is_blob || key.starts_with(&format!("{ROOT}/backups/"));
        let checked_body = || {
            let auth = &headers["authorization"];
            assert!(auth.starts_with("AWS4-HMAC-SHA256 "));
            assert!(auth.contains("x-amz-checksum-sha256"));
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
            assert_eq!(version, Some(object.version.as_str()));
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
            return Response::status(200);
        }
        match method {
            "PUT" => {
                if headers.get("if-none-match").is_some_and(|mode| mode == "*")
                    && self.objects.contains_key(&key)
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
                let version = format!("v{}", self.next_version);
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
                response
                    .headers
                    .push(("date", dates.format("%a, %d %b %Y %T GMT").to_string()));
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
                    let object = &self.objects[&key];
                    assert_eq!(version, Some(object.version.as_str()));
                    self.immutable_deletes += 1;
                    if self.retry_delete.as_ref() == Some(&key) {
                        self.retry_delete = None;
                        return Response::status(503);
                    }
                    if !self.expire_retention
                        || self.legal_hold.as_ref() == Some(&key)
                        || self.deny_delete.as_ref() == Some(&key)
                    {
                        return Response::status(403);
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
    let temp = tempfile::tempdir().unwrap();
    let credentials = temp.path().join("credentials.json");
    std::fs::write(
        &credentials,
        r#"{"accessKeyId":"test-access","secretAccessKey":"test-secret"}"#,
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
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
        &OpenEnv::default(),
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
