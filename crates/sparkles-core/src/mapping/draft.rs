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

/// The primary key columns of table `t`, in key order.
fn primary_key(c: &rusqlite::Connection, t: &str) -> rusqlite::Result<Vec<String>> {
    let q = format!("\"{}\"", t.replace('"', "\"\""));
    let mut st = c.prepare(&format!("PRAGMA table_info({q})"))?;
    let mut cols: Vec<(i64, String)> = st
        .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    cols.retain(|(k, _)| *k > 0);
    cols.sort();
    Ok(cols.into_iter().map(|(_, n)| n).collect())
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
        // (key id, position in the key, referenced table, from column, to column)
        let mut rows: Vec<(i64, i64, String, String, Option<String>)> = {
            let mut st = c
                .prepare(&format!("PRAGMA foreign_key_list({q})"))
                .map_err(sql)?;
            st.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(sql)?
            .collect::<std::result::Result<_, _>>()
            .map_err(sql)?
        };
        rows.sort_by_key(|r| (r.0, r.1));
        // one foreign key per id, with its column pairs in order; a reference without
        // target columns names the referenced table's primary key
        let mut fks: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut i = 0;
        while i < rows.len() {
            let id = rows[i].0;
            let rt = rows[i].2.clone();
            let group: Vec<_> = rows[i..].iter().take_while(|r| r.0 == id).collect();
            i += group.len();
            let parent_pk = if group.iter().any(|r| r.4.is_none()) {
                primary_key(&c, &rt).map_err(sql)?
            } else {
                Vec::new()
            };
            let mut pairs = Vec::new();
            for (k, r) in group.iter().enumerate() {
                let to = match &r.4 {
                    Some(t) => t.clone(),
                    None => parent_pk.get(k).cloned().ok_or_else(|| {
                        Error::invalid(format!(
                            "table {t:?}: a foreign key references table {rt:?} without naming columns, and {rt:?} has no primary key of {} columns",
                            group.len()
                        ))
                    })?,
                };
                pairs.push((r.3.clone(), to));
            }
            fks.push((rt, pairs));
        }
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
        for (rt, pairs) in &fks {
            let conditions = pairs
                .iter()
                .map(|(from, to)| {
                    format!(
                        "rr:joinCondition [ rr:child {} ; rr:parent {} ]",
                        turtle_string(&format!("\"{}\"", from.replace('"', "\"\""))),
                        turtle_string(&format!("\"{}\"", to.replace('"', "\"\"")))
                    )
                })
                .collect::<Vec<_>>()
                .join(" ; ");
            let name = pairs
                .iter()
                .map(|(from, _)| iri_part(from))
                .collect::<Vec<_>>()
                .join(";");
            pos.push(format!(
                "    rr:predicateObjectMap [ rr:predicate <{tp}#ref-{name}> ;\n        rr:objectMap [ rr:parentTriplesMap <#{}> ; {conditions} ] ]",
                iri_part(rt),
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
        // a composite key is one join, and a reference without columns names the
        // parent's primary key
        let p2 = dir.path().join("db2.sqlite");
        let c = rusqlite::Connection::open(&p2).unwrap();
        c.execute_batch(
            "CREATE TABLE parent (a INTEGER, b INTEGER, PRIMARY KEY (a, b));
             CREATE TABLE child (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER,
               FOREIGN KEY (a, b) REFERENCES parent (a, b));
             CREATE TABLE team (id INTEGER PRIMARY KEY);
             CREATE TABLE member (id INTEGER PRIMARY KEY, team_id INTEGER REFERENCES team);
             INSERT INTO parent VALUES (1, 1), (1, 2), (2, 1);
             INSERT INTO child VALUES (10, 1, 1);
             INSERT INTO team VALUES (5); INSERT INTO member VALUES (7, 5);",
        )
        .unwrap();
        drop(c);
        let conn2 = Connection::Sqlite { path: p2 };
        let ttl = r2rml(&conn2, &[], "http://ex.org/db/").unwrap();
        let m = super::super::parse(&ttl, super::super::MappingKind::Rml, None).unwrap();
        let inputs = super::super::Inputs {
            connections: vec![super::super::NamedConnection {
                name: "db".into(),
                connection: conn2,
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
        let refs: Vec<&str> = out.lines().filter(|l| l.contains("#ref-a;b")).collect();
        assert_eq!(refs.len(), 1, "{out}\n{ttl}");
        assert!(
            refs[0].contains("<http://ex.org/db/parent/a=1;b=1>"),
            "{out}"
        );
        assert!(
            out.contains("<http://ex.org/db/member/id=7> <http://ex.org/db/member#ref-team_id> <http://ex.org/db/team/id=5> ."),
            "{out}\n{ttl}"
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
