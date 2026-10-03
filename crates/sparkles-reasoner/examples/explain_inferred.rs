//! Load an N-Triples file, materialize a profile, then run a query over default ∪
//! inferred and print estimated vs actual rows per operator (planner diagnostics).
//! `cargo run --release -p sparkles-reasoner --example explain_inferred -- data.nt owl-rl QUERY`
use sparkles_core::io::Source;
use sparkles_core::sparql::{PlanInfo, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[Source::from_path(std::path::Path::new(&a[1]), None).unwrap()])
        .unwrap();
    let rep = sparkles_reasoner::materialize(&store, &a[2].parse().unwrap(), &Default::default())
        .unwrap();
    println!(
        "inferred {} (delta +{})",
        rep.inferred,
        store.snapshot().delta.inserts()
    );
    let opts = QueryOptions {
        default_graph_extra: vec![sparkles_reasoner::INFERRED_GRAPH.into()],
        no_cache: true,
        ..Default::default()
    };
    let r = query(store.snapshot(), &a[3], &opts).unwrap();
    fn pr(p: &PlanInfo, d: usize) {
        println!(
            "{}{} {} [est {} / actual {}]",
            "  ".repeat(d),
            p.operator,
            p.description,
            p.estimated_rows,
            p.actual_rows
        );
        p.children.iter().for_each(|c| pr(c, d + 1));
    }
    pr(&r.plan, 0);
}
