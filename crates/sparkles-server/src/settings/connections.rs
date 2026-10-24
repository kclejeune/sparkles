//! The server-wide `connections` settings kind (spec F19 §4.4): the named database
//! connections that stored mappings and uploaded mappings read, declared in the settings
//! file's `server.connections` and kept at runtime in `<dataDir>/connections.json`.
//!
//! The kind holds one member, `connections`, an object from connection names to their
//! descriptions, so that it merges and locks as the channels of `notifications` do. This
//! build reads SQLite files. A PostgreSQL or DuckDB connection is refused with the
//! roadmap item that adds it, so that a configuration never silently names a database
//! the server cannot reach.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sparkles::mapping::{Connection, NamedConnection};
use std::collections::BTreeMap;

/// The runtime layer, in the data directory.
pub const CONNECTIONS_FILE: &str = "connections.json";

/// The kind's top-level members.
pub const MEMBERS: &[&str] = &["connections"];

/// The effective `connections` object as its type.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSettings {
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionDecl>,
}

/// One connection.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ConnectionDecl {
    /// `sqlite` (`postgres` and `duckdb` are ROADMAP item 230)
    pub kind: String,
    /// a SQLite file, as an absolute path
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// the `d2rq:jdbcDSN` of existing RML mappings that names this connection
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jdbc_dsn: Option<String>,
}

pub fn defaults() -> Value {
    json!({ "connections": {} })
}

pub fn normalize(v: &Value) -> Result<Value, String> {
    parse(v).map(|_| v.clone())
}

pub fn check(v: &Value, _: super::Providers) -> Result<(), String> {
    parse(v).map(|_| ())
}

/// A connection name: letters, digits, `.`, `_` and `-`, as in
/// `urn:x-sparkles:connection:NAME`.
fn check_name(n: &str) -> Result<(), String> {
    if n.is_empty()
        || n.len() > 64
        || !n
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(format!(
            "connections: {n:?} is not a connection name (letters, digits, '.', '_' and '-', at most 64)"
        ));
    }
    Ok(())
}

/// The object as its type, with each connection checked.
pub fn parse(v: &Value) -> Result<ConnectionSettings, String> {
    let s: ConnectionSettings = serde_json::from_value(v.clone()).map_err(|e| e.to_string())?;
    for (name, c) in &s.connections {
        check_name(name)?;
        match c.kind.as_str() {
            "sqlite" => {
                let Some(p) = &c.path else {
                    return Err(format!(
                        "connections.{name}: a sqlite connection needs path, the database file"
                    ));
                };
                if !std::path::Path::new(p).is_absolute() {
                    return Err(format!(
                        "connections.{name}.path: an absolute path, since the server's working directory is not the operator's"
                    ));
                }
            }
            "postgres" | "duckdb" => {
                return Err(format!(
                    "connections.{name}: {} connections are not built yet (ROADMAP item 230); this build reads sqlite",
                    c.kind
                ));
            }
            k => {
                return Err(format!(
                    "connections.{name}.kind: {k:?} is not a connection kind; use sqlite"
                ));
            }
        }
    }
    Ok(s)
}

/// The members of `server.locked` under `connections`: the member or one connection.
pub fn check_lock(p: &[String]) -> Result<(), String> {
    match p {
        [m] if m == "connections" => Ok(()),
        [m, name] if m == "connections" => check_name(name),
        _ => Err(
            "connections is locked whole or by connection, such as connections.connections.orders"
                .into(),
        ),
    }
}

impl ConnectionSettings {
    /// The connections in the mapping engine's form.
    pub fn named(&self) -> Vec<NamedConnection> {
        self.connections
            .iter()
            .filter_map(|(name, c)| {
                let connection = match c.kind.as_str() {
                    "sqlite" => Connection::Sqlite {
                        path: c.path.clone()?.into(),
                    },
                    _ => return None,
                };
                Some(NamedConnection {
                    name: name.clone(),
                    connection,
                    jdbc_dsn: c.jdbc_dsn.clone(),
                })
            })
            .collect()
    }
}

/// The effective connections of a server, none when the settings do not read.
pub fn effective(st: &crate::state::AppState) -> Vec<NamedConnection> {
    let r = super::server::resolved_kind(st, &super::CONNECTIONS);
    match parse(&r.effective) {
        Ok(s) => s.named(),
        Err(e) => {
            tracing::warn!(target: "sparkles::mapping", "connection settings: {e}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connections_are_checked() {
        let ok = json!({"connections": {"shop": {"kind": "sqlite", "path": "/data/shop.sqlite", "jdbcDsn": "jdbc:sqlite:shop"}}});
        let s = parse(&ok).unwrap();
        let n = s.named();
        assert_eq!(n[0].name, "shop");
        assert_eq!(n[0].jdbc_dsn.as_deref(), Some("jdbc:sqlite:shop"));
        let e = parse(&json!({"connections": {"shop": {"kind": "sqlite", "path": "rel.sqlite"}}}))
            .unwrap_err();
        assert!(e.contains("absolute"), "{e}");
        let e =
            parse(&json!({"connections": {"pg": {"kind": "postgres", "path": "/x"}}})).unwrap_err();
        assert!(e.contains("230"), "{e}");
        let e =
            parse(&json!({"connections": {"a b": {"kind": "sqlite", "path": "/x"}}})).unwrap_err();
        assert!(e.contains("connection name"), "{e}");
        assert!(
            parse(
                &json!({"connections": {"x": {"kind": "sqlite", "path": "/x", "password": "p"}}})
            )
            .is_err()
        );
        assert!(check_lock(&["connections".into(), "shop".into()]).is_ok());
        assert!(check_lock(&["other".into()]).is_err());
    }
}
