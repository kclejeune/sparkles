//! How DESCRIBE describes a resource (spec G06 Phase 2): the dataset's setting at
//! `/$/describe/{ds}`, kept in its `describe.json`, and the request parameters that
//! choose another mode or lower the limits for one query.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET` needs `read` on `{ds}`,
//! `PUT` and `DELETE` need `admin`.

use super::*;
use sparkles::sparql::describe::{DescribeMode, DescribeOptions};

/// Response header: the DESCRIBE result stopped at its `maxTriples` limit.
pub(super) const SPARKLES_DESCRIBE_TRUNCATED: &str = "sparkles-describe-truncated";

/// The setting as `GET /$/describe/{ds}` reports it.
pub(crate) fn status_json(ds: &Dataset) -> J {
    crate::describe_cmd::status(&ds.store.describe_settings())
}

async fn get_setting(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(status_json(&ds)))
}

/// `PUT /$/describe/{ds}`: replace the setting with the JSON object's options. Options it
/// leaves out take their defaults.
async fn put_setting(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let mut j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    // what GET reports may be sent back as it is
    if let Some(o) = j.as_object_mut() {
        o.remove("source");
        o.remove("modes");
    }
    let s =
        DescribeOptions::from_json(&j).map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    blocking(move || {
        ds.store.set_describe_settings(Some(s))?;
        tracing::info!("/{}: DESCRIBE setting changed", ds.name);
        Ok(Json(status_json(&ds)))
    })
    .await
}

/// `DELETE /$/describe/{ds}`: back to the defaults.
async fn delete_setting(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        ds.store.set_describe_settings(None)?;
        Ok(Json(status_json(&ds)))
    })
    .await
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/$/describe/{ds}",
        get(get_setting).put(put_setting).delete(delete_setting),
    )
}

/// The DESCRIBE options of a query request: the dataset's setting, with the request's
/// `describe` (a mode), `describe-labels`, `describe-reifiers`, `describe-max-triples`
/// and `describe-max-depth` over it. The limits of a request can only lower the
/// dataset's. A malformed value is a `400`.
pub(super) fn request_options(ds: &Dataset, params: &Params) -> ApiResult<DescribeOptions> {
    let mut o = ds.store.describe_settings();
    let bad = |e: sparkles::Error| err(StatusCode::BAD_REQUEST, e.to_string());
    if let Some(m) = params.get("describe") {
        o.mode = DescribeMode::parse(m).map_err(bad)?;
    }
    for (param, key) in [
        ("describe-labels", "labels"),
        ("describe-reifiers", "reifiers"),
    ] {
        if let Some(v) = params.get(param) {
            o.set(key, v).map_err(bad)?;
        }
    }
    let mut asked = DescribeOptions::default();
    for (param, key) in [
        ("describe-max-triples", "maxTriples"),
        ("describe-max-depth", "maxDepth"),
    ] {
        if let Some(v) = params.get(param) {
            if v.trim().parse::<u64>().ok().is_none_or(|n| n == 0) {
                return Err(err(
                    StatusCode::BAD_REQUEST,
                    format!("{param} must be a positive whole number"),
                ));
            }
            asked.set(key, v).map_err(bad)?;
        }
    }
    Ok(o.lowered(asked.max_triples, asked.max_depth))
}

#[cfg(test)]
mod tests {
    use crate::state::{AppState, DbType};
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{HeaderMap, StatusCode};
    use serde_json::Value as J;
    use sparkles::store::StoreOptions;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    const DATA: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:a ex:p ex:b ; ex:q [ ex:r 1 ] .
ex:b rdfs:label \"B\" .
ex:z ex:link ex:a .
";

