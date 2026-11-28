//! S3 Object Lock needs headers and versioned deletion beyond ObjectStore's API.
//! Keep those requests on the S3 client's configured transport and credentials.

use super::{ObjectLockMode, S3WritePolicy};
use crate::repo::{RequestOp, RequestStats};
use crate::{BackupError, Code, Result};
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use object_store::aws::{
    AmazonS3, AmazonS3Builder, AmazonS3ConfigKey, AwsAuthorizer, AwsCredential,
};
use object_store::client::{
    CredentialProvider, HttpClient, HttpConnector, HttpError, HttpErrorKind, HttpRequest,
    HttpRequestBody, HttpResponse, HttpService, ReqwestConnector,
};
use object_store::{ClientOptions, GetOptions, PutOptions, PutPayload};
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

type Credentials = dyn CredentialProvider<Credential = AwsCredential>;

#[derive(Debug)]
struct Signing {
    credentials: OnceLock<Weak<Credentials>>,
    region: String,
    request_payer: bool,
    skip_signature: bool,
}

impl Signing {
    async fn authorize(&self, request: &mut HttpRequest, digest: Option<&[u8]>) -> Result<()> {
        if self.request_payer {
            request.headers_mut().insert(
                "x-amz-request-payer",
                "requester".parse().expect("static header"),
            );
        }
        if self.skip_signature {
            return Ok(());
        }
        let credentials = self
            .credentials
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| BackupError::internal("S3 Object Lock credentials are unavailable"))?;
        let credentials = credentials.get_credential().await?;
        request.headers_mut().remove("authorization");
        request.headers_mut().remove("x-amz-security-token");
        AwsAuthorizer::new(&credentials, "s3", &self.region)
            .with_request_payer(self.request_payer)
            .try_authorize(request, digest)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct Connector {
    signing: Arc<Signing>,
}

impl HttpConnector for Connector {
    fn connect(&self, options: &ClientOptions) -> object_store::Result<HttpClient> {
        let network = ReqwestConnector::default().connect(options)?;
        Ok(HttpClient::new(Transport {
            signing: self.signing.clone(),
            network,
        }))
    }
}

#[derive(Debug)]
struct Transport {
    signing: Arc<Signing>,
    network: HttpClient,
}

#[derive(Clone)]
pub(crate) struct WriteHeaders {
    storage_class: Option<&'static str>,
    lock: Option<(&'static str, String, bool)>,
    digest: [u8; 32],
}

impl WriteHeaders {
    pub(crate) fn apply(
        policy: &S3WritePolicy,
        blob: bool,
        until: Option<DateTime<Utc>>,
        payload: &PutPayload,
        options: &mut PutOptions,
    ) {
        let storage_class = policy
            .storage_class
            .filter(|_| blob)
            .map(super::StorageClass::header);
        let lock = policy.object_lock.map(|lock| {
            let until = until.expect("Object Lock deadline from the repository lease");
            (mode_name(lock.mode), timestamp(until), lock.legal_hold)
        });
        if storage_class.is_none() && lock.is_none() {
            return;
        }
        let mut hash = Sha256::new();
        for part in payload {
            hash.update(part);
        }
        options.extensions.insert(Self {
            storage_class,
            lock,
            digest: hash.finalize().into(),
        });
    }
}

#[derive(Clone)]
pub(crate) struct RetentionHead {
    uri: String,
    network: HttpClient,
    until: Option<String>,
    legal_hold: Option<String>,
    mode: Option<String>,
    date: Option<String>,
}

#[derive(Clone)]
struct ReadRetention;

impl RetentionHead {
    pub(crate) fn options() -> GetOptions {
        let mut options = GetOptions {
            head: true,
            ..Default::default()
        };
        options.extensions.insert(ReadRetention);
        options
    }

    pub(crate) fn retained(&self) -> Result<bool> {
        let held = self.legal_hold()?;
        let retained = match self.until()? {
            Some(until) => until >= self.server_time()?,
            None => false,
        };
        Ok(held || retained)
    }

