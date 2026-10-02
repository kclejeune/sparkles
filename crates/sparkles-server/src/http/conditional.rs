//! Entity tags and conditional requests on the Graph Store Protocol (RFC 9110 §8.8.3,
//! §13), and the `Sparkles-Commit-Message` request header.
//!
//! A Graph Store representation's tag is `W/"<datasetId>:<seq>:<format>"`: the commit
//! the response was read at, and the serialization (`ttl`, `nt`, `nq`, `trig`, `rdf`,
//! `jsonld`). It is weak because the bytes of one commit's serialization can change
//! without a commit (a compaction reorders the output, a prefix change rewrites Turtle),
//! while the data it describes cannot. The tag is dataset-wide: the catalog does not
//! record which graphs a commit touched, so every commit changes every graph's tag.
//!
//! `If-None-Match` on `GET`/`HEAD` uses the weak comparison and answers `304`.
//! `If-Match` (and `If-None-Match`) on `PUT`/`POST`/`DELETE` are checked with the writer
//! lock held, so no other commit can come between the check and the write; a tag
//! matches when it names the dataset's current head commit, in any serialization.

use super::*;
use sparkles::commit::DatasetId;
use sparkles::guard::Precondition;
use sparkles::store::Snapshot;

/// Request header: a message recorded with the write's commit.
pub(super) const SPARKLES_COMMIT_MESSAGE: &str = "sparkles-commit-message";

/// The serializations a tag can name.
const FORMATS: [&str; 6] = ["ttl", "nt", "nq", "trig", "rdf", "jsonld"];

/// The entity tag of a Graph Store representation read at commit `seq`.
pub(super) fn etag(dataset_id: DatasetId, seq: u64, ext: &str) -> String {
    format!("W/\"{dataset_id}:{seq}:{ext}\"")
}

/// The value of an `If-Match` or `If-None-Match` field.
#[derive(Debug, PartialEq, Eq)]
enum Tags {
    /// `*`
    Any,
    /// the opaque tags, weak or not (unquoted)
    List(Vec<String>),
}

/// Every line of field `name`, combined; `None` when absent. Tags that do not parse are
/// skipped (they match nothing).
fn tags(headers: &HeaderMap, name: header::HeaderName) -> Option<Tags> {
    let mut list = Vec::new();
    let mut any = false;
    let mut seen = false;
    for v in headers.get_all(name) {
        seen = true;
        let Ok(v) = v.to_str() else { continue };
        let mut rest = v.trim();
        while !rest.is_empty() {
            rest = rest.trim_start_matches([',', ' ', '\t']);
            if rest.is_empty() {
                break;
            }
            if let Some(r) = rest.strip_prefix('*') {
                any = true;
                rest = r;
                continue;
            }
            let r = rest.strip_prefix("W/").unwrap_or(rest);
            let Some(r) = r.strip_prefix('"') else {
                // not an entity tag: skip to the next element
                rest = rest.find(',').map_or("", |i| &rest[i..]);
                continue;
            };
            let Some(end) = r.find('"') else { break };
            list.push(r[..end].to_string());
            rest = &r[end + 1..];
        }
    }
    if !seen {
        None
    } else if any {
        Some(Tags::Any)
    } else {
        Some(Tags::List(list))
    }
}

