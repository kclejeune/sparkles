//! Budgets a request asks for (`memory-mb`, `max-result-mb`, `max-rows`,
//! `max-rows-produced`), which can only lower the server's, and the storage quota of a
//! dataset (`/$/quota/{ds}`).

use super::*;

/// Budgets one request asked for. Each one lowers the server's budget of the same kind
/// and never raises it: a larger value is clamped to the server's.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Overrides {
    memory_bytes: Option<u64>,
    result_bytes: Option<u64>,
    rows: Option<u64>,
    rows_produced: Option<u64>,
}

/// The smaller of a server budget and a requested one (`None`: unlimited).
fn lower(server: Option<u64>, asked: Option<u64>) -> Option<u64> {
    match (server, asked) {
        (Some(s), Some(a)) => Some(s.min(a)),
        (s, None) => s,
        (None, a) => a,
    }
}

impl Overrides {
    /// The budget parameters of a request: positive whole numbers, `400` otherwise. A
    /// parameter given more than once counts with its smallest value.
    pub(super) fn parse(params: &Params) -> ApiResult<Overrides> {
        let get = |k: &str| -> ApiResult<Option<u64>> {
            let mut out: Option<u64> = None;
            for v in params.all(k) {
                let n = v
                    .trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        err(
                            StatusCode::BAD_REQUEST,
                            format!("{k} must be a positive whole number, not {v:?}"),
                        )
                    })?;
                out = Some(out.map_or(n, |o| o.min(n)));
            }
            Ok(out)
        };
        let mib = |m: u64| m.saturating_mul(1 << 20);
        Ok(Overrides {
            memory_bytes: get("memory-mb")?.map(mib),
            result_bytes: get("max-result-mb")?.map(mib),
            rows: get("max-rows")?,
            rows_produced: get("max-rows-produced")?,
        })
    }

    /// Lower the engine budgets of `o` to what the request asked for.
    pub(super) fn apply(&self, o: &mut QueryOptions) {
        o.max_memory_bytes = lower(o.max_memory_bytes, self.memory_bytes);
        o.max_rows_produced = lower(o.max_rows_produced, self.rows_produced);
        o.max_rows = lower(o.max_rows.map(|r| r as u64), self.rows)
            .map(|r| usize::try_from(r).unwrap_or(usize::MAX));
    }

    /// The result-size budget of the response.
    pub(super) fn result_limit(&self, server: Option<u64>) -> Option<u64> {
        lower(server, self.result_bytes)
    }
}

// ------------------------------------------------------------ storage quota ------

/// `GET /$/quota/{ds}`: the quota in effect, where it comes from, and the bytes used.
pub(super) async fn get_quota(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    let ds = dataset(&st, &name)?;
    blocking(move || Ok(Json(quota_json(&ds)))).await
}

/// `PUT /$/quota/{ds}` with `{"maxBytes": n}` or `{"maxMb": n}`: the dataset's own
/// quota (`0`: unlimited), kept in its directory.
pub(super) async fn put_quota(
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
    let num = |k: &str| -> ApiResult<Option<u64>> {
        match j.get(k) {
            None => Ok(None),
            Some(v) => v.as_u64().map(Some).ok_or_else(|| {
                err(
                    StatusCode::BAD_REQUEST,
                    format!("{k} must be a whole number of at least 0 (0: unlimited)"),
                )
            }),
        }
    };
    let max = match (num("maxBytes")?, num("maxMb")?) {
        (Some(b), None) => b,
        (None, Some(m)) => m.saturating_mul(1 << 20),
        _ => {
            return Err(err(
                StatusCode::BAD_REQUEST,
                "give one of maxBytes or maxMb (0: unlimited); DELETE removes the dataset's quota",
            ));
        }
    };
    blocking(move || {
        ds.store.set_quota(Some(max))?;
        Ok(Json(quota_json(&ds)))
    })
    .await
}

/// `DELETE /$/quota/{ds}`: remove the dataset's own quota, so the server's
/// `--max-dataset-mb` applies.
pub(super) async fn delete_quota(State(st): St, Path(name): Path<String>) -> ApiResult<Json<J>> {
    if st.read_only {
        return Err(err(StatusCode::FORBIDDEN, "server is read-only"));
    }
    let ds = dataset(&st, &name)?;
    blocking(move || {
        if ds.kind == DbType::Persistent {
            ds.store.set_quota(None)?;
        }
        Ok(Json(quota_json(&ds)))
    })
    .await
}

/// `{dataset, maxBytes, source, defaultMaxBytes, usedBytes}`; `maxBytes` and
/// `defaultMaxBytes` are `null` when unlimited.
pub(super) fn quota_json(ds: &Dataset) -> J {
    let mut j = serde_json::to_value(ds.store.quota()).unwrap_or(J::Null);
    if let Some(o) = j.as_object_mut() {
        o.insert("dataset".into(), ds.name.clone().into());
    }
    j
}