    pub(crate) fn server_time(&self) -> Result<DateTime<Utc>> {
        self.date
            .as_deref()
            .and_then(|date| DateTime::parse_from_rfc2822(date).ok())
            .map(|date| date.with_timezone(&Utc))
            .ok_or_else(|| unavailable("HEAD did not include a valid Date header"))
    }

    pub(crate) fn legal_hold(&self) -> Result<bool> {
        match self.legal_hold.as_deref() {
            Some("ON") => Ok(true),
            Some("OFF") | None => Ok(false),
            _ => Err(unavailable(
                "HEAD returned an invalid Object Lock legal hold",
            )),
        }
    }

    pub(crate) fn mode(&self) -> Result<Option<ObjectLockMode>> {
        match self.mode.as_deref() {
            Some("GOVERNANCE") => Ok(Some(ObjectLockMode::Governance)),
            Some("COMPLIANCE") => Ok(Some(ObjectLockMode::Compliance)),
            None => Ok(None),
            _ => Err(unavailable("HEAD returned an invalid Object Lock mode")),
        }
    }

    pub(crate) fn until(&self) -> Result<Option<DateTime<Utc>>> {
        self.until
            .as_deref()
            .map(|date| {
                DateTime::parse_from_rfc3339(date)
                    .map(|date| date.with_timezone(&Utc))
                    .map_err(|_| unavailable("HEAD returned an invalid Object Lock retention date"))
            })
            .transpose()
    }
}

#[async_trait]
impl HttpService for Transport {
    async fn call(&self, mut request: HttpRequest) -> std::result::Result<HttpResponse, HttpError> {
        if let Some(options) = request.extensions_mut().remove::<WriteHeaders>() {
            let headers = request.headers_mut();
            if let Some(class) = options.storage_class {
                headers.insert("x-amz-storage-class", class.parse().expect("static header"));
            }
            if let Some((mode, until, legal_hold)) = options.lock {
                headers.insert(
                    "x-amz-object-lock-mode",
                    mode.parse().expect("static header"),
                );
                headers.insert(
                    "x-amz-object-lock-retain-until-date",
                    until.parse().expect("RFC 3339 header"),
                );
                if legal_hold {
                    headers.insert(
                        "x-amz-object-lock-legal-hold",
                        "ON".parse().expect("static header"),
                    );
                }
            }
            headers.remove("x-amz-checksum-crc64nvme");
            checksum(&mut request, &options.digest);
            self.signing
                .authorize(&mut request, Some(&options.digest))
                .await
                .map_err(|error| HttpError::new(HttpErrorKind::Unknown, error))?;
        }
        let uri = request
            .extensions()
            .get::<ReadRetention>()
            .map(|_| request.uri().to_string());
        let mut response = self.network.execute(request).await?;
        if let Some(uri) = uri {
            let field = |name| {
                response.headers().get(name).map(|value| {
                    // Invalid headers must cause a refusal rather than silently disappearing.
                    value.to_str().unwrap_or_default().to_string()
                })
            };
            let head = RetentionHead {
                uri,
                network: self.network.clone(),
                until: field("x-amz-object-lock-retain-until-date"),
                legal_hold: field("x-amz-object-lock-legal-hold"),
                mode: field("x-amz-object-lock-mode"),
                date: field("date"),
            };
            response.extensions_mut().insert(head);
        }
        Ok(response)
    }
}

pub(crate) struct Controls {
    signing: Arc<Signing>,
    permits: tokio::sync::Semaphore,
}

impl Controls {
    pub(crate) fn build(builder: AmazonS3Builder, concurrency: usize) -> Result<(AmazonS3, Self)> {
        let enabled = |value: &str| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "true" | "1" | "on" | "yes" | "y"
            )
        };
        let signing = Arc::new(Signing {
            credentials: OnceLock::new(),
            region: builder
                .get_config_value(&AmazonS3ConfigKey::Region)
                .unwrap_or_else(|| "us-east-1".into()),
            request_payer: builder
                .get_config_value(&AmazonS3ConfigKey::RequestPayer)
                .is_some_and(|value| enabled(&value) || value.eq_ignore_ascii_case("requester")),
            skip_signature: builder
                .get_config_value(&AmazonS3ConfigKey::SkipSignature)
                .is_some_and(|value| enabled(&value)),
        });
        let connector = Connector {
            signing: signing.clone(),
        };
        let store = builder.with_http_connector(connector).build()?;
        let _ = signing.credentials.set(Arc::downgrade(store.credentials()));
        Ok((
            store,
            Self {
                signing,
                permits: tokio::sync::Semaphore::new(concurrency),
            },
        ))
    }