/// Whether an opaque tag names commit `seq` of dataset `id`, in any serialization.
fn names_commit(opaque: &str, id: DatasetId, seq: u64) -> bool {
    let mut parts = opaque.rsplitn(3, ':');
    let (Some(fmt), Some(s), Some(d)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    FORMATS.contains(&fmt) && s.parse() == Ok(seq) && d.parse::<DatasetId>() == Ok(id)
}

fn failed(msg: &str) -> ApiError {
    ApiError(
        StatusCode::PRECONDITION_FAILED,
        json!({ "error": msg, "code": "precondition-failed" }),
    )
}

/// What a conditional `GET`/`HEAD` gets instead of the representation.
pub(super) enum ReadOutcome {
    /// send the representation
    Send,
    /// `304 Not Modified`
    NotModified,
}

/// Evaluate `If-Match` and `If-None-Match` for a read of a representation tagged `tag`
/// at commit `seq` (RFC 9110 §13.2.2). `If-Match` fails with `412`.
pub(super) fn check_read(
    headers: &HeaderMap,
    dataset_id: DatasetId,
    seq: u64,
    tag: &str,
) -> ApiResult<ReadOutcome> {
    if let Some(t) = tags(headers, header::IF_MATCH) {
        let ok = match &t {
            Tags::Any => true,
            Tags::List(l) => l.iter().any(|o| names_commit(o, dataset_id, seq)),
        };
        if !ok {
            return Err(failed(&format!(
                "If-Match: the representation is at commit {seq} now"
            )));
        }
    }
    let opaque = tag.trim_start_matches("W/").trim_matches('"');
    Ok(match tags(headers, header::IF_NONE_MATCH) {
        Some(Tags::Any) => ReadOutcome::NotModified,
        Some(Tags::List(l)) if l.iter().any(|o| o == opaque) => ReadOutcome::NotModified,
        _ => ReadOutcome::Send,
    })
}

/// The `304` of a conditional read: the headers the `200` would have had, no body.
pub(super) fn not_modified(tag: &str) -> Response {
    let mut r = StatusCode::NOT_MODIFIED.into_response();
    if let Ok(v) = header::HeaderValue::from_str(tag) {
        r.headers_mut().insert(header::ETAG, v);
    }
    r
}

/// Add the tag (and `Vary: Accept` when the format was negotiated) to a response.
pub(super) fn with_etag(mut r: Response, tag: &str, negotiated: bool) -> Response {
    let h = r.headers_mut();
    if let Ok(v) = header::HeaderValue::from_str(tag) {
        h.insert(header::ETAG, v);
    }
    if negotiated {
        h.append(header::VARY, header::HeaderValue::from_static("accept"));
    }
    r
}

/// The precondition of a Graph Store write from its `If-Match` and `If-None-Match`
/// fields, checked by the store with the writer lock held. `None` without either field.
pub(super) fn write_precondition(
    headers: &HeaderMap,
    dataset_id: DatasetId,
    target: &Target,
) -> Option<Precondition> {
    let if_match = tags(headers, header::IF_MATCH);
    let if_none = tags(headers, header::IF_NONE_MATCH);
    if if_match.is_none() && if_none.is_none() {
        return None;
    }
    let graph = match target {
        Target::Named(iri) => Some(iri.clone()),
        _ => None,
    };
    Some(Precondition::new(move |head: &Snapshot| {
        // the default graph and the dataset always have a representation
        let exists = match &graph {
            Some(iri) => head
                .lookup_iri(iri)
                .is_some_and(|g| head.count(Perm::Gspo, &[g.0]).unwrap_or(0) > 0),
            None => true,
        };
        let current =
            |l: &[String]| exists && l.iter().any(|o| names_commit(o, dataset_id, head.commit));
        let fail = |m: String| Err(Error::PreconditionFailed(m));
        match &if_match {
            Some(Tags::Any) if !exists => {
                return fail("If-Match: the graph does not exist".into());
            }
            Some(Tags::List(l)) if !current(l) => {
                return fail(format!(
                    "If-Match: the dataset is at commit {} now",
                    head.commit
                ));
            }
            _ => {}
        }
        match &if_none {
            Some(Tags::Any) if exists => fail("If-None-Match: the graph exists".into()),
            Some(Tags::List(l)) if current(l) => fail(format!(
                "If-None-Match: the dataset is still at commit {}",
                head.commit
            )),
            _ => Ok(()),
        }
    }))
}

/// The `Sparkles-Commit-Message` of a write: UTF-8, or an RFC 8187 extended value
/// (`UTF-8''caf%C3%A9`) for clients that can only send ASCII. Checked with
/// [`sparkles::annotations::validate_message`].
pub(super) fn commit_message(headers: &HeaderMap) -> ApiResult<Option<Arc<str>>> {
    let Some(v) = headers.get(SPARKLES_COMMIT_MESSAGE) else {
        return Ok(None);
    };
    let bad = || {
        err(
            StatusCode::BAD_REQUEST,
            "Sparkles-Commit-Message is not valid UTF-8",
        )
    };
    let raw = std::str::from_utf8(v.as_bytes()).map_err(|_| bad())?;
    let text = match raw.get(..7).filter(|p| p.eq_ignore_ascii_case("utf-8''")) {
        Some(_) => percent_encoding::percent_decode_str(&raw[7..])
            .decode_utf8()
            .map_err(|_| bad())?
            .into_owned(),
        None => raw.to_string(),
    };
    Ok(sparkles::annotations::validate_message(&text)?)
}

#[cfg(test)]
#[path = "conditional_tests.rs"]
mod http_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn h(name: header::HeaderName, v: &str) -> HeaderMap {
        let mut m = HeaderMap::new();
        m.append(name, header::HeaderValue::from_str(v).unwrap());
        m
    }

    #[test]
    fn tag_lists_parse() {
        assert_eq!(tags(&HeaderMap::new(), header::IF_MATCH), None);
        assert_eq!(
            tags(&h(header::IF_MATCH, "*"), header::IF_MATCH),
            Some(Tags::Any)
        );
        assert_eq!(
            tags(
                &h(header::IF_MATCH, r#"W/"a:1:ttl", "b", junk, "c""#),
                header::IF_MATCH
            ),
            Some(Tags::List(vec!["a:1:ttl".into(), "b".into(), "c".into()]))
        );
        let mut two = h(header::IF_NONE_MATCH, r#""x""#);
        two.append(
            header::IF_NONE_MATCH,
            header::HeaderValue::from_static(r#"W/"y""#),
        );
        assert_eq!(
            tags(&two, header::IF_NONE_MATCH),
            Some(Tags::List(vec!["x".into(), "y".into()]))
        );
    }

    #[test]
    fn tags_name_a_commit() {
        let id = DatasetId::new_v4();
        let t = etag(id, 42, RdfFormat::Turtle.file_extension());
        assert_eq!(t, format!("W/\"{id}:42:ttl\""));
        let o = t.trim_start_matches("W/").trim_matches('"');
        assert!(names_commit(o, id, 42));
        assert!(!names_commit(o, id, 41));
        assert!(!names_commit(o, DatasetId::new_v4(), 42));
        assert!(!names_commit(&format!("{id}:42:xml"), id, 42));
        assert!(!names_commit("42", id, 42));
    }

    #[test]
    fn commit_messages_decode_and_validate() {
        let m = |v: &[u8]| {
            let mut hm = HeaderMap::new();
            hm.insert(
                SPARKLES_COMMIT_MESSAGE,
                header::HeaderValue::from_bytes(v).unwrap(),
            );
            commit_message(&hm).map_err(|e| e.0)
        };
        assert_eq!(commit_message(&HeaderMap::new()).map_err(|e| e.0), Ok(None));
        assert_eq!(m(b"fix labels").unwrap().as_deref(), Some("fix labels"));
        assert_eq!(m("café".as_bytes()).unwrap().as_deref(), Some("café"));
        assert_eq!(
            m(b"UTF-8''caf%C3%A9%0").unwrap().as_deref(),
            Some("caf\u{e9}%0")
        );
        assert_eq!(m(b"utf-8''a%20b").unwrap().as_deref(), Some("a b"));
        assert_eq!(m(b"UTF-8''a%0Ab").unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(m(b"\xff").unwrap_err(), StatusCode::BAD_REQUEST);
        assert_eq!(m(b"").unwrap(), None);
        let long = "x".repeat(sparkles::annotations::MAX_MESSAGE_BYTES + 1);
        assert_eq!(m(long.as_bytes()).unwrap_err(), StatusCode::BAD_REQUEST);
    }
}
