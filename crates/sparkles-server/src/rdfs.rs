//! RDFS on read for a dataset (Fuseki's `--rdfs FILE`, Jena's `ja:DatasetRDFS`): queries
//! match the RDFS closure of each graph with respect to a schema, computed at query time
//! (see [`sparkles::sparql::rdfs`]).
//!
//! The schema is a graph of the dataset, read in the state each query sees, or an RDF
//! document given once, whose closed schema triples are kept. `/$/rdfs/{ds}` reports,
//! sets and removes the setting, and `serve --rdfs NAME=FILE` sets it at startup. A
//! persistent dataset keeps it in `rdfs.json`, with an uploaded schema in
//! `rdfs-schema.nt`.
//!
//! Authorization (the route table in `auth/routes.rs`): `GET` needs `read` on `{ds}`,
//! `PUT` and `DELETE` need `admin`.

use crate::http::{AdminBody, ApiResult, dataset, err};
use crate::state::Dataset;
use anyhow::{Context, Result};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value as J, json};
use sparkles::io::{RdfFormat, Source};
use sparkles::sparql::rdfs::SchemaSource;
use std::sync::Arc;

type St = State<Arc<crate::state::AppState>>;

pub use sparkles::reasoning::rdfs::NewSchema;
#[cfg(test)]
use sparkles::reasoning::rdfs::{SCHEMA_FILE, SETTING_FILE};

/// The triples of an RDF file, its format (and compression) from its name.
pub fn read_file(path: &std::path::Path) -> Result<Vec<oxrdf::Triple>> {
    let (quads, _) = sparkles::io::parse_to_vec(&Source::from_path(path, None)?)?;
    Ok(quads.into_iter().map(oxrdf::Triple::from).collect())
}

/// The triples of an RDF document (the quads of every graph of a quad format).
pub fn parse(bytes: Vec<u8>, format: RdfFormat) -> Result<Vec<oxrdf::Triple>> {
    let (quads, _) = sparkles::io::parse_to_vec(&Source::from_bytes(bytes, format, None))?;
    Ok(quads.into_iter().map(oxrdf::Triple::from).collect())
}

/// Set the dataset's RDFS on read, or with `None` remove it (the library's
/// [`RdfsSetting`](sparkles::handles::RdfsSetting)).
pub fn set(ds: &Dataset, new: Option<NewSchema>) -> Result<()> {
    let setting = ds.dataset.reasoning().rdfs();
    Ok(match new {
        Some(n) => setting.set(n)?,
        None => setting.reset()?,
    })
}

/// The setting for dataset info: `null`, or where the schema comes from.
pub fn info_json(ds: &Dataset) -> J {
    match ds
        .dataset
        .reasoning()
        .rdfs()
        .get()
        .as_deref()
        .map(|r| &r.source)
    {
        None => J::Null,
        Some(SchemaSource::Fixed(_)) => json!({ "source": "upload" }),
        Some(SchemaSource::Graph(g)) => {
            json!({ "source": "graph", "graph": g.as_deref().unwrap_or("default") })
        }
    }
}

/// `GET /$/rdfs/{ds}`: the setting and the schema it gives at the head.
fn status(ds: &Dataset) -> ApiResult<J> {
    let Some(r) = ds.dataset.reasoning().rdfs().get() else {
        return Ok(json!({ "enabled": false }));
    };
    let schema = r.schema(&ds.store.snapshot())?;
    let [classes, properties, domains, ranges] = schema.counts();
    let mut j = info_json(ds);
    j["enabled"] = true.into();
    j["schema"] = json!({
        "classesWithSuperclasses": classes,
        "propertiesWithSuperproperties": properties,
        "propertiesWithDomains": domains,
        "propertiesWithRanges": ranges,
        "skipped": schema.skipped,
    });
    if schema.skipped > 0 {
        j["warnings"] = json!([format!(
            "{} schema triples with a blank node or a literal were left out",
            schema.skipped
        )]);
    }
    Ok(j)
}

async fn get_rdfs(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    Ok(Json(status(&ds)?))
}

/// `PUT /$/rdfs/{ds}`: `{"graph": IRI | "default"}`, or a schema as an RDF document
/// (Turtle, N-Triples, RDF/XML, JSON-LD, TriG, N-Quads by `Content-Type`).
async fn put_rdfs(
    State(st): St,
    Path(name): Path<String>,
    headers: HeaderMap,
    AdminBody(body): AdminBody,
) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let bad = |m: String| err(StatusCode::BAD_REQUEST, m);
    let new = if ct == "application/json" {
        let v: J = serde_json::from_slice(&body).map_err(|e| bad(e.to_string()))?;
        let g = v["graph"]
            .as_str()
            .ok_or_else(|| bad("expected {\"graph\": IRI or \"default\"}".into()))?;
        if g != "default" {
            oxrdf::NamedNode::new(g).map_err(|e| bad(format!("graph: {e}")))?;
        }
        NewSchema::Graph(g.to_string())
    } else {
        let format = sparkles::sparql::results::rdf_format_from_name(&ct).ok_or_else(|| {
            err(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!("unsupported Content-Type '{ct}': send JSON or an RDF document"),
            )
        })?;
        NewSchema::Triples(parse(body.to_vec(), format).map_err(|e| bad(format!("{e:#}")))?)
    };
    set(&ds, Some(new)).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    tracing::info!("/{name}: RDFS on read set");
    Ok(Json(status(&ds)?))
}