    pub(crate) async fn delete_version(
        &self,
        head: &RetentionHead,
        version: &str,
        requests: &RequestStats,
        guard: Option<&crate::lock::LockGuard>,
    ) -> Result<bool> {
        // An unqualified DELETE can hide even a locked version behind a delete marker.
        let status = self
            .version_request(head, version, RequestOp::Delete, None, requests, guard)
            .await?;
        match status.code {
            200 | 204 => Ok(true),
            404 => Ok(false),
            _ => Err(status.error("versioned DELETE")),
        }
    }

    pub(crate) async fn extend_retention(
        &self,
        head: &RetentionHead,
        version: &str,
        mode: ObjectLockMode,
        until: DateTime<Utc>,
        requests: &RequestStats,
    ) -> Result<()> {
        let body = format!(
            "<Retention xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Mode>{}</Mode><RetainUntilDate>{}</RetainUntilDate></Retention>",
            mode_name(mode),
            timestamp(until)
        );
        let status = self
            .version_request(head, version, RequestOp::Put, Some(body), requests, None)
            .await?;
        if status.code != 200 {
            return Err(status.error("PutObjectRetention"));
        }
        Ok(())
    }

    async fn version_request(
        &self,
        head: &RetentionHead,
        version: &str,
        op: RequestOp,
        body: Option<String>,
        requests: &RequestStats,
        guard: Option<&crate::lock::LockGuard>,
    ) -> Result<VersionStatus> {
        const ENCODE: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
            .remove(b'-')
            .remove(b'_')
            .remove(b'.')
            .remove(b'~');
        let version = percent_encoding::utf8_percent_encode(version, ENCODE);
        let uri = format!(
            "{}?{}versionId={version}",
            head.uri,
            if body.is_some() { "retention=&" } else { "" }
        );
        let _permit = self
            .permits
            .acquire()
            .await
            .expect("semaphore is never closed");
        let digest = body.as_ref().map(|body| Sha256::digest(body).into());
        for attempt in 0..3 {
            let mut request =
                HttpRequest::new(body.clone().map_or_else(HttpRequestBody::empty, Into::into));
            *request.method_mut() = op
                .as_str()
                .to_ascii_uppercase()
                .parse()
                .expect("static HTTP method");
            *request.uri_mut() = uri
                .parse()
                .map_err(|_| BackupError::internal("cannot construct a versioned S3 request"))?;
            if let Some(digest) = &digest {
                request.headers_mut().insert(
                    "content-type",
                    "application/xml".parse().expect("static header"),
                );
                checksum(&mut request, digest);
            }
            self.signing
                .authorize(&mut request, digest.as_ref().map(<[u8; 32]>::as_slice))
                .await?;
            if let Some(guard) = guard {
                guard.check_live().await?;
            }
            let response = head.network.execute(request).await;
            let status = response.as_ref().ok().map(|response| response.status());
            requests.record(
                op,
                status.is_some_and(|s| {
                    s.is_success() || (op == RequestOp::Delete && s.as_u16() == 404)
                }),
            );
            let response = match response {
                Ok(response) => Some(VersionStatus::read(response).await),
                Err(_) => None,
            };
            match response {
                Some(VersionStatus {
                    code: 408 | 429 | 500..=599,
                    ..
                })
                | None
                    if attempt < 2 =>
                {
                    tokio::time::sleep(Duration::from_millis(100 << attempt)).await;
                }
                Some(status) => return Ok(status),
                None => return Err(unavailable("could not complete a versioned S3 request")),
            }
        }
        unreachable!("the last attempt returns a result")
    }
}

