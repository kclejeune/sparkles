//! `cargo run --example q -- DATA_FILE QUERY_FILE_OR_TEXT` — run a query over a file and
//! print solutions and the executed plan (debugging aid).
use sparkles::io::Source;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store = Store::in_memory(StoreOptions::default());
    for f in args[1].split(',') {
        store.load(&[Source::from_path(std::path::Path::new(f), None).unwrap()]).unwrap();
    }
    let q = std::fs::read_to_string(&args[2]).unwrap_or_else(|_| args[2].clone());
    let base = format!("file://{}", std::fs::canonicalize(&args[2]).map(|p| p.display().to_string()).unwrap_or_default());
    let r = query(store.snapshot(), &q, &QueryOptions { base_iri: Some(base), ..Default::default() }).unwrap();
    println!("{:?}  ({} results, boolean={})", r.vars, r.len(), r.boolean);
    for row in r.rows() {
        println!("  {}", row.iter().map(|t| t.as_ref().map_or("-".into(), |t| t.to_string())).collect::<Vec<_>>().join("  "));
    }
    for t in &r.triples {
        println!("  {t}");
    }
    fn pr(p: &sparkles::sparql::PlanInfo, d: usize) {
        println!("{}{} {} [{} rows]", "  ".repeat(d), p.operator, p.description, p.actual_rows);
        p.children.iter().for_each(|c| pr(c, d + 1));
    }
    pr(&r.plan, 0);
}
