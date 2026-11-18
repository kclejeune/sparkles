//! How aggregates treat errors and values that are not strings (roadmap item 1064): the
//! dataset's setting at `/$/aggregate-errors/{ds}`, kept in its
//! `aggregate-errors.json`. A query's `aggregate-errors` parameter chooses another
//! reading for that query, and without a setting the dataset follows
//! `serve --aggregate-errors`.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET` needs `read` on `{ds}`,
//! `PUT` and `DELETE` need `admin`.

use super::*;
use sparkles::sparql::AggregateErrors;

/// The setting as `GET /$/aggregate-errors/{ds}` reports it.
pub(crate) fn status_json(ds: &Dataset) -> J {
    serde_json::to_value(ds.dataset.settings().aggregate_errors().status()).unwrap_or_default()
}

async fn get_setting(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(status_json(&ds)))
}

/// `PUT /$/aggregate-errors/{ds}`: give the dataset the reading `{"mode": "spec"}`
/// names. What `GET` reports may be sent back as it is.
async fn put_setting(
    State(st): St,
    Path(name): Path<String>,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let j: J = serde_json::from_slice(&body)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("invalid JSON: {e}")))?;
    let m: AggregateErrors = j
        .get("mode")
        .and_then(J::as_str)
        .ok_or_else(|| err(StatusCode::BAD_REQUEST, "the body needs a mode"))?
        .parse()
        .map_err(|e: sparkles::Error| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    blocking(move || {
        ds.dataset.settings().aggregate_errors().set(m)?;
        tracing::info!("/{}: aggregate errors now read as {m}", ds.name);
        Ok(Json(status_json(&ds)))
    })
    .await
}

/// `DELETE /$/aggregate-errors/{ds}`: back to the server's default.
async fn delete_setting(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        ds.dataset.settings().aggregate_errors().reset()?;
        Ok(Json(status_json(&ds)))
    })
    .await
}

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/$/aggregate-errors/{ds}",
        get(get_setting).put(put_setting).delete(delete_setting),
    )
}

#[cfg(test)]
mod tests {
    use crate::state::{AppState, DbType};
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::StatusCode;
    use serde_json::Value as J;
    use sparkles::store::StoreOptions;
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&body).into_owned())
    }

    fn req(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    /// The `?r` of the group with unbound values, under `params`.
    async fn min(app: &axum::Router, params: &str) -> (StatusCode, String) {
        let q = "SELECT ?g (MIN(?v) AS ?r) WHERE { VALUES (?g ?v) { (1 1) (1 UNDEF) (1 3) } } GROUP BY ?g";
        let uri = format!(
            "/ds/sparql?query={}{params}",
            url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
        );
        let r = Request::builder()
            .uri(uri)
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(app, r).await;
        if status != StatusCode::OK {
            return (status, body);
        }
        let j: J = serde_json::from_str(&body).unwrap();
        let v = j["results"]["bindings"][0]["r"]["value"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (status, v)
    }

    #[tokio::test]
    async fn the_setting_and_the_parameter_choose_the_reading() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(
                dir.path(),
                StoreOptions {
                    aggregate_errors: sparkles::sparql::AggregateErrors::Sparkles,
                    ..Default::default()
                },
                Duration::from_secs(30),
            )
            .unwrap(),
        );
        st.create("ds", DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());
        // the server's default
        let (s, body) = call(&app, req("GET", "/$/aggregate-errors/ds", "")).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        let j: J = serde_json::from_str(&body).unwrap();
        assert_eq!(j["mode"], "sparkles");
        assert_eq!(j["source"], "default");
        assert_eq!(min(&app, "").await, (StatusCode::OK, "1".into()));
        // the dataset's own reading
        let (s, body) = call(
            &app,
            req("PUT", "/$/aggregate-errors/ds", r#"{"mode":"jena"}"#),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(
            serde_json::from_str::<J>(&body).unwrap()["source"],
            "dataset"
        );
        let root = st.get("ds").unwrap().store.root().unwrap().to_path_buf();
        assert!(root.join(sparkles::store::AGGREGATE_ERRORS_FILE).exists());
        assert_eq!(min(&app, "").await, (StatusCode::OK, String::new()));
        // a query's own choice comes first
        assert_eq!(
            min(&app, "&aggregate-errors=sparkles").await,
            (StatusCode::OK, "1".into())
        );
        assert_eq!(
            min(&app, "&aggregate-errors=oxigraph").await.0,
            StatusCode::BAD_REQUEST
        );
        let (s, _) = call(
            &app,
            req("PUT", "/$/aggregate-errors/ds", r#"{"mode":"x"}"#),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        // back to the server's default
        let (s, body) = call(&app, req("DELETE", "/$/aggregate-errors/ds", "")).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(min(&app, "").await, (StatusCode::OK, "1".into()));
    }

    /// What `INSERT … WHERE { SELECT (MIN(?v) …) }` writes under `params`: the minimum,
    /// or nothing when the reading leaves it unbound.
    async fn inserted_min(app: &axum::Router, params: &str) -> (StatusCode, String) {
        let u = "DELETE WHERE { <urn:r> ?p ?o } ; INSERT { <urn:r> <urn:min> ?m } WHERE { \
                 SELECT (MIN(?v) AS ?m) WHERE { VALUES ?v { 3 UNDEF 1 } } }";
        let r = Request::builder()
            .method("POST")
            .uri(format!("/ds/update?{params}"))
            .header("content-type", "application/sparql-update")
            .body(Body::from(u))
            .unwrap();
        let (status, body) = call(app, r).await;
        if status != StatusCode::OK {
            return (status, body);
        }
        let q = "SELECT ?m WHERE { <urn:r> <urn:min> ?m }";
        let r = Request::builder()
            .uri(format!(
                "/ds/sparql?query={}",
                url::form_urlencoded::byte_serialize(q.as_bytes()).collect::<String>()
            ))
            .header("accept", "application/sparql-results+json")
            .body(Body::empty())
            .unwrap();
        let (_, body) = call(app, r).await;
        let j: J = serde_json::from_str(&body).unwrap();
        let v = j["results"]["bindings"][0]["m"]["value"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (status, v)
    }

    #[tokio::test]
    async fn an_update_reads_aggregates_as_the_parameter_or_the_dataset_says() {
        let dir = tempfile::tempdir().unwrap();
        let st = Arc::new(
            AppState::new(dir.path(), StoreOptions::default(), Duration::from_secs(30)).unwrap(),
        );
        st.create("ds", DbType::Persistent).unwrap();
        let app = crate::http::router(st.clone());
        // the server's default, jena, leaves the minimum unbound
        assert_eq!(
            inserted_min(&app, "").await,
            (StatusCode::OK, String::new())
        );
        assert_eq!(
            inserted_min(&app, "aggregate-errors=sparkles").await,
            (StatusCode::OK, "1".into())
        );
        let (s, body) = call(
            &app,
            req("PUT", "/$/aggregate-errors/ds", r#"{"mode":"sparkles"}"#),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(inserted_min(&app, "").await, (StatusCode::OK, "1".into()));
        assert_eq!(
            inserted_min(&app, "aggregate-errors=jena").await,
            (StatusCode::OK, String::new())
        );
        assert_eq!(
            inserted_min(&app, "aggregate-errors=oxigraph").await.0,
            StatusCode::BAD_REQUEST
        );
    }
}
