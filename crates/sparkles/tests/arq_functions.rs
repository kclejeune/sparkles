//! XPath `fn:` and Jena ARQ `afn:` functions, compared with ARQ's answers.
//!
//! Each case is an expression and the value Jena 6.2.0's `arq` gives for it, written as
//! a SPARQL term (an empty string for unbound). Sparkles' value must have the same
//! datatype and be equal to it; strings and temporal values must also have the same
//! lexical form. Where Sparkles answers otherwise on purpose, the case says so and gives
//! Sparkles' value.

use oxrdf::Term;
use sparkles::sparql::{QueryOptions, query};
use sparkles::store::{Store, StoreOptions};

const PREFIXES: &str = "PREFIX fn: <http://www.w3.org/2005/xpath-functions#>
PREFIX afn: <http://jena.apache.org/ARQ/function#>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
";

fn check(s: &Store, expr: &str, want: &str) {
    let q = if want.is_empty() {
        format!("{PREFIXES}SELECT ?x WHERE {{ BIND(({expr}) AS ?x) }}")
    } else {
        let lexical = if want.starts_with('"') {
            format!("(STR(?x) = STR({want}))")
        } else {
            "true".to_string()
        };
        format!(
            "{PREFIXES}SELECT ?x (DATATYPE(?x) = DATATYPE({want}) && ?x = {want} && {lexical} AS ?ok) \
             WHERE {{ BIND(({expr}) AS ?x) }}"
        )
    };
    let rows = query(s.snapshot(), &q, &QueryOptions::default())
        .unwrap_or_else(|e| panic!("{expr}: {e}"))
        .rows();
    let row = &rows[0];
    if want.is_empty() {
        assert!(row[0].is_none(), "{expr}: {:?}, want unbound", row[0]);
    } else {
        let ok = matches!(&row[1], Some(Term::Literal(l)) if l.value() == "true");
        assert!(ok, "{expr}: {:?}, want {want}", row[0]);
    }
}

