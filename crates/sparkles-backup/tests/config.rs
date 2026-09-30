//! Repository URLs and configuration checks.

use sparkles_backup::{Code, Credentials, RepoConfig, RepoType, Sse};
use std::path::PathBuf;

fn url(u: &str) -> RepoConfig {
    RepoConfig::from_url("r", u).unwrap()
}

fn url_err(u: &str) -> Code {
    RepoConfig::from_url("r", u).unwrap_err().code()
}

#[test]
fn urls() {
    let c = url("file:///srv/backups/sparkles");
    assert_eq!(c.kind, RepoType::Fs);
    assert_eq!(c.path.as_deref(), Some("/srv/backups/sparkles"));
    assert!(c.conditional_writes && !c.readonly);
    assert_eq!(
        url("file:///srv/with%20space").path.as_deref(),
        Some("/srv/with space")
    );
    assert_eq!(url("memory://").kind, RepoType::Memory);

    let c = url(
        "s3://kg-backups/prod/sparkles?region=eu-central-1&endpoint=http%3A%2F%2F127.0.0.1%3A9000\
         &path_style=true&allow_http=true&sse=aws:kms&kms_key_id=arn:k&max_concurrency=16\
         &max_upload_bytes_per_sec=1048576&readonly=true&conditional_writes=false",
    );
    assert_eq!(c.kind, RepoType::S3);
    assert_eq!(c.bucket.as_deref(), Some("kg-backups"));
    assert_eq!(c.prefix.as_deref(), Some("prod/sparkles"));
    assert_eq!(c.region.as_deref(), Some("eu-central-1"));
    assert_eq!(c.endpoint.as_deref(), Some("http://127.0.0.1:9000"));
    assert!(c.path_style && c.allow_http && c.readonly && !c.conditional_writes);
    assert_eq!(c.sse, Some(Sse::AwsKms));
    assert_eq!(c.kms_key_id.as_deref(), Some("arn:k"));
    assert_eq!(c.max_concurrency, Some(16));
    assert_eq!(c.max_upload_bytes_per_sec, Some(1 << 20));
    assert_eq!(c.credentials, Credentials::Default);
    c.validate(&[]).unwrap();
    assert_eq!(c.location(), "s3://kg-backups/prod/sparkles");
    let c = url("s3://bucket");
    assert_eq!(c.prefix, None);
    c.validate(&[]).unwrap();
    assert_eq!(url("gs://b/p").kind, RepoType::Gcs);
    assert_eq!(url("az://c/p").kind, RepoType::Azure);

    // secrets never go in URLs
    for u in [
        "s3://AKIA:secret@bucket/p",
        "s3://bucket/p?access_key_id=AKIA",
        "s3://bucket/p?secret_access_key=x",
        "s3://bucket/p?AWS_SESSION_TOKEN=x",
        "file://user@/srv",
    ] {
        let e = RepoConfig::from_url("r", u).unwrap_err();
        assert_eq!(e.code(), Code::InvalidConfig, "{u}");
        assert!(
            !e.message().contains("secret") || !e.message().contains("=x"),
            "{u}"
        );
    }
    for u in [
        "/srv/backups",
        "ftp://host/x",
        "s3:///prefix",
        "s3://b/p?unknown=1",
        "file:///x?region=eu",
        "memory://x",
        "file://otherhost/srv",
        "s3://b?path_style=maybe",
        "s3://b?max_concurrency=many",
    ] {
        assert_eq!(url_err(u), Code::InvalidConfig, "{u}");
    }
}

#[test]
fn configuration_checks() {
    let fs = |p: &str| RepoConfig {
        name: "local".into(),
        kind: RepoType::Fs,
        path: Some(p.into()),
        conditional_writes: true,
        ..Default::default()
    };
    fs("/srv/r").validate(&[]).unwrap();
    let data = [PathBuf::from("/var/lib/sparkles")];
    fs("/var/lib/sparkles-backups").validate(&data).unwrap();
    for bad in [
        "relative/dir",
        "",
        "/var/lib/sparkles",
        "/var/lib/sparkles/backup",
        "/var/lib/other/../sparkles/x",
    ] {
        let e = fs(bad).validate(&data).unwrap_err();
        assert_eq!(e.code(), Code::InvalidConfig, "{bad}");
        assert_eq!(e.http_status(), 400);
        assert_eq!(e.body()["field"], "path", "{bad}");
    }
    // a symbolic link into the data directory
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&data_dir, tmp.path().join("link")).unwrap();
        let e = fs(tmp.path().join("link/repo").to_str().unwrap())
            .validate(std::slice::from_ref(&data_dir))
            .unwrap_err();
        assert_eq!(e.code(), Code::InvalidConfig);
    }

    let mut c = fs("/srv/r");
    c.name = "Bad Name".into();
    assert_eq!(c.validate(&[]).unwrap_err().code(), Code::InvalidName);

    let s3 = || RepoConfig {
        name: "s3".into(),
        kind: RepoType::S3,
        bucket: Some("b".into()),
        conditional_writes: true,
        ..Default::default()
    };
    s3().validate(&[]).unwrap();
    let check = |f: &dyn Fn(&mut RepoConfig), field: &str| {
        let mut c = s3();
        f(&mut c);
        let e = c.validate(&[]).unwrap_err();
        assert_eq!(e.code(), Code::InvalidConfig, "{field}");
        assert_eq!(e.body()["field"], field);
    };
    check(&|c| c.bucket = None, "bucket");
    check(&|c| c.path = Some("/x".into()), "path");
    check(
        &|c| c.endpoint = Some("http://minio:9000".into()),
        "endpoint",
    );
    check(
        &|c| c.endpoint = Some("https://u:p@minio:9000".into()),
        "endpoint",
    );
    check(
        &|c| c.endpoint = Some("https://minio:9000/?x=1".into()),
        "endpoint",
    );
    check(&|c| c.endpoint = Some("minio:9000".into()), "endpoint");
    check(&|c| c.kms_key_id = Some("arn".into()), "kmsKeyId");
    check(&|c| c.max_concurrency = Some(0), "maxConcurrency");
    check(
        &|c| c.max_upload_bytes_per_sec = Some(0),
        "maxUploadBytesPerSec",
    );
    check(&|c| c.prefix = Some("a//b".into()), "prefix");
    check(&|c| c.prefix = Some("a/../b".into()), "prefix");
    check(
        &|c| {
            c.credentials = Credentials::File {
                path: "rel.json".into(),
            }
        },
        "credentials.path",
    );
    let mut ok = s3();
    ok.endpoint = Some("http://minio:9000".into());
    ok.allow_http = true;
    ok.sse = Some(Sse::AwsKms);
    ok.kms_key_id = Some("arn".into());
    ok.prefix = Some("/prod/sparkles/".into());
    ok.validate(&[]).unwrap();
    // fields of other types
    let mut c = fs("/srv/r");
    c.bucket = Some("b".into());
    assert_eq!(c.validate(&[]).unwrap_err().body()["field"], "bucket");
    let mut c = fs("/srv/r");
    c.credentials = Credentials::Env {
        access_key_id_var: "K".into(),
        secret_access_key_var: "S".into(),
        session_token_var: None,
    };
    assert_eq!(c.validate(&[]).unwrap_err().body()["field"], "credentials");
}

