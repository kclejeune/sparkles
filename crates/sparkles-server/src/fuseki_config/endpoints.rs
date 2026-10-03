//! Fuseki's operations and the endpoint names Sparkles serves them at. The assembler
//! bodies of `POST /$/datasets` and the configuration converter share this table.

/// The names (`""` is the dataset URL itself) Sparkles serves a standard Fuseki
/// operation at, or `None` for an operation Sparkles does not serve. The direct Graph
/// Store operations are served at the dataset URL with `--gsp-direct-naming`.
pub fn served_names(op: &str) -> Option<&'static [&'static str]> {
    Some(match op {
        "query" => &["", "sparql", "query"],
        "update" => &["", "update"],
        "gsp-rw" | "gsp_rw" => &["", "data"],
        "gsp-r" | "gsp_r" => &["", "get", "data"],
        "upload" => &["upload"],
        "patch" => &["", "patch"],
        "prefixes-r" | "prefixes-rw" => &["prefixes"],
        "shacl" if cfg!(feature = "shacl") => &["shacl"],
        "no-op" | "no_op" => &[
            "", "sparql", "query", "update", "data", "get", "upload", "shacl", "prefixes", "patch",
        ],
        "gsp-direct-rw" | "gsp-direct-r" => &[""],
        _ => return None,
    })
}

/// Operations that change the data.
pub fn is_write(op: &str) -> bool {
    matches!(
        op,
        "update" | "gsp-rw" | "gsp_rw" | "upload" | "gsp-direct-rw" | "patch" | "prefixes-rw"
    )
}

/// The endpoint name of a grant (spec C12) that covers an operation.
pub fn grant_endpoint(op: &str) -> Option<&'static str> {
    Some(match op {
        "query" => "query",
        "update" => "update",
        "gsp-r" | "gsp_r" | "gsp-direct-r" => "gsp-r",
        "gsp-rw" | "gsp_rw" | "gsp-direct-rw" => "gsp-rw",
        "upload" => "upload",
        "patch" => "patch",
        "shacl" => "shacl",
        "prefixes-r" | "prefixes-rw" => "info",
        _ => return None,
    })
}

/// The canonical spelling of an operation (`gsp_r` → `gsp-r`).
pub fn canonical(op: &str) -> &str {
    match op {
        "gsp_r" => "gsp-r",
        "gsp_rw" => "gsp-rw",
        "no_op" => "no-op",
        o => o,
    }
}
