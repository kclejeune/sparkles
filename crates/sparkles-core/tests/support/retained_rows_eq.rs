// Test-only comparisons preserve row order, width, values and multiplicity while
// borrowing payloads from the original retained owners.
macro_rules! assert_retained_rows_eq {
    ($actual:expr, $expected:expr $(, $message:expr)* $(,)?) => {{
        let actual = &$actual;
        let expected = &$expected;
        assert_eq!(actual.len(), expected.len() $(, $message)*);
        for row in 0..actual.len() {
            assert_eq!(actual.row(row).unwrap().values(), expected.row(row).unwrap().values() $(, $message)*);
        }
    }};
}