struct VersionStatus {
    code: u16,
    error_body: Vec<u8>,
}

impl VersionStatus {
    async fn read(response: HttpResponse) -> Self {
        const MAX_BODY: usize = 8 * 1024;
        let status = response.status();
        let mut body = response.into_body().bytes_stream();
        let mut error_body = Vec::new();
        let mut drained = 0;
        while let Some(Ok(chunk)) = body.next().await {
            let take = chunk.len().min(MAX_BODY - drained);
            if !status.is_success() {
                error_body.extend_from_slice(&chunk[..take]);
            }
            drained += take;
            if drained == MAX_BODY {
                break;
            }
        }
        Self {
            code: status.as_u16(),
            error_body,
        }
    }

    fn error(&self, operation: &str) -> BackupError {
        let message = format!(
            "{operation}: Server returned non-2xx status code: {}: {}",
            self.code,
            String::from_utf8_lossy(&self.error_body)
        );
        unavailable(crate::error::redact_bodies(&message))
    }
}

fn mode_name(mode: ObjectLockMode) -> &'static str {
    match mode {
        ObjectLockMode::Governance => "GOVERNANCE",
        ObjectLockMode::Compliance => "COMPLIANCE",
    }
}

fn timestamp(until: DateTime<Utc>) -> String {
    until.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn checksum(request: &mut HttpRequest, digest: &[u8; 32]) {
    let headers = request.headers_mut();
    headers.insert(
        "x-amz-sdk-checksum-algorithm",
        "SHA256".parse().expect("static header"),
    );
    headers.insert(
        "x-amz-checksum-sha256",
        STANDARD.encode(digest).parse().expect("base64 header"),
    );
}

pub(crate) fn unavailable(message: impl std::fmt::Display) -> BackupError {
    BackupError::new(
        Code::RepositoryUnavailable,
        format!("S3 storage controls: {message}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builder() -> AmazonS3Builder {
        AmazonS3Builder::new()
            .with_bucket_name("test-s3")
            .with_access_key_id("test-access")
            .with_secret_access_key("test-secret")
    }

    #[tokio::test]
    async fn controls_respect_requester_pays_and_skip_signature() {
        for value in ["Requester", "true", "1"] {
            let (_store, controls) = Controls::build(
                builder().with_config(AmazonS3ConfigKey::RequestPayer, value),
                1,
            )
            .unwrap();
            let mut request = HttpRequest::new(HttpRequestBody::empty());
            *request.uri_mut() = "https://test-s3.example.test/key?versionId=v1"
                .parse()
                .unwrap();
            controls
                .signing
                .authorize(&mut request, None)
                .await
                .unwrap();
            assert_eq!(request.headers()["x-amz-request-payer"], "requester");
            assert!(
                request.headers()["authorization"]
                    .to_str()
                    .unwrap()
                    .contains("x-amz-request-payer")
            );
        }
        let (store, controls) = Controls::build(
            AmazonS3Builder::new()
                .with_bucket_name("public-s3")
                .with_config(AmazonS3ConfigKey::SkipSignature, "true"),
            1,
        )
        .unwrap();
        drop(store);
        let mut request = HttpRequest::new(HttpRequestBody::empty());
        // Removing the provider makes falling through to credential lookup fail immediately.
        controls
            .signing
            .authorize(&mut request, None)
            .await
            .unwrap();
        assert!(request.headers().get("authorization").is_none());
        assert!(request.headers().get("x-amz-date").is_none());
    }
}
