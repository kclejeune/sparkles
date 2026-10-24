//! `sparkles map draft` (spec F19 §2.1): an R2RML mapping in the shape of the W3C
//! Direct Mapping for the tables of a SQLite database, which a user edits. Each table is
//! a triples map whose subjects are `{base}{table}/{key}={value};…` from its primary
//! key, typed `{base}{table}`, with a predicate `{base}{table}#{column}` per column and
//! a referencing object map per foreign key.

use super::Connection;
use crate::error::{Error, Result};
use rusqlite::OpenFlags;

fn turtle_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn iri_part(s: &str) -> String {
    let mut out = String::new();
    super::template::iri_safe(s, &mut out);
    out
}

/// The R2RML draft of `tables` (all tables when empty) of a SQLite connection.
pub fn r2rml(conn: &Connection, tables: &[String], base: &str) -> Result<String> {
    let Connection::Sqlite { path } = conn else {
        return Err(Error::Unsupported(format!(
            "drafts read SQLite databases; {} connections are not built yet (ROADMAP item 230)",
            conn.kind()
        )));
    };
    let c = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| Error::invalid(format!("SQLite database {}: {e}", path.display())))?;
    let sql =
        |e: rusqlite::Error| Error::invalid(format!("SQLite database {}: {e}", path.display()));
    let mut names: Vec<String> = {
        let mut st = c
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .map_err(sql)?;
        st.query_map([], |r| r.get::<_, String>(0))
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?
    };
    if !tables.is_empty() {
        for t in tables {
            if !names.contains(t) {
                return Err(Error::invalid(format!("the database has no table {t:?}")));
            }
        }
        names.retain(|n| tables.contains(n));
    }
    let mut out = format!("@prefix rr: <http://www.w3.org/ns/r2rml#> .\n@base <{base}> .\n\n");
    for t in &names {
        let q = format!("\"{}\"", t.replace('"', "\"\""));
        // (name, primary key position)
        let cols: Vec<(String, i64)> = {
            let mut st = c.prepare(&format!("PRAGMA table_info({q})")).map_err(sql)?;
            st.query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))
                .map_err(sql)?
                .collect::<std::result::Result<_, _>>()
                .map_err(sql)?
        };
        // (referenced table, from column, to column)
        let fks: Vec<(String, String, Option<String>)> = {
            let mut st = c
                .prepare(&format!("PRAGMA foreign_key_list({q})"))
                .map_err(sql)?;
            st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?
        };
        let mut pk: Vec<(i64, String)> = cols
            .iter()
            .filter(|(_, k)| *k > 0)
            .map(|(n, k)| (*k, n.clone()))
            .collect();
        pk.sort();
        let tp = iri_part(t);
        let subject = if pk.is_empty() {
            "    rr:subjectMap [ rr:termType rr:BlankNode ; rr:template \"".to_string()
                + &cols
                    .iter()
                    .map(|(n, _)| format!("{{{n}}}"))
                    .collect::<Vec<_>>()
                    .join(";")
                + &format!("\" ; rr:class <{tp}> ] ;\n")
        } else {
            let key = pk
                .iter()
                .map(|(_, n)| {
                    format!(
                        "{}={{{}}}",
                        iri_part(n),
                        n.replace('{', "\\{").replace('}', "\\}")
                    )
                })
                .collect::<Vec<_>>()
                .join(";");
            format!(
                "    rr:subjectMap [ rr:template {} ; rr:class <{tp}> ] ;\n",
                turtle_string(&format!("{base}{tp}/{key}"))
            )
        };
        out.push_str(&format!(
            "<#{tp}>\n    rr:logicalTable [ rr:tableName {} ] ;\n",
            turtle_string(&q)
        ));
        out.push_str(&subject);
        let mut pos = Vec::new();
        for (n, _) in &cols {
            pos.push(format!(
                "    rr:predicateObjectMap [ rr:predicate <{tp}#{}> ; rr:objectMap [ rr:column {} ] ]",
                iri_part(n),
                turtle_string(&format!("\"{}\"", n.replace('"', "\"\"")))
            ));
        }
        for (rt, from, to) in &fks {
            let to = to.clone().unwrap_or_else(|| from.clone());
            pos.push(format!(
                "    rr:predicateObjectMap [ rr:predicate <{tp}#ref-{}> ;\n        rr:objectMap [ rr:parentTriplesMap <#{}> ; rr:joinCondition [ rr:child {} ; rr:parent {} ] ] ]",
                iri_part(from),
                iri_part(rt),
                turtle_string(&format!("\"{from}\"")),
                turtle_string(&format!("\"{to}\""))
            ));
        }
        out.push_str(&pos.join(" ;\n"));
        out.push_str(" .\n\n");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_a_mapping_that_runs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("db.sqlite");
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch(
            "CREATE TABLE customer (id INTEGER PRIMARY KEY, name TEXT);
             CREATE TABLE orders (id INTEGER PRIMARY KEY, customer INTEGER REFERENCES customer(id));
             INSERT INTO customer VALUES (1, 'Ann'); INSERT INTO orders VALUES (10, 1);",
        )
        .unwrap();
        drop(c);
        let conn = Connection::Sqlite { path: p };
        let ttl = r2rml(&conn, &[], "http://ex.org/db/").unwrap();
        let m = super::super::parse(&ttl, super::super::MappingKind::Rml, None).unwrap();
        let inputs = super::super::Inputs {
            connections: vec![super::super::NamedConnection {
                name: "db".into(),
                connection: conn,
                jdbc_dsn: None,
            }],
            ..Default::default()
        };
        let mut out = Vec::new();
        super::super::write(
            &m,
            &inputs,
            &Default::default(),
            &mut out,
            oxrdfio::RdfFormat::NQuads,
        )
        .unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("<http://ex.org/db/orders/id=10> <http://ex.org/db/orders#ref-customer> <http://ex.org/db/customer/id=1> ."), "{out}\n{ttl}");
        assert!(
            out.contains(
                "<http://ex.org/db/customer/id=1> <http://ex.org/db/customer#name> \"Ann\" ."
            ),
            "{out}"
        );
        assert!(
            r2rml(
                &super::super::Connection::Postgres {
                    url: "postgres://x".into()
                },
                &[],
                "http://e/"
            )
            .is_err()
        );
    }
}
