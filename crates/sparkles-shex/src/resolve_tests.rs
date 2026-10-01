use super::*;
use crate::NodeKind;

/// A directory of schema files.
struct Dir(tempfile::TempDir);

impl Dir {
    fn new(files: &[(&str, &str)]) -> Dir {
        let d = tempfile::tempdir().unwrap();
        for (name, text) in files {
            let p = d.path().join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        Dir(d)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.path().join(name)
    }

    /// The schema in `name`, read with its file URL as the base.
    fn schema(&self, name: &str) -> Schema {
        let p = self.path(name);
        Schema::parse_shexc(&std::fs::read_to_string(&p).unwrap(), Some(&file_url(&p))).unwrap()
    }
}

fn labels(s: &Schema) -> Vec<String> {
    s.shapes.iter().map(|d| d.label.to_shexj()).collect()
}

fn iri(l: &str) -> Label {
    Label::Iri(format!("http://ex.org/{l}"))
}

const P: &str = "PREFIX : <http://ex.org/>\n";

fn err(schema: &Schema, r: &dyn Resolver) -> String {
    close(schema, r).unwrap_err().message
}

#[test]
fn transitive_imports_once_each() {
    let d = Dir::new(&[
        (
            "a.shex",
            &format!("{P}IMPORT <b> IMPORT <c>\n:A {{ :p @:B ; :q @:C }}"),
        ),
        ("b.shex", &format!("{P}IMPORT <d>\n:B {{ :p @:D }}")),
        ("c.shex", &format!("{P}IMPORT <d>\n:C {{ :p @:D }}")),
        ("d.shex", &format!("{P}:D {{ :p . }}")),
    ]);
    let r = FileResolver::default();
    let s = close(&d.schema("a.shex"), &r).unwrap();
    assert_eq!(
        labels(&s),
        [
            "http://ex.org/A",
            "http://ex.org/B",
            "http://ex.org/C",
            "http://ex.org/D"
        ]
    );
    assert_eq!(r.used.schemas(), 3);
    crate::compile(&d.schema("a.shex"), &FileResolver::default()).unwrap();
}

#[test]
fn cycles_end() {
    let d = Dir::new(&[
        ("a.shex", &format!("{P}IMPORT <b>\n:A {{ :p @:B }}")),
        ("b.shex", &format!("{P}IMPORT <a>\n:B {{ :p @:A }}")),
        ("self.shex", &format!("{P}IMPORT <self>\n:S {{ :p @:S }}")),
    ]);
    let r = FileResolver::default();
    let s = close(&d.schema("a.shex"), &r).unwrap();
    assert_eq!(labels(&s), ["http://ex.org/A", "http://ex.org/B"]);
    // a is not fetched again
    assert_eq!(r.used.schemas(), 1);
    let s = close(&d.schema("self.shex"), &FileResolver::default()).unwrap();
    assert_eq!(labels(&s), ["http://ex.org/S"]);
    // the importing schema reached under a name its base does not give
    let mut a = d.schema("a.shex");
    a.base = None;
    let s = close(&a, &FileResolver::default()).unwrap();
    assert_eq!(labels(&s), ["http://ex.org/A", "http://ex.org/B"]);
}

#[test]
fn overlapping_labels_are_errors() {
    let d = Dir::new(&[
        (
            "a.shex",
            &format!("{P}IMPORT <b> IMPORT <c>\n:A {{ :p . }}"),
        ),
        ("b.shex", &format!("{P}:B {{ :p . }}")),
        ("c.shex", &format!("{P}:B {{ :q . }}")),
        ("x.shex", &format!("{P}IMPORT <y>\n:A {{ :p . }}")),
        ("y.shex", &format!("{P}:A {{ :q . }}")),
    ]);
    let m = err(&d.schema("a.shex"), &FileResolver::default());
    assert!(
        m.starts_with("<http://ex.org/B> is declared both in <file://"),
        "{m}"
    );
    assert!(m.contains("/b> and in the imported schema <file://"), "{m}");
    assert!(m.ends_with("/c>"), "{m}");
    let m = err(&d.schema("x.shex"), &FileResolver::default());
    assert!(
        m.starts_with("<http://ex.org/A> is declared both in the importing schema and in"),
        "{m}"
    );
}

#[test]
fn imported_start_is_ignored_and_start_actions_are_errors() {
    let d = Dir::new(&[
        (
            "a.shex",
            &format!("{P}IMPORT <b>\nstart = @:A\n:A {{ :p @:B }}"),
        ),
        ("b.shex", &format!("{P}start = @:B\n:B {{ :p . }}")),
        ("n.shex", &format!("{P}IMPORT <b>\n:A {{ :p @:B }}")),
        ("acts.shex", &format!("{P}IMPORT <withacts>\n:A {{ }}")),
        ("withacts.shex", &format!("{P}%:x{{ y %}}\n:B {{ }}")),
    ]);
    let s = close(&d.schema("a.shex"), &FileResolver::default()).unwrap();
    assert_eq!(s.start, Some(ShapeExpr::Ref(iri("A"))));
    let s = close(&d.schema("n.shex"), &FileResolver::default()).unwrap();
    assert_eq!(s.start, None);
    let m = err(&d.schema("acts.shex"), &FileResolver::default());
    assert!(
        m.starts_with("the imported schema <file://")
            && m.ends_with("/withacts> has start actions"),
        "{m}"
    );
}

#[test]
fn extensions_are_tried() {
    let d = Dir::new(&[
        (
            "a.shex",
            &format!("{P}IMPORT <b> IMPORT <c.shex> IMPORT <j>\n:A {{ }}"),
        ),
        ("b", &format!("{P}:B {{ }}")),
        ("b.shex", &format!("{P}:Wrong {{ }}")),
        ("c.shex", &format!("{P}:C {{ }}")),
        ("j.json", "{ \"type\": \"Schema\" }"),
    ]);
    // the exact name first, then .shex, then .json (ShExJ, sniffed)
    let s = close(&d.schema("a.shex"), &FileResolver::default());
    match s {
        Ok(s) => assert_eq!(
            labels(&s),
            ["http://ex.org/A", "http://ex.org/B", "http://ex.org/C"]
        ),
        // until the ShExJ reader is in place, j.json is found and fails to read
        Err(e) => assert!(e.message.contains("/j.json>"), "{e}"),
    }
    let mut no_json = d.schema("a.shex");
    no_json.imports.pop();
    let s = close(&no_json, &FileResolver::default()).unwrap();
    assert_eq!(
        labels(&s),
        ["http://ex.org/A", "http://ex.org/B", "http://ex.org/C"]
    );
    let mut missing = d.schema("a.shex");
    missing.imports = vec![file_url(&d.path("nothere"))];
    let m = err(&missing, &FileResolver::default());
    assert!(
        m.starts_with("import <file://") && m.ends_with("/nothere> could not be resolved"),
        "{m}"
    );
    // inline bodies, with the same rule
    let r = FileResolver {
        inline: HashMap::from([(
            "http://ex.org/lib.json".to_string(),
            Schema::parse_shexc(&format!("{P}:L {{ }}"), None).unwrap(),
        )]),
        ..Default::default()
    };
    let s = Schema::parse_shexc(&format!("{P}IMPORT :lib\n:A {{ :p @:L }}"), None).unwrap();
    assert_eq!(
        labels(&close(&s, &r).unwrap()),
        ["http://ex.org/A", "http://ex.org/L"]
    );
}

#[test]
fn relative_imports_use_the_directories() {
    let d = Dir::new(&[("sub/b.shex", &format!("{P}:B {{ }}"))]);
    let s = Schema::parse_shexc(&format!("{P}IMPORT <b>\n:A {{ :p @:B }}"), None).unwrap();
    assert_eq!(s.imports, ["b"]);
    let r = FileResolver {
        dirs: vec![d.path("nowhere"), d.path("sub")],
        ..Default::default()
    };
    assert_eq!(
        labels(&close(&s, &r).unwrap()),
        ["http://ex.org/A", "http://ex.org/B"]
    );
    let m = err(&s, &FileResolver::default());
    assert_eq!(m, "import <b> could not be resolved");
}

#[test]
fn file_rules_apply() {
    let d = Dir::new(&[
        (
            "load/a.shex",
            &format!("{P}IMPORT <b> IMPORT <../out>\n:A {{ }}"),
        ),
        ("load/b.shex", &format!("{P}:B {{ }}")),
        ("out.shex", &format!("{P}:O {{ }}")),
    ]);
    let mut a = d.schema("load/a.shex");
    let r = FileResolver {
        files: FileLoads::under(&d.path("load")).unwrap(),
        ..Default::default()
    };
    let m = err(&a, &r);
    assert!(
        m.starts_with("import not allowed: <file://")
            && m.ends_with("/out> is not a file in the load directory"),
        "{m}"
    );
    a.imports.pop();
    assert_eq!(
        labels(&close(&a, &r).unwrap()),
        ["http://ex.org/A", "http://ex.org/B"]
    );
    let r = FileResolver {
        files: FileLoads::Disabled,
        ..Default::default()
    };
    assert_eq!(
        err(&a, &r),
        "import not allowed: file imports are not enabled (no load directory is configured)"
    );
}

#[test]
fn network_imports_go_through_the_policy() {
    let s = Schema::parse_shexc(&format!("{P}IMPORT <http://10.0.0.1/x>\n:A {{ }}"), None).unwrap();
    assert_eq!(
        err(&s, &FileResolver::default()),
        "import not allowed: <http://10.0.0.1/x>: http(s) imports are not enabled"
    );
    let policy = OutboundPolicy {
        allow_private: false,
        ..Default::default()
    };
    let budget = RequestBudget::new(&policy);
    let r = FileResolver {
        outbound: Some((policy, budget)),
        ..Default::default()
    };
    let m = err(&s, &r);
    assert!(
        m.starts_with("import not allowed: GET <http://10.0.0.1/x>"),
        "{m}"
    );
    // other schemes are unknown
    let s = Schema::parse_shexc(&format!("{P}IMPORT <urn:x:y>\n:A {{ }}"), None).unwrap();
    assert_eq!(err(&s, &r), "import <urn:x:y> could not be resolved");
}

#[test]
fn limits() {
    let d = Dir::new(&[
        ("a.shex", &format!("{P}IMPORT <b> IMPORT <c>\n:A {{ }}")),
        ("b.shex", &format!("{P}:B {{ }}")),
        ("c.shex", &format!("{P}:C {{ :p . ; :q . ; :r . }}")),
    ]);
    let a = d.schema("a.shex");
    let r = FileResolver {
        limits: ImportLimits {
            max_schemas: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let m = err(&a, &r);
    assert!(
        m.ends_with("/c>: the imports exceed the limit of 1 schemas"),
        "{m}"
    );
    let b_len = std::fs::metadata(d.path("b.shex")).unwrap().len();
    let r = FileResolver {
        limits: ImportLimits {
            max_bytes: b_len + 5,
            ..Default::default()
        },
        ..Default::default()
    };
    let m = err(&a, &r);
    assert!(
        m.ends_with(&format!(
            "/c>: the imports exceed the limit of {} bytes",
            b_len + 5
        )),
        "{m}"
    );
    assert_eq!(ImportLimits::default().max_schemas, DEFAULT_MAX_IMPORTS);
}

#[test]
fn externals_from_externs() {
    let s = Schema::parse_shexc(
        &format!("{P}:S {{ :p @:Ext }}\n:Ext EXTERNAL\n:Missing EXTERNAL"),
        None,
    )
    .unwrap();
    let externs = Schema::parse_shexc(
        &format!("{P}:Ext {{ :q @:Helper }}\n:Helper IRI\n:Unused {{ }}\n:Missing EXTERNAL"),
        None,
    )
    .unwrap();
    let r = FileResolver {
        externs: Some(externs),
        ..Default::default()
    };
    let c = close(&s, &r).unwrap();
    assert_eq!(
        labels(&c),
        [
            "http://ex.org/S",
            "http://ex.org/Ext",
            "http://ex.org/Missing",
            "http://ex.org/Helper"
        ]
    );
    assert!(matches!(c.shapes[1].expr, ShapeExpr::Shape(_)));
    assert_eq!(c.shapes[2].expr, ShapeExpr::External);
    assert!(matches!(&c.shapes[3].expr, ShapeExpr::Nc(nc) if nc.node_kind == Some(NodeKind::Iri)));
    // without a definition, a referenced EXTERNAL shape fails the checks
    let s = Schema::parse_shexc(&format!("{P}:S {{ :p @:Ext }}\n:Ext EXTERNAL"), None).unwrap();
    let e = crate::compile(&s, &FileResolver::default()).unwrap_err();
    assert!(e.message.contains("has no definition"), "{e}");
    let r = FileResolver {
        externs: Some(Schema::parse_shexc(&format!("{P}:Ext {{ :q . }}"), None).unwrap()),
        ..Default::default()
    };
    crate::compile(&s, &r).unwrap();
}

#[test]
fn file_urls() {
    assert_eq!(
        file_url(Path::new("/a b/c%d.shex")),
        "file:///a%20b/c%25d.shex"
    );
    assert!(file_url(Path::new("rel")).starts_with("file:///"));
}
