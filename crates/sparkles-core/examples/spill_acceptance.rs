//! Explicit X08 A11/A12 acceptance, outside the ordinary test suite.
//!
//! cargo run --release -p sparkles-core --example spill_acceptance -- \
//!     sort OUTPUT 10000 2000 plain 64
//! Other cases are all, count, sum, avg, min, max, sample, concat, count-distinct and
//! distinct. A12 uses 10000 bases and 1000 repetitions. Each invocation checks one
//! unlimited eager reference and one strictly bounded cursor, then writes receipts.
//! Small invocations exercise exactly the same fixtures and independent oracles,
//! but never set the receipt's fullScale field. Use a fresh OUTPUT per invocation.
use oxrdf::{Literal, NamedNode, Term};
use oxsdatatypes::Decimal;
use serde_json::{Value, json};
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, FallbackPolicy, QueryOptions, query, select_cursor,
};
use sparkles_core::spill::{Spill, SpillOptions};
use sparkles_core::store::{Store, StoreOptions};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Instant;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[global_allocator]
static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;

const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
const SORT_ROWS: usize = 20_000_000;
const GROUPS: usize = 10_000_000;
const AGGREGATES: [&str; 8] = [
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "sample",
    "concat",
    "count-distinct",
];

fn aggregate_expression(case: &str) -> &'static str {
    match case {
        "count" => "COUNT(*)",
        "sum" => "SUM(?value)",
        "avg" => "AVG(?value)",
        "min" => "MIN(?value)",
        "max" => "MAX(?value)",
        "sample" => "SAMPLE(?value)",
        "concat" => "GROUP_CONCAT(STR(?value); separator=\",\")",
        "count-distinct" => "COUNT(DISTINCT ?value)",
        _ => panic!("unknown acceptance case"),
    }
}

struct Config {
    case: String,
    out: PathBuf,
    bases: usize,
    repeats: usize,
    encrypted: bool,
    budget: u64,
}
impl Config {
    fn groups(&self) -> usize {
        self.bases
            .checked_mul(self.repeats)
            .expect("row count overflow")
    }
    fn full_scale(&self) -> bool {
        self.groups()
            == if self.case == "sort" {
                SORT_ROWS
            } else {
                GROUPS
            }
    }
}

fn permutation(n: usize) -> Vec<usize> {
    assert!(n > 0);
    fn gcd(mut a: usize, mut b: usize) -> usize {
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    }
    let mut step = 7919;
    while gcd(step, n) != 1 {
        step -= 1;
    }
    (0..n).map(|i| i * step % n).collect()
}

struct Key {
    sparql: String,
    term: Option<Term>,
    // Exact RDF-term duplicates are true ties. Equal numeric values of different
    // RDF types may be refined by the unlimited ordering using lexical/datatype keys.
    equality: usize,
}
fn keys() -> Vec<Key> {
    let mut out = vec![Key {
        sparql: "UNDEF".into(),
        term: None,
        equality: 0,
    }];
    for (lexical, datatype) in [
        ("7", "integer"),
        ("7.0", "decimal"),
        ("7.0E0", "double"),
        ("-2", "integer"),
        ("3.5", "decimal"),
        ("text-a", "string"),
        ("text-b", "string"),
        ("2020-01-01", "date"),
        ("2020-01-02", "date"),
        ("2020-01-01Z", "date"),
        ("2020-01-01+02:00", "date"),
        ("2020-01-01T00:00:00", "dateTime"),
        ("2020-01-01T00:00:00Z", "dateTime"),
        ("2020-01-01T00:00:00+02:00", "dateTime"),
        ("true", "boolean"),
        ("0", "integer"),
        ("1999-12-31", "date"),
    ] {
        let literal = Literal::new_typed_literal(
            lexical,
            NamedNode::new(format!("{XSD}{datatype}")).unwrap(),
        );
        out.push(Key {
            sparql: literal.to_string(),
            term: Some(literal.into()),
            equality: out.len(),
        });
    }
    let language = Literal::new_language_tagged_literal("text-a", "en").unwrap();
    out.push(Key {
        sparql: language.to_string(),
        term: Some(language.into()),
        equality: out.len(),
    });
    let iri = NamedNode::new("urn:a").unwrap();
    out.push(Key {
        sparql: iri.to_string(),
        term: Some(iri.into()),
        equality: out.len(),
    });
    out
}

