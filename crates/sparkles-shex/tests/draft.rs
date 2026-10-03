//! The ShEx schemas drafted from the data (`sparkles_core::schema::draft`): at support 1 every
//! association of the draft's shape map conforms, and below 1 only instances that a
//! drafted constraint excludes fail.

use sparkles_core::schema::draft::{DraftOptions, ShapesDraft};
use sparkles_core::schema::draft_shapes;
use sparkles_core::store::Store;
use sparkles_shex::{NoImports, ResultMap, ShapeMap, Status, ValidateOptions};

#[path = "../../sparkles-shacl/tests/draft_fixture/mod.rs"]
mod draft_fixture;
use draft_fixture::{EX, fixture, random_data, store};

fn draft(s: &Store, support: f64, closed: bool) -> ShapesDraft {
    let o = DraftOptions {
        dataset: "t".into(),
        support,
        closed,
        prefixes: vec![("ex".into(), EX.into())],
        ..Default::default()
    };
    draft_shapes(&s.snapshot(), &o).unwrap_or_else(|e| panic!("{e}"))
}

fn validate(s: &Store, d: &ShapesDraft) -> ResultMap {
    let schema = sparkles_shex::parse_schema(&d.shex, None, None)
        .unwrap_or_else(|e| panic!("{e}\n{}", d.shex));
    let compiled =
        sparkles_shex::compile(&schema, &NoImports).unwrap_or_else(|e| panic!("{e}\n{}", d.shex));
    let map = ShapeMap::parse(&d.shape_map, compiled.prefixes(), None)
        .unwrap_or_else(|e| panic!("{e}\n{}", d.shape_map));
    sparkles_shex::validate(&s.snapshot(), &compiled, &map, &ValidateOptions::default()).unwrap()
}

#[test]
fn drafts_at_full_support_conform() {
    let s = store(&fixture());
    for closed in [false, true] {
        let d = draft(&s, 1.0, closed);
        let r = validate(&s, &d);
        let bad: Vec<_> = r
            .results
            .iter()
            .filter(|x| x.status == Status::Nonconformant)
            .map(|x| format!("{} @ {:?}: {:?}", x.node, x.shape, x.reason))
            .collect();
        assert!(r.conforms, "closed={closed}: {bad:#?}\n{}", d.shex);
        // every direct instance of every class was checked
        assert!(r.conformant >= 37, "{}", r.conformant);
    }
}

#[test]
fn random_drafts_conform() {
    for seed in 1..=60u64 {
        let data = random_data(seed);
        let s = store(&data);
        for closed in [false, true] {
            let d = draft(&s, 1.0, closed);
            let r = validate(&s, &d);
            let bad: Vec<_> = r
                .results
                .iter()
                .filter(|x| x.status == Status::Nonconformant)
                .map(|x| format!("{} @ {:?}: {:?}", x.node, x.shape, x.reason))
                .take(3)
                .collect();
            assert!(
                r.conforms,
                "seed {seed} closed={closed}: {bad:#?}\n{}\n{data}",
                d.shex
            );
        }
    }
}

#[test]
fn lower_support_fails_only_excluded_instances() {
    let s = store(&fixture());
    let d = draft(&s, 0.9, false);
    let r = validate(&s, &d);
    let excluded: u64 = d
        .shapes
        .iter()
        .flat_map(|s| &s.properties)
        .flat_map(|p| &p.constraints)
        .map(|c| c.excluded)
        .sum();
    assert!(r.nonconformant > 0, "{}", d.shex);
    assert!(
        r.nonconformant as u64 <= excluded,
        "{} nonconformant, {excluded} excluded",
        r.nonconformant
    );
    // the misspelt status fails the person shape
    assert!(r.results.iter().any(|x| x.status == Status::Nonconformant
        && x.node.to_string() == format!("<{EX}p7>")));
}
