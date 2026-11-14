macro_rules! assert_fixture_rows_eq {
    ($actual:expr, $expected:expr $(, $message:expr)* $(,)?) => {{
        let actual = &$actual;
        let expected = &$expected;
        assert_eq!(actual.len(), expected.len() $(, $message)*);
        for (row, expected_row) in expected.iter().enumerate() {
            assert_eq!(actual.row(row).unwrap().values(), expected_row.as_slice() $(, $message)*);
        }
    }};
}