fn fixture(config: &Config, ordered: bool) -> String {
    let mut body = String::new();
    if config.case == "sort" {
        let keys = keys();
        body.push_str("VALUES (?g ?key) { ");
        for g in permutation(config.bases) {
            write!(&mut body, "({g} {}) ", keys[g % keys.len()].sparql).unwrap();
        }
    } else {
        body.push_str("VALUES (?g ?offset_g) { ");
        for g in permutation(config.bases) {
            write!(&mut body, "({g} {}) ", g * 7919 % 97).unwrap();
        }
    }
    body.push_str(if config.case == "sort" {
        "} VALUES ?h { "
    } else {
        "} VALUES (?h ?offset_h) { "
    });
    for h in permutation(config.repeats) {
        if config.case == "sort" {
            write!(&mut body, "{h} ").unwrap();
        } else {
            write!(&mut body, "({h} {}) ", h * 104729 % 97).unwrap();
        }
    }
    body.push_str("} ");
    if config.case == "sort" {
        write!(&mut body, "BIND(?g * {} + ?h AS ?row)", config.repeats).unwrap();
        format!(
            "SELECT ?row ?key WHERE {{ {body} }}{}",
            if ordered { " ORDER BY ?key" } else { "" }
        )
    } else {
        body.push_str("VALUES ?v { 3 1 3 2 } BIND(?v + ?offset_g + ?offset_h AS ?value)");
        if config.case == "distinct" {
            format!("SELECT DISTINCT ?g ?h WHERE {{ {body} }} ORDER BY DESC(?g) ?h")
        } else {
            let projection = if config.case == "all" {
                AGGREGATES
                    .iter()
                    .enumerate()
                    .map(|(i, case)| format!("({} AS ?answer{i})", aggregate_expression(case)))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                format!("({} AS ?answer)", aggregate_expression(&config.case))
            };
            format!("SELECT ?g ?h {projection} WHERE {{ {body} }} GROUP BY ?g ?h")
        }
    }
}

fn integer(term: &Option<Term>) -> usize {
    let Some(Term::Literal(value)) = term else {
        panic!("expected an integer, got {term:?}")
    };
    assert_eq!(
        value.datatype().as_str(),
        format!("{XSD}integer"),
        "identity or aggregate integer datatype changed"
    );
    value.value().parse().expect("nonnegative integer result")
}

struct Seen {
    bits: Vec<u64>,
    count: usize,
    size: usize,
}
impl Seen {
    fn new(size: usize) -> Self {
        Self {
            bits: vec![0; size.div_ceil(64)],
            count: 0,
            size,
        }
    }
    fn add(&mut self, id: usize) {
        assert!(id < self.size, "identity {id} outside fixture");
        let bit = 1u64 << (id % 64);
        assert_eq!(self.bits[id / 64] & bit, 0, "duplicate identity {id}");
        self.bits[id / 64] |= bit;
        self.count += 1;
    }
    fn finish(&self) {
        assert_eq!(self.count, self.size, "missing result identities");
    }
}