/// Credentials files are read when the store is built, and their content never shows
/// in errors.
#[test]
fn credentials_files() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("creds.json");
    std::fs::write(
        &file,
        r#"{"accessKeyId": "AKIA", "secretAccessKey": "hunter2"}"#,
    )
    .unwrap();
    let mut c = RepoConfig {
        name: "s3".into(),
        kind: RepoType::S3,
        bucket: Some("b".into()),
        region: Some("eu-central-1".into()),
        conditional_writes: true,
        credentials: Credentials::File {
            path: file.display().to_string(),
        },
        ..Default::default()
    };
    sparkles_backup::repo::build_store(&c).unwrap();
    std::fs::write(&file, "secret-garbage hunter2").unwrap();
    let e = sparkles_backup::repo::build_store(&c).unwrap_err();
    assert_eq!(e.code(), Code::InvalidConfig);
    assert!(!e.message().contains("hunter2"), "{}", e.message());
    c.credentials = Credentials::Env {
        access_key_id_var: "SPARKLES_TEST_UNSET_KEY_VAR".into(),
        secret_access_key_var: "SPARKLES_TEST_UNSET_SECRET_VAR".into(),
        session_token_var: None,
    };
    let e = sparkles_backup::repo::build_store(&c).unwrap_err();
    assert!(e.message().contains("SPARKLES_TEST_UNSET_KEY_VAR"));
}

/// Named credential sources, and the characters of buckets and regions (they become
/// part of the service's host name).
#[test]
fn named_credentials_and_host_parts() {
    let mut c = RepoConfig {
        name: "s3".into(),
        kind: RepoType::S3,
        bucket: Some("b".into()),
        conditional_writes: true,
        credentials: Credentials::Named { name: "lab".into() },
        ..Default::default()
    };
    c.validate(&[]).unwrap();
    // resolved by the server, never here
    let e = sparkles_backup::repo::build_store(&c).unwrap_err();
    assert_eq!(e.code(), Code::InvalidConfig);
    c.credentials = Credentials::Named {
        name: "Not A Name".into(),
    };
    assert_eq!(c.validate(&[]).unwrap_err().code(), Code::InvalidConfig);
    c.credentials = Credentials::Default;
    for (bucket, region) in [
        ("b/x", None),
        ("b@evil.example", None),
        ("b", Some("x.evil.example")),
        ("b", Some("eu#")),
    ] {
        c.bucket = Some(bucket.into());
        c.region = region.map(Into::into);
        assert_eq!(
            c.validate(&[]).unwrap_err().code(),
            Code::InvalidConfig,
            "{bucket} {region:?}"
        );
    }
}

/// With an outbound policy, endpoints it refuses are refused before any connection.
#[test]
fn endpoints_under_an_outbound_policy() {
    let c = RepoConfig {
        name: "s3".into(),
        kind: RepoType::S3,
        bucket: Some("b".into()),
        endpoint: Some("http://127.0.0.1:9000".into()),
        allow_http: true,
        conditional_writes: true,
        ..Default::default()
    };
    let strict = sparkles::outbound::OutboundPolicy::default();
    let e = sparkles_backup::repo::build_store_with(&c, Some(&strict)).unwrap_err();
    assert_eq!(e.code(), Code::InvalidConfig);
    assert!(e.message().contains("outbound policy"), "{}", e.message());
    let open = sparkles::outbound::OutboundPolicy {
        allow_private: true,
        ..Default::default()
    };
    sparkles_backup::repo::build_store_with(&c, Some(&open)).unwrap();
    // without a policy (config-file repositories), anything goes
    sparkles_backup::repo::build_store(&c).unwrap();
}
