//! `cargo run --release --example qdb -- DB_DIR QUERY` — run a query against an existing
//! database directory and print the row count, timing and executed plan.
use sparkles::sparql::{PlanInfo, QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let store = Store::open(std::path::Path::new(&args[1]), StoreOptions::default()).unwrap();
    let opts = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    let r = query(store.snapshot(), &args[2], &opts).unwrap();
    println!(
        "{} results (boolean={}) in {:.2} ms",
        r.len(),
        r.boolean,
        r.timing.total_ms
    );
    fn pr(p: &PlanInfo, d: usize) {
        println!(
            "{}{} {} [{} rows, {:.2} ms]",
            "  ".repeat(d),
            p.operator,
            p.description,
            p.actual_rows,
            p.time_ms
        );
        p.children.iter().for_each(|c| pr(c, d + 1));
    }
    pr(&r.plan, 0);
}
