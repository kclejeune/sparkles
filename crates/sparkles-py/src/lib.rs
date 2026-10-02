//! The `sparkles._sparkles` extension module: Python bindings for the Sparkles engine
//! (spec `docs/specs/P01-python-bindings.md`). `python/sparkles/__init__.py` re-exports
//! it and defines the exception classes.

mod dataset;
mod errors;
mod io;
mod results;
mod terms;
mod txn;
mod validate;

use pyo3::prelude::*;
use pyo3::types::PyFrozenSet;

/// The cargo features of this build.
const FEATURES: &[(&str, bool)] = &[
    ("reasoning", cfg!(feature = "reasoning")),
    ("shacl", cfg!(feature = "shacl")),
    ("shex", cfg!(feature = "shex")),
    ("text", cfg!(feature = "text")),
    ("geo", cfg!(feature = "geo")),
    ("zstd", true),
    ("brotli", true),
];

#[pymodule(gil_used = true)]
fn _sparkles(m: &Bound<'_, PyModule>) -> PyResult<()> {
    terms::register(m)?;
    io::register(m)?;
    results::register(m)?;
    validate::register(m)?;
    m.add_class::<dataset::PyDataset>()?;
    m.add_class::<txn::PyTransaction>()?;
    m.add("INFERRED_GRAPH", dataset::INFERRED_GRAPH)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    let features: Vec<&str> = FEATURES
        .iter()
        .filter(|(_, on)| *on)
        .map(|(f, _)| *f)
        .collect();
    m.add("FEATURES", PyFrozenSet::new(m.py(), &features)?)?;
    Ok(())
}
