//! With the `fault-injection` feature (enabled for the crate's tests through its
//! dev-dependency on itself), `SPARKLES_FMT_FAULT=drop-token` makes the printer drop a
//! token, and the safety checks refuse the output. Its own test binary: the variable is
//! process-wide.

use sparkles_fmt::{Check, FormatError, Language, Options, format};

#[test]
fn a_dropped_token_is_refused() {
    // SAFETY: the only test of this binary, so no other thread reads the environment
    unsafe { std::env::set_var("SPARKLES_FMT_FAULT", "drop-token") };
    let e = format(
        "SELECT ?s WHERE { ?s ?p ?o }",
        Language::Sparql,
        &Options::default(),
    )
    .unwrap_err();
    assert_eq!(
        e,
        FormatError::Unsafe {
            check: Check::Algebra
        }
    );
    assert_eq!(e.code(), "unsafe-format");
}