#[test]
fn xpath_functions_match_arq() {
    let s = Store::in_memory(StoreOptions::default());
    for (expr, want) in [
        // rounding to a precision (F&O 3.1)
        ("fn:round-half-to-even(2.5)", "2.0"),
        ("fn:round-half-to-even(3.5)", "4.0"),
        ("fn:round-half-to-even(3.567812e0, 2)", "3.57e0"),
        ("fn:round-half-to-even(1250, -2)", "1200"),
        (
            "fn:round-half-to-even(\"0.125\"^^xsd:float, 2)",
            "\"0.12\"^^xsd:float",
        ),
        ("fn:round(-2.5)", "-2.0"),
        ("fn:round(3.145, 2)", "3.15"),
        ("fn:round(1.125e0, 2)", "1.13e0"),
        ("fn:round(-1.125e0, 2)", "-1.12e0"),
        ("fn:round(12345.6, -2)", "12300.0"),
        ("fn:round-half-to-even(2.5, 1.0)", ""),
        // op:numeric-mod and op:numeric-integer-divide
        ("fn:numeric-mod(-7, 2)", "-1"),
        ("fn:numeric-mod(7.5, 2)", "1.5"),
        ("fn:numeric-mod(7.5e0, 2)", "1.5e0"),
        ("fn:numeric-mod(1, 0)", ""),
        ("fn:numeric-integer-divide(7, -2)", "-3"),
        ("fn:numeric-integer-divide(-7.5, 2)", "-3"),
        ("fn:numeric-integer-divide(10.5e0, 3)", "3"),
        ("fn:numeric-integer-divide(1, 0)", ""),
        // durations: F&O normalizes the components and gives them the duration's sign.
        // ARQ returns the fields as written, unsigned: 1, 14, 0, 90 and 3 for the first
        // five here
        ("fn:years-from-duration(\"P1Y14M\"^^xsd:duration)", "2"),
        ("fn:months-from-duration(\"P1Y14M\"^^xsd:duration)", "2"),
        ("fn:hours-from-duration(\"-PT90M\"^^xsd:duration)", "-1"),
        ("fn:minutes-from-duration(\"PT90M\"^^xsd:duration)", "30"),
        ("fn:days-from-duration(\"P3DT25H\"^^xsd:duration)", "4"),
        (
            "fn:seconds-from-duration(\"PT1M3.5S\"^^xsd:dayTimeDuration)",
            "3.5",
        ),
        (
            "fn:years-from-duration(\"P2Y\"^^xsd:yearMonthDuration)",
            "2",
        ),
        ("fn:days-from-duration(\"2024-01-01\"^^xsd:date)", ""),
        // dates and times
        (
            "fn:dateTime(\"2024-02-29\"^^xsd:date, \"10:30:00+02:00\"^^xsd:time)",
            "\"2024-02-29T10:30:00+02:00\"^^xsd:dateTime",
        ),
        (
            "fn:dateTime(\"2024-02-29Z\"^^xsd:date, \"10:30:00+02:00\"^^xsd:time)",
            "",
        ),
        (
            "fn:adjust-dateTime-to-timezone(\"2024-01-01T10:00:00+02:00\"^^xsd:dateTime, \"-PT5H\"^^xsd:dayTimeDuration)",
            "\"2024-01-01T03:00:00-05:00\"^^xsd:dateTime",
        ),
        (
            "fn:adjust-dateTime-to-timezone(\"2024-01-01T10:00:00+02:00\"^^xsd:dateTime)",
            "\"2024-01-01T08:00:00Z\"^^xsd:dateTime",
        ),
        (
            "fn:adjust-date-to-timezone(\"2024-01-01\"^^xsd:date, \"PT3H\"^^xsd:dayTimeDuration)",
            "\"2024-01-01+03:00\"^^xsd:date",
        ),
        (
            "fn:adjust-time-to-timezone(\"10:00:00Z\"^^xsd:time, \"\")",
            "\"10:00:00\"^^xsd:time",
        ),
        (
            "afn:adjust-to-timezone(\"2024-01-01T10:00:00Z\"^^xsd:dateTime, \"PT1H\"^^xsd:dayTimeDuration)",
            "\"2024-01-01T11:00:00+01:00\"^^xsd:dateTime",
        ),
        (
            "fn:timezone-from-date(\"2024-01-01-05:00\"^^xsd:date)",
            "\"-PT5H\"^^xsd:dayTimeDuration",
        ),
        (
            "fn:timezone-from-time(\"10:00:00Z\"^^xsd:time)",
            "\"PT0S\"^^xsd:dayTimeDuration",
        ),
        ("fn:years-from-date(\"2024-03-04\"^^xsd:date)", "2024"),
        (
            "fn:days-from-dateTime(\"2024-03-04T00:00:00\"^^xsd:dateTime)",
            "4",
        ),
        ("fn:implicit-timezone()", "\"PT0S\"^^xsd:dayTimeDuration"),
        ("afn:timezone()", "\"PT0S\"^^xsd:dayTimeDuration"),
        (
            "afn:date(\"2024-05-06\")",
            "\"2024-05-06T00:00:00Z\"^^xsd:dateTime",
        ),
        ("afn:date(\"2024-5-6\")", ""),
        // strings
        ("fn:normalize-unicode(\"e\\u0301\")", "\"\\u00e9\""),
        ("STRLEN(fn:normalize-unicode(\"\\u00e9\", \"NFD\"))", "2"),
        ("fn:normalize-unicode(\"x\", \"fully-normalized\")", ""),
        ("afn:strlen(\"abc\")", "3"),
        ("afn:substr(\"hello\", 1, 3)", "\"el\""),
        ("afn:substring(\"hello\", 2)", "\"llo\""),
        ("afn:substr(\"hello\", 4, 9)", ""),
        (
            "afn:sha1sum(<http://ex/>)",
            "\"1a58a59c5c912cb5ec143eaf2d41d4e2958ef37b\"",
        ),
        ("afn:langeq(\"chat\"@fr-ca, \"fr\")", "true"),
        ("afn:langeq(\"chat\"@en, \"fr\")", "false"),
        // casts to the derived integer types check their ranges
        ("xsd:byte(300)", ""),
        ("xsd:byte(\"12\")", "\"12\"^^xsd:byte"),
        ("xsd:int(3000000000)", ""),
        ("xsd:nonNegativeInteger(-1)", ""),
        ("xsd:positiveInteger(\"5\")", "\"5\"^^xsd:positiveInteger"),
        ("xsd:unsignedShort(70000)", ""),
        ("xsd:negativeInteger(-2.7)", "\"-2\"^^xsd:negativeInteger"),
        (
            "xsd:unsignedLong(\"9223372036854775807\")",
            "\"9223372036854775807\"^^xsd:unsignedLong",
        ),
        // ARQ has no xsd:unsignedByte cast
        ("xsd:unsignedByte(255)", "\"255\"^^xsd:unsignedByte"),
        ("xsd:unsignedByte(256)", ""),
        // other casts
        ("xsd:anyURI(\"http://x/\")", "\"http://x/\"^^xsd:anyURI"),
        ("xsd:gYear(\"2024\")", "\"2024\"^^xsd:gYear"),
        ("xsd:gYear(\"2024-05-06\"^^xsd:date)", "\"2024\"^^xsd:gYear"),
        (
            "xsd:gYearMonth(\"2024-05-06\"^^xsd:date)",
            "\"2024-05\"^^xsd:gYearMonth",
        ),
        // F&O 3.1 §19.1.5 keeps the timezone, which ARQ drops ("--05-06")
        (
            "xsd:gMonthDay(\"2024-05-06T10:00:00Z\"^^xsd:dateTime)",
            "\"--05-06Z\"^^xsd:gMonthDay",
        ),
        (
            "xsd:gMonth(\"2024-05-06\"^^xsd:date)",
            "\"--05\"^^xsd:gMonth",
        ),
        ("xsd:gDay(\"---05\")", "\"---05\"^^xsd:gDay"),
        // numbers and errors
        ("afn:evenInteger(4)", "true"),
        ("afn:evenInteger(-3)", "false"),
        ("afn:evenInteger(4.0)", ""),
        ("fn:error()", ""),
        ("fn:error(\"stop\")", ""),
    ] {
        check(&s, expr, want);
    }
}

#[test]
fn uuids_are_fresh() {
    let s = Store::in_memory(StoreOptions::default());
    let q = format!(
        "{PREFIXES}SELECT (isIRI(afn:uuid()) AS ?a) (STRLEN(afn:struuid()) AS ?b) \
         (afn:struuid() != afn:struuid() AS ?c) WHERE {{}}"
    );
    let rows = query(s.snapshot(), &q, &QueryOptions::default())
        .unwrap()
        .rows();
    let v: Vec<String> = rows[0]
        .iter()
        .map(|t| match t {
            Some(Term::Literal(l)) => l.value().to_string(),
            t => format!("{t:?}"),
        })
        .collect();
    assert_eq!(v, ["true", "36", "true"]);
}