fn aggregate_code(case: &str, value: &Option<Term>, offset: usize) -> u16 {
    let Some(Term::Literal(literal)) = value else {
        panic!("aggregate is not an RDF literal");
    };
    let datatype = match case {
        "avg" => "decimal",
        "concat" => "string",
        _ => "integer",
    };
    assert_eq!(
        literal.datatype().as_str(),
        format!("{XSD}{datatype}"),
        "aggregate datatype changed"
    );
    match case {
        "count" => assert_eq!(integer(value), 4),
        "sum" => assert_eq!(integer(value), 9 + 4 * offset),
        "avg" => {
            let actual: Decimal = literal.value().parse().expect("exact decimal result");
            let expected = Decimal::from(offset as u64)
                .checked_add("2.25".parse::<Decimal>().unwrap())
                .unwrap();
            assert_eq!(actual, expected, "AVG changed its exact decimal value");
        }
        "min" => assert_eq!(integer(value), 1 + offset),
        "max" => assert_eq!(integer(value), 3 + offset),
        "count-distinct" => assert_eq!(integer(value), 3),
        "sample" => {
            let n = integer(value);
            assert!(
                (offset + 1..=offset + 3).contains(&n),
                "SAMPLE is outside its group"
            );
            return (n - offset) as u16;
        }
        "concat" => {
            let Some(Term::Literal(text)) = value else {
                panic!("GROUP_CONCAT not a literal")
            };
            let values: Vec<usize> = text
                .value()
                .split(',')
                .map(|s| s.parse().expect("concat integer"))
                .collect();
            assert_eq!(values.len(), 4);
            let mut counts = [0; 4];
            let mut code = 0;
            for n in values {
                assert!((offset + 1..=offset + 3).contains(&n));
                let n = n - offset;
                counts[n] += 1;
                code = (code << 2) | n as u16;
            }
            assert_eq!(counts, [0, 1, 1, 2], "GROUP_CONCAT changed multiplicities");
            return code;
        }
        _ => panic!("not an aggregate"),
    }
    0
}

// Both nondeterministic eager choices fit in ten bits: SAMPLE occupies bits8..9
// and the four two-bit GROUP_CONCAT values occupy bits0..7.
fn aggregate_row_code(case: &str, row: &[Option<Term>], offset: usize) -> u16 {
    if case != "all" {
        assert_eq!(row.len(), 3, "aggregate column count changed");
        return aggregate_code(case, &row[2], offset);
    }
    assert_eq!(
        row.len(),
        2 + AGGREGATES.len(),
        "aggregate column count changed"
    );
    let mut code = 0;
    for (case, value) in AGGREGATES.iter().zip(&row[2..]) {
        let choice = aggregate_code(case, value, offset);
        code |= match *case {
            "sample" => choice << 8,
            "concat" => choice,
            _ => 0,
        };
    }
    code
}

fn group_offset(config: &Config, id: usize) -> usize {
    (id / config.repeats * 7919 % 97) + (id % config.repeats * 104729 % 97)
}

fn group_id(config: &Config, row: &[Option<Term>]) -> usize {
    let g = integer(&row[0]);
    let h = integer(&row[1]);
    assert!(g < config.bases && h < config.repeats);
    g * config.repeats + h
}

fn reference_options() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