/// `DELETE /$/rdfs/{ds}`.
async fn delete_rdfs(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    set(&ds, None).map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(json!({ "enabled": false })))
}

pub fn routes() -> Router<Arc<crate::state::AppState>> {
    Router::new().route(
        "/$/rdfs/{ds}",
        get(get_rdfs).put(put_rdfs).delete(delete_rdfs),
    )
}

/// `serve --rdfs NAME=FILE`: the dataset's schema from a file.
pub fn configure(st: &crate::state::AppState, spec: &str) -> Result<()> {
    let (name, file) = spec
        .split_once('=')
        .with_context(|| format!("--rdfs {spec}: expected NAME=FILE"))?;
    let ds = st
        .datasets
        .read()
        .get(name)
        .cloned()
        .with_context(|| format!("--rdfs {spec}: no dataset /{name}"))?;
    let triples =
        read_file(std::path::Path::new(file)).with_context(|| format!("--rdfs {spec}"))?;
    set(&ds, Some(NewSchema::Triples(triples)))?;
    Ok(())
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

    async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, J) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| J::String(String::from_utf8_lossy(&body).into_owned())),
        )
    }

    fn put(uri: &str, ct: &str, body: &str) -> Request<Body> {
        Request::put(uri)
            .header("content-type", ct)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::get(uri).body(Body::empty()).unwrap()
    }

    const DATA: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:tom a ex:Cat .
ex:ann ex:owns ex:rex .
";
    const SCHEMA: &str = "@prefix ex: <http://ex.org/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
ex:Cat rdfs:subClassOf ex:Animal .
ex:owns rdfs:range ex:Pet .
";
    const ANIMALS: &str =
        "/d/sparql?query=SELECT%20%3Fx%20%7B%20%3Fx%20a%20%3Chttp%3A%2F%2Fex.org%2FAnimal%3E%20%7D";

    fn rows(j: &J) -> usize {
        j["results"]["bindings"].as_array().unwrap().len()
    }

    #[tokio::test]
    async fn rdfs_setting_answers_queries_and_persists() {
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
        let (s, j) = call(&app, get("/$/rdfs/d")).await;
        assert_eq!((s, &j["enabled"]), (StatusCode::OK, &J::Bool(false)));
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 0);

        // an uploaded schema
        let (s, j) = call(&app, put("/$/rdfs/d", "text/turtle", SCHEMA)).await;
        assert_eq!(s, StatusCode::OK, "{j}");
        assert_eq!(j["source"], "upload");
        assert_eq!(j["schema"]["classesWithSuperclasses"], 1);
        assert_eq!(j["schema"]["propertiesWithRanges"], 1);
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 1, "{j}");
        let (_, j) = call(&app, get("/$/datasets/d")).await;
        assert_eq!(j["rdfs"]["source"], "upload", "{j}");
        let root = st.get("d").unwrap().store.root().unwrap().to_path_buf();
        assert!(root.join(super::SETTING_FILE).exists());
        assert!(root.join(super::SCHEMA_FILE).exists());

        // kept across a restart
        drop(app);
        drop(st);
        let st = open();
        let app = crate::http::router(st.clone());
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 1, "{j}");

        // a schema graph, read as it is now
        let (s, j) = call(
            &app,
            put(
                "/$/rdfs/d",
                "application/json",
                r#"{"graph": "http://ex.org/schema"}"#,
            ),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{j}");
        assert_eq!(j["graph"], "http://ex.org/schema");
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 0);
        let update = Request::post("/d/update")
            .header("content-type", "application/sparql-update")
            .body(Body::from(
                "INSERT DATA { GRAPH <http://ex.org/schema> { <http://ex.org/Cat> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <http://ex.org/Animal> } }",
            ))
            .unwrap();
        let (s, _) = call(&app, update).await;
        assert!(s.is_success());
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 1, "{j}");

        // errors, then removal
        let (s, _) = call(
            &app,
            put("/$/rdfs/d", "application/json", r#"{"graph": 1}"#),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let (s, _) = call(&app, put("/$/rdfs/d", "text/x-unknown", "x")).await;
        assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        let req = Request::delete("/$/rdfs/d").body(Body::empty()).unwrap();
        let (s, j) = call(&app, req).await;
        assert_eq!((s, &j["enabled"]), (StatusCode::OK, &J::Bool(false)));
        let (_, j) = call(&app, get(ANIMALS)).await;
        assert_eq!(rows(&j), 0);
        assert!(!root.join(super::SETTING_FILE).exists());
    }
}
