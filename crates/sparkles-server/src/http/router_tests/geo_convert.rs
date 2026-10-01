//! `POST /$/geo/convert`: geometry literals as CRS84 GeoJSON, one result per literal in
//! request order, errors per item; bad bodies and oversized requests are refused.

use super::*;

fn convert(body: &str) -> Request<Body> {
    Request::post("/$/geo/convert")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn literals_become_geojson_in_order() {
    let s = server();
    let body = serde_json::json!({ "literals": [
        { "value": "POINT(2 3)", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
        { "value": "<http://www.opengis.net/def/crs/EPSG/0/4326> POINT(48.8566 2.3522)",
          "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
        { "value": "POINT(1)", "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
        { "value": "{\"type\":\"Point\",\"coordinates\":[30,30]}",
          "datatype": "http://www.opengis.net/ont/geosparql#geoJSONLiteral" },
        { "value": "<http://example.org/crs/mars> POINT(1 1)",
          "datatype": "http://www.opengis.net/ont/geosparql#wktLiteral" },
    ]});
    let r = send(&s.app, convert(&body.to_string())).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text());
    let j = r.json();
    let results = j["results"].as_array().unwrap();
    assert_eq!(results.len(), 5);
    assert_eq!(
        results[0],
        serde_json::json!({ "geometry": { "type": "Point", "coordinates": [2, 3] } })
    );
    assert_eq!(
        results[1]["geometry"]["coordinates"],
        serde_json::json!([2.3522, 48.8566])
    );
    assert!(
        results[2]["error"]
            .as_str()
            .unwrap()
            .starts_with("malformed literal"),
        "{}",
        results[2]
    );
    assert_eq!(results[3]["geometry"]["type"], "Point");
    assert!(
        results[4]["error"]
            .as_str()
            .unwrap()
            .contains("unknown CRS")
    );
    // an empty request is an empty answer
    let r = send(&s.app, convert(r#"{"literals":[]}"#)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["results"], serde_json::json!([]));
}

#[tokio::test]
async fn bad_requests() {
    let s = server();
    for body in [
        "",
        "{",
        r#"{"literals":[{"value":"POINT(1 1)"}]}"#,
        r#"[1]"#,
    ] {
        let r = send(&s.app, convert(body)).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{body}: {}", r.text());
    }
    let item =
        r#"{"value":"POINT(0 0)","datatype":"http://www.opengis.net/ont/geosparql#wktLiteral"}"#;
    let many = format!(
        r#"{{"literals":[{}]}}"#,
        vec![item; sparkles::geo::convert::MAX_ITEMS + 1].join(",")
    );
    let r = send(&s.app, convert(&many)).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.text().contains("at most 10000"), "{}", r.text());
}
