//! The spatial index's files: written after builds, read back at open, rebuilt when
//! damaged or made for something else, reused across compactions, and never part of
//! backups or clones.

use super::tests::{EX, FIXTURE, GEO, NEAR, WORLD, opts, rng, unindexed, update, window};
use crate::geo::GeoConfig;
use crate::geo::persist::{self, Header};
use crate::io::RdfFormat;
use crate::sparql::plan::GraphFilter;
use crate::store::Store;
use crate::store::{Snapshot, StoreOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const P: &str = "PREFIX ex: <http://example.org/> \
    PREFIX geo: <http://www.opengis.net/ont/geosparql#> \
    PREFIX geof: <http://www.opengis.net/def/function/geosparql/> \
    PREFIX spatial: <http://jena.apache.org/spatial#> \
    PREFIX uom: <http://www.opengis.net/def/uom/OGC/1.0/> ";

/// Queries whose answers must not change with how the base came to be.
const QUERIES: [&str; 4] = [
    "SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfWithin(?w, \"POLYGON((0 0, 10 0, 10 10, 0 10, 0 0))\"^^geo:wktLiteral)) }",
    "SELECT ?g { ?g geo:asWKT ?w FILTER(geof:sfIntersects(?w, \"POLYGON((9 -1, 30 -1, 30 31, 9 31, 9 -1))\"^^geo:wktLiteral)) }",
    "SELECT ?f { ?f spatial:nearby (2 2 500 uom:kilometre) }",
    "SELECT ?f ?d { ?f geo:hasGeometry/geo:asWKT ?w BIND(geof:distance(?w, \"POINT(0 0)\"^^geo:wktLiteral, uom:metre) AS ?d) } ORDER BY ?d",
];

/// The answers of [`QUERIES`], each sorted.
fn answers(ds: &Store) -> Vec<Vec<String>> {
    QUERIES
        .iter()
        .map(|q| {
            let r = ds
                .query(&format!("{P}{q}"))
                .unwrap_or_else(|e| panic!("{q}: {e}"));
            let mut v: Vec<String> = r.rows().iter().map(|row| format!("{row:?}")).collect();
            v.sort();
            v
        })
        .collect()
}

/// The fixture in the base of a persistent store at `dir`, with the index enabled.
fn persistent(dir: &Path, cfg: GeoConfig) -> Store {
    let ds = Store::open(dir, opts()).unwrap();
    ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
    ds.compact().unwrap();
    let s = ds.enable_geo(cfg).unwrap();
    assert_eq!(s.state, "ready");
    ds
}

fn geo_dir(snap: &Snapshot) -> PathBuf {
    persist::dir_of(snap.generation.dir.as_ref().unwrap())
}

/// The base of the current view.
fn base(ds: &Store) -> Arc<crate::geo::index::GeoBase> {
    ds.snapshot()
        .geo
        .as_ref()
        .and_then(|v| v.usable().cloned())
        .expect("a ready index")
}

fn reopen(dir: &Path) -> Store {
    let ds = Store::open(dir, opts()).unwrap();
    let s = ds.wait_geo().unwrap();
    assert_eq!(s.state, "ready", "{s:?}");
    ds
}

#[test]
fn files_are_written_and_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    let gdir = geo_dir(&ds.snapshot());
    for f in [persist::RTREE_FILE, persist::COLUMN_FILE] {
        assert!(gdir.join(f).exists(), "{f}");
    }
    assert!(!gdir.join("rtree.tmp").exists() && !gdir.join("column.tmp").exists());
    let s = ds.geo_status().unwrap();
    let files = s.files.unwrap();
    assert!(!files.opened && files.bytes > 0);
    assert_eq!(s.memory.mapped_bytes, files.bytes);
    // the base built here is read in place already
    let b = base(&ds);
    assert_eq!((b.column.parsed, b.column.reused), (10, 0));
    assert!(b.tree.as_ref().unwrap().is_mapped());
    let expected = answers(&ds);
    let rows = window(ds.snapshot(), WORLD, GraphFilter::All).0;
    assert_eq!(rows.len(), 7);
    drop(ds);

    // reopened: nothing is parsed, the answers are the same
    let ds = reopen(dir.path());
    let s = ds.geo_status().unwrap();
    assert!(s.files.unwrap().opened);
    assert_eq!(s.literals, 7);
    assert_eq!(
        (
            s.skipped.malformed,
            s.skipped.unknown_crs,
            s.skipped.empty,
            s.crs.len()
        ),
        (1, 1, 1, 3)
    );
    assert_eq!(base(&ds).column.parsed, 0);
    assert_eq!(answers(&ds), expected);
    let (again, st, _) = window(ds.snapshot(), WORLD, GraphFilter::All);
    assert!(!st.fallback);
    assert_eq!(again, rows);
    // and as a scan finds them
    assert_eq!(
        window(unindexed(&ds.snapshot()), NEAR, GraphFilter::All).0,
        window(ds.snapshot(), NEAR, GraphFilter::All).0
    );
    // a change of distance model keeps the files; a change of predicates does not
    let s = ds
        .enable_geo(GeoConfig {
            distance: crate::geo::DistanceModel::Haversine,
            ..GeoConfig::default()
        })
        .unwrap();
    assert!(s.files.unwrap().opened);
    let s = ds
        .enable_geo(GeoConfig {
            predicates: vec![format!("{GEO}asWKT")],
            ..GeoConfig::default()
        })
        .unwrap();
    assert!(!s.files.unwrap().opened);
    assert_eq!(s.rows.base, 6);
    drop(ds);
    let ds = reopen(dir.path());
    let s = ds.geo_status().unwrap();
    assert!(s.files.unwrap().opened);
    assert_eq!(s.rows.base, 6);
    // disabling removes them
    ds.disable_geo().unwrap();
    assert!(!gdir.exists());
}

