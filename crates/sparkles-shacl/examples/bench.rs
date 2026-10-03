//! Validation benchmark on a `scripts/gen-data.py` dataset.
//!
//! ```sh
//! python3 scripts/gen-data.py 10000 > data.nt
//! cargo run --release -p sparkles-shacl --example bench -- data.nt [shapes.ttl|-] [iterations] [expected-report.ttl]
//! ```
//!
//! With an expected report (e.g. from Jena's `shacl validate`), the results are compared
//! on (focusNode, resultPath, value, sourceConstraintComponent, severity).

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::{Shapes, ValidateOptions, validate};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

const SHAPES: &str = r#"
@prefix ex: <http://example.org/> .
@prefix foaf: <http://xmlns.com/foaf/0.1/> .
@prefix sh: <http://www.w3.org/ns/shacl#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:PersonShape a sh:NodeShape ; sh:targetClass ex:Person ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ; sh:maxCount 1 ; sh:datatype xsd:string ] ;
  sh:property [ sh:path foaf:age ; sh:datatype xsd:integer ; sh:minInclusive 18 ; sh:maxInclusive 65 ] ;
  sh:property [ sh:path ex:salary ; sh:datatype xsd:decimal ; sh:minExclusive 0 ] ;
  sh:property [ sh:path foaf:knows ; sh:class ex:Person ; sh:nodeKind sh:IRI ] ;
  sh:property [ sh:path ex:authorOf ; sh:class ex:Document ; sh:maxCount 1 ] ;
  sh:property [ sh:path ( foaf:knows ex:worksFor ) ; sh:maxCount 4 ] .

ex:EmployeeShape a sh:NodeShape ; sh:targetClass ex:Employee ;
  sh:property [ sh:path ex:worksFor ; sh:minCount 1 ; sh:class ex:Organization ] .

ex:StudentShape a sh:NodeShape ; sh:targetClass ex:Student ;
  sh:property [ sh:path ex:advisor ; sh:minCount 1 ; sh:node ex:ResearcherShape ] .
ex:ResearcherShape a sh:NodeShape ; sh:class ex:Researcher .

ex:DocumentShape a sh:NodeShape ; sh:targetClass ex:Document ;
  sh:property [ sh:path ex:title ; sh:minCount 1 ; sh:uniqueLang true ; sh:pattern "^On the theory" ; sh:languageIn ( "en" ) ] ;
  sh:property [ sh:path ex:year ; sh:datatype xsd:integer ; sh:maxInclusive 2025 ] ;
  sh:property [ sh:path ex:cites ; sh:class ex:Document ] .

ex:OrgShape a sh:NodeShape ; sh:targetClass ex:Organization ;
  sh:closed true ; sh:ignoredProperties ( rdf:type ) ;
  sh:property [ sh:path foaf:name ; sh:minCount 1 ] ;
  sh:property [ sh:path ex:city ; sh:in ( "Kyoto" "Paris" "Berlin" "Boston" "Zurich" "Toronto" "Freiburg" "London" ) ] ;
  sh:property [ sh:path ex:founded ; sh:datatype xsd:date ] .
"#;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let data = args
        .get(1)
        .expect("usage: bench DATA.nt [SHAPES.ttl] [ITERATIONS]");
    let shapes_text = match args.get(2).filter(|s| *s != "-") {
        Some(p) => std::fs::read_to_string(p)?,
        None => SHAPES.to_string(),
    };
    let iters: usize = args.get(3).map_or(5, |s| s.parse().unwrap());

    let t = Instant::now();
    let store = Store::in_memory(StoreOptions::default());
    let n = store.load(&[Source::from_path(Path::new(data), None)?])?;
    println!(
        "load: {n} triples in {:.1} ms",
        t.elapsed().as_secs_f64() * 1e3
    );
    let snap = store.snapshot();

    let t = Instant::now();
    let shapes = Shapes::parse(&shapes_text, RdfFormat::Turtle, None)?;
    println!(
        "shapes: {} shapes parsed in {:.2} ms",
        shapes.len(),
        t.elapsed().as_secs_f64() * 1e3
    );

    for parallel in [false, true] {
        let opts = ValidateOptions {
            parallel,
            ..Default::default()
        };
        let mut times = Vec::new();
        let mut report = None;
        for _ in 0..iters {
            let t = Instant::now();
            let r = validate(&snap, &shapes, &opts)?;
            times.push(t.elapsed().as_secs_f64() * 1e3);
            report = Some(r);
        }
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let r = report.unwrap();
        let mut by: BTreeMap<String, usize> = BTreeMap::new();
        for x in &r.results {
            let c = x
                .source_constraint_component
                .as_str()
                .rsplit('#')
                .next()
                .unwrap()
                .to_string();
            *by.entry(c).or_default() += 1;
        }
        println!(
            "validate ({}): median {:.1} ms, min {:.1} ms over {iters} runs; conforms={} results={} {by:?}",
            if parallel { "parallel" } else { "sequential" },
            times[times.len() / 2],
            times[0],
            r.conforms,
            r.results.len()
        );
    }
    let t = Instant::now();
    let r = validate(&snap, &shapes, &ValidateOptions::default())?;
    let ttl = r.to_turtle();
    if let Some(expected) = args.get(4) {
        let text = std::fs::read(expected)?;
        let mut g = oxrdf::Graph::new();
        for t in oxttl::TurtleParser::new().for_slice(&text) {
            g.insert(&t?);
        }
        let exp = sparkles_shacl::ValidationReport::from_rdf(&g, None)?;
        let key = |x: &sparkles_shacl::ValidationResult| {
            format!(
                "{} {:?} {:?} {} {}",
                x.focus_node,
                x.result_path.as_ref().map(|p| p.to_string()),
                x.value.as_ref().map(|v| v.to_string()),
                x.source_constraint_component,
                x.severity
            )
        };
        let mut a: Vec<String> = r.results.iter().map(key).collect();
        let mut b: Vec<String> = exp.results.iter().map(key).collect();
        a.sort();
        b.sort();
        println!(
            "compare with {expected}: {} (ours {}, expected {})",
            if a == b { "identical" } else { "DIFFERENT" },
            a.len(),
            b.len()
        );
    }
    println!(
        "report: {} bytes of Turtle in {:.1} ms (incl. validation)",
        ttl.len(),
        t.elapsed().as_secs_f64() * 1e3
    );
    Ok(())
}