fn cursor_options() -> CursorOptions {
    CursorOptions {
        batch_rows: 4096,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

fn read_cursor(
    store: &Store,
    text: &str,
    options: &QueryOptions,
    mut check: impl FnMut(Vec<Option<Term>>),
) -> Result<(Value, Value)> {
    let mut cursor = select_cursor(store.snapshot(), text, options, &cursor_options())?;
    while let Some(batch) = cursor.next_batch()? {
        for i in 0..batch.len() {
            check(batch.row(i)?);
        }
    }
    assert_eq!(cursor.status(), CursorStatus::Complete);
    let stats = serde_json::to_value(cursor.stats())?;
    let plan = serde_json::from_str(&cursor.plan_json()?)?;
    cursor.close();
    Ok((stats, plan))
}

fn spill_node(plan: &Value, counter: &str, name: &str) -> bool {
    match plan {
        Value::Object(map) => {
            let own = map
                .get("operator")
                .and_then(Value::as_str)
                .is_some_and(|op| op.contains(name))
                && map
                    .get("counters")
                    .and_then(|v| v.get("spilledBytes"))
                    .and_then(Value::as_u64)
                    .is_some_and(|n| n > 0)
                && map
                    .get("counters")
                    .and_then(|v| v.get(counter))
                    .and_then(Value::as_u64)
                    .is_some_and(|n| n > 0);
            own || map.values().any(|v| spill_node(v, counter, name))
        }
        Value::Array(values) => values.iter().any(|v| spill_node(v, counter, name)),
        _ => false,
    }
}

fn no_fallback(plan: &Value) {
    match plan {
        Value::Object(map) => {
            assert_ne!(
                map.get("materializes"),
                Some(&Value::Bool(true)),
                "eager fallback in acceptance cursor"
            );
            assert_ne!(
                map.get("complete"),
                Some(&Value::Bool(false)),
                "incomplete operator in acceptance cursor"
            );
            for value in map.values() {
                no_fallback(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                no_fallback(value);
            }
        }
        _ => {}
    }
}

/// Assert measured row counts on the actual physical operator, not just the
/// generator's intended cardinality or some other operator's spill bytes.
fn operator_rows(plan: &Value, name: &str, output: usize, input: usize) -> bool {
    match plan {
        Value::Object(map) => {
            let own = map.get("operator").is_some_and(|operator| {
                operator.get("operator").and_then(Value::as_str) == Some(name)
                    && operator.get("actualRows").and_then(Value::as_u64) == Some(output as u64)
                    && map
                        .get("children")
                        .and_then(|children| children.get(0))
                        .and_then(|child| child.get("operator"))
                        .and_then(|operator| operator.get("actualRows"))
                        .and_then(Value::as_u64)
                        == Some(input as u64)
            });
            own || map
                .values()
                .any(|value| operator_rows(value, name, output, input))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| operator_rows(value, name, output, input)),
        _ => false,
    }
}

fn no_files(dir: &Path) -> Result<()> {
    for item in std::fs::read_dir(dir)? {
        let item = item?;
        if item.file_type()?.is_dir() {
            no_files(&item.path())?;
        } else {
            assert_eq!(item.file_name(), "SPARKLES-SPILL", "leftover spill file");
        }
    }
    Ok(())
}

fn sort_oracle(
    config: &Config,
    store: &Store,
    text: &str,
) -> Result<(Vec<u32>, Vec<u32>, u64, Value)> {
    let n = config.groups();
    let mut ranks = vec![u32::MAX; n];
    let mut input_count = 0;
    let input = fixture(config, false);
    std::fs::write(config.out.join("input.rq"), &input)?;
    let (input_stats, input_plan) = read_cursor(store, &input, &reference_options(), |row| {
        let id = integer(&row[0]);
        assert!(id < n);
        assert_eq!(ranks[id], u32::MAX, "duplicate input identity");
        ranks[id] = u32::try_from(input_count).unwrap();
        input_count += 1;
    })?;
    std::fs::write(
        config.out.join("input-plan.json"),
        serde_json::to_vec_pretty(&json!({"stats":input_stats,"plan":input_plan}))?,
    )?;
    assert_eq!(input_count, n);
    let expected = query(store.snapshot(), text, &reference_options())?;
    std::fs::write(
        config.out.join("eager-plan.json"),
        serde_json::to_vec_pretty(&expected.plan)?,
    )?;
    assert_eq!(expected.len(), n);
    let keys = keys();
    let mut seen = Seen::new(n);
    let mut last = [None; 20];
    let mut order = Vec::with_capacity(n);
    for i in 0..n {
        let id = integer(&expected.term(expected.table.cols[0][i]));
        seen.add(id);
        let key = &keys[(id / config.repeats) % keys.len()];
        assert_eq!(
            expected.term(expected.table.cols[1][i]),
            key.term,
            "fixture key changed"
        );
        let rank = ranks[id];
        if let Some(previous) = last[key.equality] {
            assert!(
                previous < rank,
                "unlimited reference changed equal-key input order at position {i}, identity {id}, class {}, previous {previous}, rank {rank}",
                key.equality
            );
        }
        last[key.equality] = Some(rank);
        order.push(u32::try_from(id).unwrap());
    }
    seen.finish();
    let peak = expected.mem_peak_bytes;
    let plan = serde_json::to_value(&expected.plan)?;
    drop(expected);
    Ok((order, ranks, peak, plan))
}

fn run(config: Config) -> Result<()> {
    assert!(config.bases > 0 && config.repeats > 0);
    assert!(config.groups() < u32::MAX as usize);
    assert!(config.budget > 0);
    assert!(
        !config.out.exists(),
        "preserve prior output; use a fresh directory"
    );
    std::fs::create_dir_all(&config.out)?;
    let text = fixture(&config, true);
    std::fs::write(config.out.join("query.rq"), &text)?;
    let store = Store::in_memory(StoreOptions::default());
    let start = Instant::now();
    let n = config.groups();
    let mut oracle_codes = vec![
        0;
        if matches!(config.case.as_str(), "all" | "sample" | "concat") {
            n
        } else {
            0
        }
    ];
    let (sort_order, input_ranks, eager_peak, eager_plan) = if config.case == "sort" {
        sort_oracle(&config, &store, &text)?
    } else {
        let result = query(store.snapshot(), &text, &reference_options())?;
        assert_eq!(result.len(), n);
        let mut seen = Seen::new(n);
        for i in 0..n {
            let row: Vec<_> = result
                .table
                .cols
                .iter()
                .map(|col| result.term(col[i]))
                .collect();
            let id = group_id(&config, &row);
            seen.add(id);
            if config.case == "distinct" {
                let expected =
                    (config.bases - 1 - i / config.repeats) * config.repeats + i % config.repeats;
                assert_eq!(id, expected, "unlimited DISTINCT order");
            } else {
                let code = aggregate_row_code(&config.case, &row, group_offset(&config, id));
                if !oracle_codes.is_empty() {
                    oracle_codes[id] = code;
                }
            }
        }
        seen.finish();
        let peak = result.mem_peak_bytes;
        let plan = serde_json::to_value(&result.plan)?;
        drop(result);
        (Vec::new(), Vec::new(), peak, plan)
    };
    let eager_seconds = start.elapsed().as_secs_f64();
    std::fs::write(
        config.out.join("eager-plan.json"),
        serde_json::to_vec_pretty(&eager_plan)?,
    )?;
    let spill_root = config.out.join("spill");
    let spill = Spill::open(&SpillOptions {
        dir: spill_root.clone(),
        max_bytes: 48 << 30,
        query_bytes: 48 << 30,
        min_free_bytes: 1 << 30,
        encrypted: config.encrypted,
    })?;
    let options = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(config.budget),
        spill: Some(spill.clone()),
        ..Default::default()
    };
    let mut count = 0;
    let mut seen = Seen::new(n);
    let keys = keys();
    let mut last = [None; 20];
    let started = Instant::now();
    let (stats, plan) = read_cursor(&store, &text, &options, |row| {
        let id = if config.case == "sort" {
            integer(&row[0])
        } else {
            group_id(&config, &row)
        };
        seen.add(id);
        if config.case == "sort" {
            assert_eq!(
                id, sort_order[count] as usize,
                "sort sequence differs at row {count}"
            );
            let key = &keys[(id / config.repeats) % keys.len()];
            assert_eq!(row[1], key.term, "fixture key changed");
            let rank = input_ranks[id];
            if let Some(previous) = last[key.equality] {
                assert!(previous < rank, "tie input order changed");
            }
            last[key.equality] = Some(rank);
        } else if config.case == "distinct" {
            let expected = (config.bases - 1 - count / config.repeats) * config.repeats
                + count % config.repeats;
            assert_eq!(id, expected, "spilled DISTINCT sequence");
        } else {
            let code = aggregate_row_code(&config.case, &row, group_offset(&config, id));
            if !oracle_codes.is_empty() {
                assert_eq!(
                    code, oracle_codes[id],
                    "eager aggregate choice/order changed"
                );
            }
        }
        count += 1;
    })?;
    let cursor_seconds = started.elapsed().as_secs_f64();
    assert_eq!(count, n);
    seen.finish();
    no_fallback(&plan);
    assert!(stats["memPeakBytes"].as_u64().unwrap() <= config.budget);
    assert!(
        spill.written_bytes() > 0,
        "fixture did not force physical spill"
    );
    if config.case == "sort" {
        assert!(
            operator_rows(&plan, "OrderBy", n, n),
            "sort actual row counts"
        );
        assert!(
            spill_node(&plan, "spillRuns", "OrderBy"),
            "no physical external sort"
        );
    } else if config.case == "distinct" {
        assert!(
            operator_rows(&plan, "Distinct", n, n * 4),
            "DISTINCT actual row counts"
        );
        assert!(
            spill_node(&plan, "spillPasses", "Distinct"),
            "no physical DISTINCT spill"
        );
        assert!(
            spill_node(&plan, "spillRuns", "OrderBy"),
            "no physical DISTINCT sort"
        );
    } else {
        assert!(
            operator_rows(&plan, "GroupBy", n, n * 4),
            "group actual row counts"
        );
        assert!(
            spill_node(&plan, "spillPasses", "Group"),
            "no physical grouping spill"
        );
    }
    assert_eq!(spill.reserved_bytes(), 0);
    no_files(&spill_root)?;
    std::fs::write(
        config.out.join("cursor-plan.json"),
        serde_json::to_vec_pretty(&plan)?,
    )?;
    let report = json!({"complete":true,"case":config.case,"fullScale":config.full_scale(),"bases":config.bases,"repeats":config.repeats,"rows":count,"inputRows":if config.case=="sort" {n} else {n*4},"budgetBytes":config.budget,"encrypted":config.encrypted,"eagerPeakBytes":eager_peak,"eagerSeconds":eager_seconds,"cursorSeconds":cursor_seconds,"cursorStats":stats,"spillWrittenBytes":spill.written_bytes(),"spillReservedAfterClose":spill.reserved_bytes(),"oracle":"exact sequence or exact dense-group coverage with independent values and eager choices; no bag-hash-only check","cleanup":"no spill files after cursor close"});
    std::fs::write(
        config.out.join("result.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 7 {
        return Err(
            "usage: spill_acceptance CASE OUTPUT BASES REPEATS plain|encrypted BUDGET_MIB".into(),
        );
    }
    let encrypted = match args[5].as_str() {
        "plain" => false,
        "encrypted" => true,
        _ => return Err("unknown spill mode".into()),
    };
    run(Config {
        case: args[1].clone(),
        out: args[2].clone().into(),
        bases: args[3].parse()?,
        repeats: args[4].parse()?,
        encrypted,
        budget: args[6]
            .parse::<u64>()?
            .checked_mul(1 << 20)
            .ok_or("budget overflow")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tie_oracle_keeps_distinct_numeric_rdf_terms_separate() {
        let keys = keys();
        assert_ne!(keys[1].term, keys[2].term);
        assert_ne!(keys[1].equality, keys[2].equality);
        assert_ne!(keys[2].equality, keys[3].equality);
    }
    #[test]
    fn fixture_permutations_are_complete_even_at_shared_factors() {
        for n in [1, 17, 7919, 10000] {
            let mut values = permutation(n);
            values.sort_unstable();
            assert_eq!(values, (0..n).collect::<Vec<_>>());
        }
    }
    #[test]
    #[should_panic(expected = "duplicate identity")]
    fn identity_oracle_refuses_duplicate_rows() {
        let mut seen = Seen::new(3);
        seen.add(1);
        seen.add(1);
    }
    #[test]
    #[should_panic(expected = "missing result")]
    fn identity_oracle_refuses_missing_rows() {
        let seen = Seen::new(3);
        seen.finish();
    }
    #[test]
    #[should_panic(expected = "multiplicities")]
    fn concat_oracle_refuses_a_correct_length_wrong_bag() {
        aggregate_code(
            "concat",
            &Some(Literal::new_simple_literal("3,1,2,2").into()),
            0,
        );
    }
    #[test]
    fn concat_oracle_preserves_order_beside_independent_bag_check() {
        assert_ne!(
            aggregate_code(
                "concat",
                &Some(Literal::new_simple_literal("3,1,3,2").into()),
                0,
            ),
            aggregate_code(
                "concat",
                &Some(Literal::new_simple_literal("1,3,2,3").into()),
                0,
            )
        );
    }
    #[test]
    fn physical_spill_check_requires_the_correct_operator() {
        let wrong = json!({"operator":"Group","counters":{"spilledBytes":20,"spillPasses":2}});
        assert!(!spill_node(&wrong, "spillPasses", "Distinct"));
        assert!(spill_node(&wrong, "spillPasses", "Group"));
    }

    #[test]
    #[should_panic(expected = "datatype")]
    fn numeric_oracle_refuses_a_numeric_looking_string() {
        aggregate_code("sum", &Some(Literal::new_simple_literal("9").into()), 0);
    }

    #[test]
    #[should_panic]
    fn aggregate_oracle_refuses_another_groups_values() {
        aggregate_code("sum", &Some(Literal::from(9).into()), 5);
    }

    #[test]
    fn cardinality_proof_rejects_a_small_fixture_with_a_large_claim() {
        let plan = json!({"operator":{"operator":"GroupBy","actualRows":100},
            "children":[{"operator":{"operator":"CartesianProduct","actualRows":400}}]});
        assert!(operator_rows(&plan, "GroupBy", 100, 400));
        assert!(!operator_rows(&plan, "GroupBy", 10_000_000, 40_000_000));
    }
    #[test]
    #[should_panic(expected = "datatype")]
    fn identity_oracle_refuses_a_string_typed_integer() {
        integer(&Some(Literal::new_simple_literal("7").into()));
    }

    #[test]
    #[should_panic(expected = "integer result")]
    fn aggregate_oracle_refuses_invalid_integer_exponent_syntax() {
        aggregate_code(
            "count",
            &Some(
                Literal::new_typed_literal("4e0", NamedNode::new(format!("{XSD}integer")).unwrap())
                    .into(),
            ),
            0,
        );
    }

    #[test]
    #[should_panic(expected = "exact decimal value")]
    fn average_oracle_refuses_an_inexact_decimal_hidden_by_float_rounding() {
        aggregate_code(
            "avg",
            &Some(
                Literal::new_typed_literal(
                    "2.25000000000000001",
                    NamedNode::new(format!("{XSD}decimal")).unwrap(),
                )
                .into(),
            ),
            0,
        );
    }

    #[test]
    fn numeric_oracles_accept_equivalent_exact_lexical_values() {
        aggregate_code(
            "count",
            &Some(
                Literal::new_typed_literal(
                    "+004",
                    NamedNode::new(format!("{XSD}integer")).unwrap(),
                )
                .into(),
            ),
            0,
        );
        aggregate_code(
            "avg",
            &Some(
                Literal::new_typed_literal(
                    "+2.2500",
                    NamedNode::new(format!("{XSD}decimal")).unwrap(),
                )
                .into(),
            ),
            0,
        );
    }
    fn combined_row() -> Vec<Option<Term>> {
        vec![
            Some(Literal::from(0).into()),
            Some(Literal::from(0).into()),
            Some(Literal::from(4).into()),
            Some(Literal::from(9).into()),
            Some(
                Literal::new_typed_literal(
                    "2.25",
                    NamedNode::new(format!("{XSD}decimal")).unwrap(),
                )
                .into(),
            ),
            Some(Literal::from(1).into()),
            Some(Literal::from(3).into()),
            Some(Literal::from(3).into()),
            Some(Literal::new_simple_literal("3,1,3,2").into()),
            Some(Literal::from(3).into()),
        ]
    }

    #[test]
    fn combined_oracle_checks_every_column() {
        let row = combined_row();
        aggregate_row_code("all", &row, 0);
        for index in 2..row.len() {
            let mut wrong = row.clone();
            wrong[index] = None;
            assert!(
                std::panic::catch_unwind(|| aggregate_row_code("all", &wrong, 0)).is_err(),
                "unchecked column {index}"
            );
        }
        assert!(std::panic::catch_unwind(|| aggregate_row_code("all", &row[..9], 0)).is_err());
        let mut extra = row.clone();
        extra.push(None);
        assert!(std::panic::catch_unwind(|| aggregate_row_code("all", &extra, 0)).is_err());
    }

    #[test]
    fn combined_choice_code_preserves_both_eager_choices() {
        let row = combined_row();
        let code = aggregate_row_code("all", &row, 0);
        let mut sample = row.clone();
        sample[7] = Some(Literal::from(1).into());
        let mut concat = row.clone();
        concat[8] = Some(Literal::new_simple_literal("1,3,2,3").into());
        assert_ne!(aggregate_row_code("all", &sample, 0), code);
        assert_ne!(aggregate_row_code("all", &concat, 0), code);
        let concat_bits = code & 255;
        for sample in 1..=3 {
            assert_eq!(((sample << 8) | concat_bits) & 255, concat_bits);
        }
    }
}