/// Rewrite the header of `path` through `f` (with a valid checksum).
fn rewrite_header(path: &Path, f: impl FnOnce(&mut Header)) {
    let mut b = std::fs::read(path).unwrap();
    let mut h = Header::decode(&b).unwrap();
    f(&mut h);
    b[..persist::HEADER_BYTES].copy_from_slice(&h.encode());
    std::fs::write(path, b).unwrap();
}

#[test]
fn damaged_files_are_rebuilt() {
    type Damage = Box<dyn Fn(&Path)>;
    let flip = |at: fn(u64) -> u64| -> Damage {
        Box::new(move |p: &Path| {
            let mut b = std::fs::read(p).unwrap();
            let i = at(b.len() as u64) as usize;
            b[i] ^= 0x40;
            std::fs::write(p, b).unwrap();
        })
    };
    let cases: Vec<(&str, &str, Damage)> = vec![
        (
            "missing",
            persist::RTREE_FILE,
            Box::new(|p: &Path| std::fs::remove_file(p).unwrap()),
        ),
        (
            "empty",
            persist::COLUMN_FILE,
            Box::new(|p: &Path| std::fs::write(p, b"").unwrap()),
        ),
        (
            "truncated",
            persist::RTREE_FILE,
            Box::new(|p: &Path| {
                let b = std::fs::read(p).unwrap();
                std::fs::write(p, &b[..b.len() - 9]).unwrap();
            }),
        ),
        (
            "extended",
            persist::COLUMN_FILE,
            Box::new(|p: &Path| {
                let mut b = std::fs::read(p).unwrap();
                b.extend_from_slice(&[0; 8]);
                std::fs::write(p, b).unwrap();
            }),
        ),
        ("header", persist::RTREE_FILE, flip(|_| 20)),
        ("footer", persist::COLUMN_FILE, flip(|n| n - 20)),
        ("index", persist::RTREE_FILE, flip(|_| 70)),
        ("column index", persist::COLUMN_FILE, flip(|_| 100)),
        // the last byte of the tree, and a coordinate of a geometry record
        ("tree data", persist::RTREE_FILE, flip(|n| n - 33)),
        ("record data", persist::COLUMN_FILE, flip(|n| n - 40)),
        (
            "other configuration",
            persist::COLUMN_FILE,
            Box::new(|p: &Path| rewrite_header(p, |h| h.config_hash ^= 1)),
        ),
        (
            "other generation",
            persist::RTREE_FILE,
            Box::new(|p: &Path| rewrite_header(p, |h| h.base_seq += 1)),
        ),
        (
            "other quads",
            persist::COLUMN_FILE,
            Box::new(|p: &Path| rewrite_header(p, |h| h.quads += 1)),
        ),
        (
            "other version",
            persist::RTREE_FILE,
            Box::new(|p: &Path| rewrite_header(p, |h| h.version += 1)),
        ),
    ];
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    let expected = answers(&ds);
    let gdir = geo_dir(&ds.snapshot());
    drop(ds);
    for (what, file, damage) in cases {
        damage(&gdir.join(file));
        // `sparkles check` notices
        let report = crate::check::check(dir.path(), &Default::default()).unwrap();
        let geo = report.checks.iter().find(|c| c.name == "geo").unwrap();
        assert!(
            geo.issues
                .iter()
                .any(|i| i.message.contains("rebuilt on open")),
            "{what}: {geo:?}"
        );
        let ds = reopen(dir.path());
        let s = ds.geo_status().unwrap();
        assert!(!s.files.unwrap().opened, "{what}: the files were used");
        assert_eq!(answers(&ds), expected, "{what}");
        drop(ds);
        // the rebuilt files are good again
        let report = crate::check::check(dir.path(), &Default::default()).unwrap();
        let geo = report.checks.iter().find(|c| c.name == "geo").unwrap();
        assert!(geo.issues.is_empty(), "{what}: {geo:?}");
        let ds = reopen(dir.path());
        assert!(ds.geo_status().unwrap().files.unwrap().opened, "{what}");
        drop(ds);
    }
}