    /// The status, the headers and the body of a response.
    async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, String) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }

    fn json(body: &str) -> J {
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"))
    }

    fn put(body: &str) -> Request<Body> {
        Request::put("/$/describe/d")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// `DESCRIBE ex:a` as N-Triples, with extra query parameters: its lines, sorted.
    async fn describe(app: &axum::Router, params: &str) -> (StatusCode, HeaderMap, Vec<String>) {
        let q = "DESCRIBE%20%3Chttp%3A%2F%2Fex.org%2Fa%3E";
        let req = Request::get(format!("/d/sparql?query={q}{params}"))
            .header("accept", "application/n-triples")
            .body(Body::empty())
            .unwrap();
        let (s, h, body) = call(app, req).await;
        let mut lines: Vec<String> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect();
        lines.sort();
        (s, h, lines)
    }

    #[tokio::test]
    async fn the_setting_and_the_request_choose_the_description() {
        let dir = tempfile::tempdir().unwrap();
        let open = || {
            Arc::new(
                AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30))
                    .unwrap(),
            )
        };
        let st = open();
        st.create("d", DbType::Persistent).unwrap();
        st.get("d")
            .unwrap()
            .store
            .load(&[sparkles::io::Source::from_bytes(
                DATA.as_bytes().to_vec(),
                sparkles::io::RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let app = crate::http::router(st.clone());
        let get = || Request::get("/$/describe/d").body(Body::empty()).unwrap();
        let (s, _, body) = call(&app, get()).await;
        let j = json(&body);
        assert_eq!(s, StatusCode::OK);
        assert_eq!(
            (j["mode"].as_str(), j["source"].as_str()),
            (Some("cbd"), Some("default"))
        );
        // the default: the resource's triples and its blank node's
        let (_, _, lines) = describe(&app, "").await;
        assert_eq!(lines.len(), 3, "{lines:?}");

        // the dataset's setting
        let (s, _, body) = call(&app, put(r#"{"mode": "scbd", "labels": true}"#)).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let j = json(&body);
        assert_eq!(
            (j["mode"].as_str(), j["source"].as_str()),
            (Some("scbd"), Some("dataset"))
        );
        let (_, _, lines) = describe(&app, "").await;
        assert_eq!(lines.len(), 5, "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("ex.org/link")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("rdf-schema#label")),
            "{lines:?}"
        );
        let root = st.get("d").unwrap().store.root().unwrap().to_path_buf();
        assert!(root.join(sparkles::store::DESCRIBE_FILE).exists());
        // what GET reports can be sent back as it is
        let (s, _, _) = call(&app, put(&body)).await;
        assert_eq!(s, StatusCode::OK);

        // the service description lists it
        let req = Request::get("/d/sparql")
            .header("accept", "application/n-triples")
            .body(Body::empty())
            .unwrap();
        let (_, _, sd) = call(&app, req).await;
        assert!(
            sd.contains("<urn:x-sparkles:describeMode> \"scbd\""),
            "{sd}"
        );
        assert!(
            sd.contains("<urn:x-sparkles:describeLabels> \"true\""),
            "{sd}"
        );

        // kept across a restart
        drop(app);
        drop(st);
        let st = open();
        let app = crate::http::router(st.clone());
        let (_, _, lines) = describe(&app, "").await;
        assert_eq!(lines.len(), 5, "{lines:?}");

        // a request's mode and limits
        let (_, _, lines) = describe(&app, "&describe=outgoing&describe-labels=false").await;
        assert_eq!(lines.len(), 2, "{lines:?}");
        let (s, h, lines) = describe(&app, "&describe-max-triples=1").await;
        assert_eq!((s, lines.len()), (StatusCode::OK, 1));
        assert_eq!(h[super::SPARKLES_DESCRIBE_TRUNCATED], "true");
        let (_, h, _) = describe(&app, "").await;
        assert!(!h.contains_key(super::SPARKLES_DESCRIBE_TRUNCATED));
        for bad in [
            "&describe=everything",
            "&describe-labels=maybe",
            "&describe-max-triples=0",
            "&describe-max-depth=x",
        ] {
            let (s, _, _) = describe(&app, bad).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        }

        // a malformed setting, then removal
        for bad in [r#"{"mode": "all"}"#, r#"{"depth": 2}"#, "[]", "{"] {
            let (s, _, _) = call(&app, put(bad)).await;
            assert_eq!(s, StatusCode::BAD_REQUEST, "{bad}");
        }
        let req = Request::delete("/$/describe/d")
            .body(Body::empty())
            .unwrap();
        let (s, _, body) = call(&app, req).await;
        assert_eq!(
            (s, json(&body)["source"].as_str()),
            (StatusCode::OK, Some("default"))
        );
        assert!(!root.join(sparkles::store::DESCRIBE_FILE).exists());
        let (_, _, lines) = describe(&app, "").await;
        assert_eq!(lines.len(), 3, "{lines:?}");
    }
}
