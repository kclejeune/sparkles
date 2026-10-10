//! The `sparkles._sparkles` extension module: Python bindings for the Sparkles engine
//! (spec `docs/specs/P01-python-bindings.md`). `python/sparkles/__init__.py` re-exports
//! it and defines the exception classes.

/// The library's own allocations go to mimalloc when the feature is on. The host runtime keeps
/// its own heap, so this changes only the native side's allocations.
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod admin;
mod backups;
mod branches;
mod catalog;
mod dataset;
mod errors;
mod graphql;
mod handles;
mod indexes;
mod interrupt;
mod io;
mod queries;
mod query_cursor;
mod querybuilder;
mod rdflib_nodes;
mod reasoning;
mod results;
mod schema;
mod terms;
mod txn;
mod utilities;
mod validate;
mod validation;

use pyo3::prelude::*;
use pyo3::types::PyFrozenSet;

/// The cargo features of this build.
const FEATURES: &[(&str, bool)] = &[
    ("reasoning", cfg!(feature = "reasoning")),
    ("shacl", cfg!(feature = "shacl")),
    ("shex", cfg!(feature = "shex")),
    ("text", cfg!(feature = "text")),
    ("geo", cfg!(feature = "geo")),
    ("backup", cfg!(feature = "backup")),
    ("graphql", cfg!(feature = "graphql")),
    ("zstd", true),
    ("brotli", true),
];

#[pymodule(gil_used = true)]
fn _sparkles(m: &Bound<'_, PyModule>) -> PyResult<()> {
    terms::register(m)?;
    utilities::register(m)?;
    io::register(m)?;
    results::register(m)?;
    query_cursor::register(m)?;
    schema::register(m)?;
    validate::register(m)?;
    validation::register(m)?;
    admin::register(m)?;
    branches::register(m)?;
    #[cfg(feature = "backup")]
    backups::register(m)?;
    catalog::register(m)?;
    handles::register(m)?;
    graphql::register(m)?;
    indexes::register(m)?;
    reasoning::register(m)?;
    querybuilder::register(m)?;
    queries::register(m)?;
    rdflib_nodes::register(m)?;
    m.add_class::<dataset::PyDataset>()?;
    m.add_class::<txn::PyTransaction>()?;
    m.add_class::<interrupt::PyCancelToken>()?;
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