#[test]
fn compaction_parses_only_new_literals() {
    for persistent_store in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        // loads of more than a quad are bulk commits (a new generation)
        let o = StoreOptions {
            bulk_threshold: 1,
            ..opts()
        };
        let ds = if persistent_store {
            Store::open(dir.path(), o).unwrap()
        } else {
            crate::store::Store::in_memory(o)
        };
        let mut seed = 3;
        let mut q = String::new();
        for i in 0..500 {
            let (x, y) = (rng(&mut seed) * 40.0, rng(&mut seed) * 40.0);
            q += &format!("<{EX}r{i}> <{GEO}asWKT> \"POINT({x} {y})\"^^<{GEO}wktLiteral> .\n");
        }
        ds.load_str(&q, RdfFormat::NTriples).unwrap();
        ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
        ds.compact().unwrap();
        ds.enable_geo(GeoConfig::default()).unwrap();
        assert_eq!(base(&ds).column.parsed, 510);
        let old = ds.snapshot().generation.dir.clone();
        update(
            &ds,
            "INSERT",
            r#"ex:n1 geo:asWKT "POINT(1 1)"^^geo:wktLiteral . ex:n2 geo:asWKT "POINT(1.5 1.5)"^^geo:wktLiteral .
            ex:n3 geo:asWKT "LINESTRING(0 0, 3 3)"^^geo:wktLiteral"#,
        );
        update(
            &ds,
            "DELETE",
            r#"ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral"#,
        );
        let before = window(ds.snapshot(), NEAR, GraphFilter::All).0;
        let expected = answers(&ds);
        ds.compact().unwrap();
        // POINT(2 2) is gone; the commits parsed the three new literals already
        let b = base(&ds);
        assert_eq!(
            (b.column.parsed, b.column.reused),
            (0, 512),
            "{persistent_store}"
        );
        assert_eq!(answers(&ds), expected);
        assert_eq!(window(ds.snapshot(), NEAR, GraphFilter::All).0, before);
        // a bulk load of two new literals (and one known) parses the two
        ds.load_str(
            &format!(
                "<{EX}b1> <{GEO}asWKT> \"POINT(2.1 2.1)\"^^<{GEO}wktLiteral> .\n\
                 <{EX}b2> <{GEO}asWKT> \"POINT(2.2 2.2)\"^^<{GEO}wktLiteral> .\n\
                 <{EX}b3> <{GEO}asWKT> \"POINT(1 1)\"^^<{GEO}wktLiteral> .\n"
            ),
            RdfFormat::NTriples,
        )
        .unwrap();
        let b = base(&ds);
        assert_eq!(
            (b.column.parsed, b.column.reused),
            (2, 512),
            "{persistent_store}"
        );
        let mut more = before.clone();
        more.extend(["b1", "b2", "b3"].map(String::from));
        assert_eq!(window(ds.snapshot(), NEAR, GraphFilter::All).0, more);
        let expected = answers(&ds);
        assert_eq!(
            window(ds.snapshot(), WORLD, GraphFilter::All).0,
            window(unindexed(&ds.snapshot()), WORLD, GraphFilter::All).0
        );
        if persistent_store {
            // the old generation and its files are gone, the new one has its own
            assert!(!old.unwrap().exists());
            assert!(geo_dir(&ds.snapshot()).join(persist::RTREE_FILE).exists());
            assert!(b.tree.as_ref().unwrap().is_mapped());
            drop((b, ds));
            let ds = reopen(dir.path());
            assert_eq!(base(&ds).column.parsed, 0);
            assert_eq!(answers(&ds), expected);
        }
    }
}

