//! SQLite sources (spec F19 §4.4, §5.2), behind the `mapping-sqlite` feature. A logical
//! table is read as a derived table projected to the referenced columns, with a stepped
//! statement, so memory stays flat. The file is opened read-only with `query_only` on,
//! and the run's deadline and cancellation interrupt a statement through SQLite's
//! progress handler. A join whose two sides are on one connection runs in SQLite as one
//! statement (§5.3).

use super::{Cell, Dt, Row, RowSource, canonical_decimal, canonical_double, canonical_integer};
use crate::error::{Error, Result};
use crate::mapping::model::SqlQuery;
use crate::mapping::{Connection, RunOptions};
use rusqlite::OpenFlags;
use rusqlite::types::ValueRef;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Open a connection's file read-only.
fn connect(path: &Path, opts: &RunOptions) -> Result<rusqlite::Connection> {
    let c = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| Error::invalid(format!("SQLite database {}: {e}", path.display())))?;
    c.execute_batch("PRAGMA query_only = 1")
        .map_err(|e| Error::invalid(format!("SQLite database {}: {e}", path.display())))?;
    if let Some(check) = opts.check.clone() {
        // a deadline or a cancellation interrupts a long statement
        c.progress_handler(100_000, Some(move || check().is_err()))
            .map_err(|e| Error::invalid(format!("SQLite database {}: {e}", path.display())))?;
    }
    Ok(c)
}

fn sqlite_path(conn: &Connection) -> Result<&PathBuf> {
    match conn {
        Connection::Sqlite { path } => Ok(path),
        other => Err(Error::Unsupported(format!(
            "{} connections are not built yet; SQLite is (ROADMAP item 230)",
            other.kind()
        ))),
    }
}

/// The derived table of a logical table.
pub(crate) fn derived(q: &SqlQuery) -> String {
    match q {
        SqlQuery::Table(t) => format!("SELECT * FROM {t}"),
        SqlQuery::Query(q) => q.trim().trim_end_matches(';').to_string(),
    }
}

fn quote(id: &str) -> String {
    format!("\"{}\"", id.replace('"', "\"\""))
}

/// The columns of a derived table, by preparing it. The query itself is prepared, not
/// a `SELECT *` over it, because SQLite renames the duplicate columns of a subquery
/// (`ID:1`), and a duplicate name makes a reference ambiguous.
fn columns(c: &rusqlite::Connection, inner: &str) -> Result<Vec<String>> {
    let st = c
        .prepare(inner)
        .map_err(|e| Error::invalid(format!("SQL {inner:?}: {e}")))?;
    Ok(st.column_names().into_iter().map(str::to_string).collect())
}