/// Run a SPARQL update with the `geo:` and `ex:` prefixes on a store (in a failpoint).
fn store_update(st: &crate::store::Store, u: &str) {
    crate::sparql::update::update(
        st,
        &format!("PREFIX geo: <{GEO}> PREFIX ex: <{EX}> {u}"),
        &Default::default(),
    )
    .unwrap_or_else(|e| panic!("{u}: {e}"));
}

/// The subjects in windows `NEAR` and `WORLD`, through the index, checked against a scan.
fn windows(ds: &Store) -> Vec<std::collections::BTreeSet<String>> {
    let snap = ds.snapshot();
    [NEAR, WORLD]
        .iter()
        .map(|w| {
            let found = window(snap.clone(), *w, GraphFilter::All).0;
            assert_eq!(found, window(unindexed(&snap), *w, GraphFilter::All).0);
            found
        })
        .collect()
}

#[test]
fn a_compaction_builds_the_base_before_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    let rows0 = base(&ds).rows.len();
    let mut seed = 11;
    let mut ins = String::new();
    for i in 0..300 {
        let (x, y) = (rng(&mut seed) * 40.0, rng(&mut seed) * 40.0);
        ins += &format!("ex:c{i} geo:asWKT \"POINT({x} {y})\"^^geo:wktLiteral . ");
    }
    update(&ds, "INSERT", &ins);
    // commits made during the build reach the new generation as its overlay
    ds.set_failpoint(
        "compact-built",
        Some(Arc::new(|st: &crate::store::Store| {
            store_update(
                st,
                r#"INSERT DATA { ex:d1 geo:asWKT "POINT(1.2 1.2)"^^geo:wktLiteral .
                ex:d2 geo:asWKT "POINT(39 39)"^^geo:wktLiteral }"#,
            );
            store_update(
                st,
                r#"DELETE DATA { ex:g1 geo:asWKT "POINT(2 2)"^^geo:wktLiteral }"#,
            );
        })),
    );
    let r = ds.compact_with(&Default::default()).unwrap();
    ds.set_failpoint("compact-built", None);
    assert_eq!(r.caught_up_commits, 2);
    let snap = ds.snapshot();
    let b = base(&ds);
    assert_eq!(b.generation, snap.generation.uid);
    // the base holds the state the build read, and the commits parsed its new literals
    assert_eq!(b.rows.len(), rows0 + 300);
    assert_eq!(b.column.parsed, 0);
    assert_eq!(snap.geo.as_ref().unwrap().overlay.rows.len(), 2);
    assert!(b.tree.as_ref().unwrap().is_mapped());
    let found = windows(&ds);
    assert!(
        found[0].contains("d1") && !found[0].contains("g1"),
        "{found:?}"
    );
    assert!(found[1].contains("d2"));
    let expected = answers(&ds);
    drop((b, snap, ds));
    // the files the build wrote are read back at open
    let ds = reopen(dir.path());
    assert!(ds.geo_status().unwrap().files.unwrap().opened);
    assert_eq!(windows(&ds), found);
    assert_eq!(answers(&ds), expected);
}

#[test]
fn a_rebuild_during_the_compaction_builds_the_base_at_the_switch() {
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    update(
        &ds,
        "INSERT",
        r#"ex:n1 geo:asWKT "POINT(1 1)"^^geo:wktLiteral"#,
    );
    let epoch = ds.snapshot().geo.as_ref().unwrap().epoch;
    // the base built with the generation is for the old epoch: the switch builds again
    ds.set_failpoint(
        "compact-indexed",
        Some(Arc::new(|st: &crate::store::Store| {
            st.rebuild_geo().unwrap();
            store_update(
                st,
                r#"INSERT DATA { ex:d1 geo:asWKT "POINT(1.2 1.2)"^^geo:wktLiteral }"#,
            );
        })),
    );
    let r = ds.compact_with(&Default::default()).unwrap();
    ds.set_failpoint("compact-indexed", None);
    assert_eq!(r.caught_up_commits, 1);
    assert_eq!(ds.geo_status().unwrap().state, "ready");
    assert_eq!(ds.snapshot().geo.as_ref().unwrap().epoch, epoch + 1);
    assert_eq!(base(&ds).generation, ds.snapshot().generation.uid);
    let found = windows(&ds);
    assert!(
        found[0].contains("n1") && found[0].contains("d1"),
        "{found:?}"
    );
    // so does a disable: the new generation has no index
    ds.set_failpoint(
        "compact-indexed",
        Some(Arc::new(|st: &crate::store::Store| {
            st.disable_geo().unwrap()
        })),
    );
    ds.compact().unwrap();
    ds.set_failpoint("compact-indexed", None);
    assert!(ds.snapshot().geo.is_none());
    assert!(!geo_dir(&ds.snapshot()).exists());
}

#[test]
fn a_build_racing_a_compaction_leaves_no_files_behind() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Store::open(dir.path(), opts()).unwrap();
    ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
    ds.compact().unwrap();
    ds.pause_geo_build(true);
    ds.enable_geo(GeoConfig::default()).unwrap();
    let old = ds.snapshot().generation.dir.clone().unwrap();
    // the compaction builds the new generation's base itself
    ds.compact().unwrap();
    assert!(!old.exists());
    ds.pause_geo_build(false);
    let s = ds.wait_geo().unwrap();
    assert_eq!(s.state, "ready");
    assert!(!old.exists());
    assert!(geo_dir(&ds.snapshot()).join(persist::COLUMN_FILE).exists());
    // a retired generation gets no files
    let snap = ds.snapshot();
    snap.generation.geo.retire();
    std::fs::remove_dir_all(geo_dir(&snap)).unwrap();
    ds.rebuild_geo().unwrap();
    assert!(!geo_dir(&snap).exists());
    assert_eq!(ds.geo_status().unwrap().state, "ready");
}

#[test]
fn backups_and_clones_hold_no_index_files() {
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    assert!(geo_dir(&ds.snapshot()).exists());
    let cap = ds.backup_capture("geo").unwrap();
    assert!(cap.files.iter().any(|f| f.path == "geo.json"));
    assert!(
        cap.files
            .iter()
            .all(|f| !f.path.contains("/geo/") && !f.path.ends_with(".spkg")),
        "{:?}",
        cap.files.iter().map(|f| &f.path).collect::<Vec<_>>()
    );
    let restored = tempfile::tempdir().unwrap();
    let to = restored.path().join("db");
    cap.write_to(&to).unwrap();
    drop(cap);
    let r = reopen(&to);
    assert!(!r.geo_status().unwrap().files.unwrap().opened);
    assert_eq!(answers(&r), answers(&ds));
    let cloned = tempfile::tempdir().unwrap();
    ds.clone_to(cloned.path(), &Default::default()).unwrap();
    let gens: Vec<_> = std::fs::read_dir(cloned.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("gen-"))
        .collect();
    assert!(!gens.is_empty());
    for g in gens {
        assert!(!g.path().join(persist::DIR).exists());
    }
}

#[test]
fn in_memory_stores_write_nothing() {
    let ds = crate::store::Store::in_memory(StoreOptions::default());
    ds.load_str(FIXTURE, RdfFormat::TriG).unwrap();
    ds.compact().unwrap();
    let s = ds.enable_geo(GeoConfig::default()).unwrap();
    assert!(s.files.is_none() && s.memory.mapped_bytes == 0);
}

#[test]
fn read_only_stores_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ds = persistent(dir.path(), GeoConfig::default());
    let expected = answers(&ds);
    let gdir = geo_dir(&ds.snapshot());
    drop(ds);
    std::fs::remove_dir_all(&gdir).unwrap();
    let ro = StoreOptions {
        geo_files: false,
        ..opts()
    };
    let open = |o: StoreOptions| {
        let ds = Store::open(dir.path(), o).unwrap();
        assert_eq!(ds.wait_geo().unwrap().state, "ready");
        ds
    };
    // built in memory, nothing written: not at open, a rebuild, a compaction or a disable
    let ds = open(ro.clone());
    assert!(ds.geo_status().unwrap().files.is_none());
    assert_eq!(answers(&ds), expected);
    ds.rebuild_geo().unwrap();
    ds.compact().unwrap();
    assert!(!geo_dir(&ds.snapshot()).exists());
    assert_eq!(answers(&ds), expected);
    drop(ds);
    // files written by a writable store are read, and damaged ones left alone
    let ds = open(opts());
    let gdir = geo_dir(&ds.snapshot());
    drop(ds);
    let ds = open(ro.clone());
    assert!(ds.geo_status().unwrap().files.unwrap().opened);
    drop(ds);
    let rtree = gdir.join(persist::RTREE_FILE);
    let mut b = std::fs::read(&rtree).unwrap();
    b[70] ^= 1;
    std::fs::write(&rtree, &b).unwrap();
    let ds = open(ro);
    assert!(ds.geo_status().unwrap().files.is_none());
    assert_eq!(answers(&ds), expected);
    ds.disable_geo().unwrap();
    assert_eq!(std::fs::read(&rtree).unwrap(), b);
}