/// The column a reference names: an R2RML column name, which may be a delimited
/// identifier (`"Name"`), matched exactly first and then without case.
fn resolve(cols: &[String], r: &str, what: &str) -> Result<String> {
    let delimited = r.len() >= 2 && r.starts_with('"') && r.ends_with('"');
    let want = if delimited {
        r[1..r.len() - 1].replace("\"\"", "\"")
    } else {
        r.to_string()
    };
    let ambiguous = || {
        Err(Error::invalid(format!(
            "{what} has more than one column named {r:?}, so the reference is ambiguous"
        )))
    };
    let exact: Vec<&String> = cols.iter().filter(|c| **c == want).collect();
    match exact.as_slice() {
        [c] => return Ok((*c).clone()),
        [] => {}
        _ => return ambiguous(),
    }
    if !delimited {
        let folded: Vec<&String> = cols
            .iter()
            .filter(|c| c.eq_ignore_ascii_case(&want))
            .collect();
        match folded.as_slice() {
            [c] => return Ok((*c).clone()),
            [] => {}
            _ => return ambiguous(),
        }
    }
    Err(Error::invalid(format!(
        "{what} has no column {r:?}; its columns are {}",
        cols.iter()
            .map(|c| format!("{c:?}"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

pub(crate) fn open(
    conn: &Connection,
    query: &SqlQuery,
    refs: &[String],
    opts: &RunOptions,
) -> Result<Box<dyn RowSource>> {
    let path = sqlite_path(conn)?.clone();
    let c = connect(&path, opts)?;
    let inner = derived(query);
    let what = match query {
        SqlQuery::Table(t) => format!("table {t}"),
        SqlQuery::Query(q) => format!("query {q:?}"),
    };
    let cols = columns(&c, &inner)?;
    let mut select = Vec::new();
    for r in refs {
        select.push(quote(&resolve(&cols, r, &what)?));
    }
    if select.is_empty() {
        select.push("1".into());
    }
    let sql = format!("SELECT {} FROM ({inner}) AS m0", select.join(", "));
    Ok(Box::new(SqlSource {
        conn: c,
        sql,
        names: refs.to_vec(),
        opts: opts.clone(),
    }))
}

/// A join of two logical tables on one connection, as one statement: the child's
/// references first, then the parent's. NULL never joins.
pub(crate) fn open_join(
    conn: &Connection,
    child: &SqlQuery,
    parent: &SqlQuery,
    child_refs: &[String],
    parent_refs: &[String],
    conditions: &[(String, String)],
    opts: &RunOptions,
) -> Result<(Box<dyn RowSource>, String)> {
    let path = sqlite_path(conn)?.clone();
    let c = connect(&path, opts)?;
    let (ci, pi) = (derived(child), derived(parent));
    let (cc, pc) = (columns(&c, &ci)?, columns(&c, &pi)?);
    let mut select = Vec::new();
    for r in child_refs {
        select.push(format!("c.{}", quote(&resolve(&cc, r, "the child table")?)));
    }
    for r in parent_refs {
        select.push(format!(
            "p.{}",
            quote(&resolve(&pc, r, "the parent table")?)
        ));
    }
    if select.is_empty() {
        select.push("1".into());
    }
    let mut on = Vec::new();
    for (a, b) in conditions {
        on.push(format!(
            "c.{} = p.{}",
            quote(&resolve(&cc, a, "the child table")?),
            quote(&resolve(&pc, b, "the parent table")?)
        ));
    }
    let sql = format!(
        "SELECT {} FROM ({ci}) AS c JOIN ({pi}) AS p ON {}",
        select.join(", "),
        if on.is_empty() {
            "1 = 1".to_string()
        } else {
            on.join(" AND ")
        }
    );
    let names = child_refs.iter().chain(parent_refs).cloned().collect();
    Ok((
        Box::new(SqlSource {
            conn: c,
            sql: sql.clone(),
            names,
            opts: opts.clone(),
        }),
        sql,
    ))
}

struct SqlSource {
    conn: rusqlite::Connection,
    sql: String,
    names: Vec<String>,
    opts: RunOptions,
}

struct SqlRow<'r, 's> {
    row: &'r rusqlite::Row<'s>,
    types: &'r [Option<Dt>],
    names: &'r [String],
}

/// The natural datatype of a declared SQL type (R2RML §10.2).
fn natural(decl: &str) -> Option<Dt> {
    let d = decl.to_ascii_uppercase();
    let d = d.split('(').next().unwrap_or(&d).trim();
    Some(if d.contains("INT") {
        Dt::Integer
    } else if d.starts_with("BOOL") {
        Dt::Boolean
    } else if d.starts_with("DEC") || d.starts_with("NUMERIC") {
        Dt::Decimal
    } else if d.contains("REAL") || d.contains("FLOA") || d.contains("DOUB") {
        Dt::Double
    } else if d == "DATE" {
        Dt::Date
    } else if d.starts_with("TIMESTAMP") || d.starts_with("DATETIME") {
        Dt::DateTime
    } else if d.starts_with("TIME") {
        Dt::Time
    } else if d.contains("BLOB") || d.contains("BINARY") || d == "BYTEA" {
        Dt::HexBinary
    } else if d.contains("CHAR") || d.contains("TEXT") || d.contains("CLOB") {
        Dt::Str
    } else {
        return None;
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn cell<'a>(v: ValueRef<'a>, dt: Option<&Dt>) -> Option<Cell<'a>> {
    let text = |t: &'a [u8]| String::from_utf8_lossy(t);
    Some(match (v, dt) {
        (ValueRef::Null, _) => return None,
        (ValueRef::Integer(i), Some(Dt::Boolean)) => Cell {
            lex: Cow::Borrowed(if i != 0 { "true" } else { "false" }),
            dt: Dt::Boolean,
        },
        (ValueRef::Integer(i), Some(Dt::Decimal)) => Cell {
            lex: Cow::Owned(format!("{i}.0")),
            dt: Dt::Decimal,
        },
        (ValueRef::Integer(i), Some(Dt::Double)) => Cell {
            lex: Cow::Owned(canonical_double(i as f64)),
            dt: Dt::Double,
        },
        (ValueRef::Integer(i), Some(Dt::Str)) => Cell {
            lex: Cow::Owned(i.to_string()),
            dt: Dt::Str,
        },
        (ValueRef::Integer(i), _) => Cell {
            lex: Cow::Owned(i.to_string()),
            dt: Dt::Integer,
        },
        (ValueRef::Real(f), Some(Dt::Decimal)) => Cell {
            lex: Cow::Owned(canonical_decimal(&format!("{f}")).unwrap_or_else(|| f.to_string())),
            dt: Dt::Decimal,
        },
        (ValueRef::Real(f), Some(Dt::Str)) => Cell {
            lex: Cow::Owned(f.to_string()),
            dt: Dt::Str,
        },
        (ValueRef::Real(f), _) => Cell {
            lex: Cow::Owned(canonical_double(f)),
            dt: Dt::Double,
        },
        (ValueRef::Text(t), Some(Dt::Integer)) => {
            let s = text(t);
            match canonical_integer(&s) {
                Some(c) => Cell {
                    lex: Cow::Owned(c),
                    dt: Dt::Integer,
                },
                None => Cell {
                    lex: s,
                    dt: Dt::Str,
                },
            }
        }
        (ValueRef::Text(t), Some(Dt::Decimal)) => {
            let s = text(t);
            match canonical_decimal(&s) {
                Some(c) => Cell {
                    lex: Cow::Owned(c),
                    dt: Dt::Decimal,
                },
                None => Cell {
                    lex: s,
                    dt: Dt::Str,
                },
            }
        }
        (ValueRef::Text(t), Some(Dt::Double)) => {
            let s = text(t);
            match s.trim().parse::<f64>() {
                Ok(f) => Cell {
                    lex: Cow::Owned(canonical_double(f)),
                    dt: Dt::Double,
                },
                Err(_) => Cell {
                    lex: s,
                    dt: Dt::Str,
                },
            }
        }
        (ValueRef::Text(t), Some(Dt::Boolean)) => {
            let s = text(t);
            match s.trim().to_ascii_lowercase().as_str() {
                "true" | "1" | "t" => Cell {
                    lex: Cow::Borrowed("true"),
                    dt: Dt::Boolean,
                },
                "false" | "0" | "f" => Cell {
                    lex: Cow::Borrowed("false"),
                    dt: Dt::Boolean,
                },
                _ => Cell {
                    lex: s,
                    dt: Dt::Str,
                },
            }
        }
        (ValueRef::Text(t), Some(Dt::DateTime)) => {
            let s = text(t);
            Cell {
                lex: Cow::Owned(s.trim().replacen(' ', "T", 1)),
                dt: Dt::DateTime,
            }
        }
        (ValueRef::Text(t), Some(d @ (Dt::Date | Dt::Time))) => Cell {
            lex: text(t),
            dt: d.clone(),
        },
        (ValueRef::Text(t), Some(Dt::HexBinary)) => Cell {
            lex: Cow::Owned(hex(t)),
            dt: Dt::HexBinary,
        },
        (ValueRef::Text(t), _) => Cell {
            lex: text(t),
            dt: Dt::Str,
        },
        (ValueRef::Blob(b), _) => Cell {
            lex: Cow::Owned(hex(b)),
            dt: Dt::HexBinary,
        },
    })
}

impl Row for SqlRow<'_, '_> {
    fn get<'a>(&'a self, r: usize, out: &mut Vec<Cell<'a>>) -> Result<()> {
        let v = self
            .row
            .get_ref(r)
            .map_err(|e| Error::invalid(format!("SQL column {}: {e}", r + 1)))?;
        if let Some(c) = cell(v, self.types[r].as_ref()) {
            out.push(c);
        }
        Ok(())
    }

    fn position(&self) -> String {
        let mut parts = Vec::new();
        for (i, n) in self.names.iter().enumerate().take(3) {
            let v = match self.row.get_ref(i) {
                Ok(ValueRef::Null) => "NULL".to_string(),
                Ok(ValueRef::Integer(x)) => x.to_string(),
                Ok(ValueRef::Real(x)) => x.to_string(),
                Ok(ValueRef::Text(t)) => format!("'{}'", String::from_utf8_lossy(t)),
                Ok(ValueRef::Blob(b)) => format!("X'{}'", hex(b)),
                Err(_) => "?".into(),
            };
            parts.push(format!("{n} = {v}"));
        }
        format!("the row with {}", parts.join(", "))
    }
}

impl RowSource for SqlSource {
    fn scan(&mut self, f: &mut dyn FnMut(&dyn Row) -> Result<()>) -> Result<()> {
        let sql_err = |e: rusqlite::Error, sql: &str| -> Error {
            if matches!(
                &e,
                rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::OperationInterrupted
            ) {
                return Error::Cancelled;
            }
            Error::invalid(format!("SQL {sql:?}: {e}"))
        };
        let mut st = self
            .conn
            .prepare(&self.sql)
            .map_err(|e| sql_err(e, &self.sql))?;
        let types: Vec<Option<Dt>> = st
            .columns()
            .iter()
            .map(|c| c.decl_type().and_then(natural))
            .collect();
        let mut rows = st.query([]).map_err(|e| sql_err(e, &self.sql))?;
        let mut n = 0u64;
        loop {
            let row = match rows.next() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                Err(e) => {
                    // an interrupt from the progress handler: report the run's own reason
                    if let Some(c) = &self.opts.check {
                        c()?;
                    }
                    return Err(sql_err(e, &self.sql));
                }
            };
            n += 1;
            if n.is_multiple_of(65_536)
                && let Some(c) = &self.opts.check
            {
                c()?;
            }
            f(&SqlRow {
                row,
                types: &types,
                names: &self.names,
            })?;
        }
        Ok(())
    }
}
