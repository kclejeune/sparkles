//! Tool profiles (C33, item 489): named subsets of the tools, which `sparkles mcp
//! --profile` and `serve --mcp-profile` select, together with single tools named by
//! `--tools` and `--mcp-tools`. A profile only narrows what the server offers: the
//! write switches, the read-only server, the caller's grants and `--disable-tool` still
//! apply to every tool it names.

use std::collections::BTreeSet;

/// A named set of tools.
pub struct Profile {
    pub name: &'static str,
    pub tools: &'static [&'static str],
}

/// Every profile, in the order the help and the errors list them. A tool that this
/// build lacks, such as `search_text` without the `text` feature, is skipped.
pub const PROFILES: &[Profile] = &[
    Profile {
        name: "explore",
        tools: &[
            "list_datasets",
            "describe_schema",
            "draft_shapes",
            "diff_schema",
            "sparql_query",
            "explain_query",
            "describe_resource",
            "find_paths",
            "graph_algorithm",
            "list_commits",
            "list_changes",
            "search_text",
            "search_semantic",
            "similar_entities",
            "check_query",
            "similar_queries",
            "community_reports",
            "why_empty",
            "share_query",
            "explain_inference",
            "graphql_query",
        ],
    },
    Profile {
        name: "memory",
        tools: &[
            "list_datasets",
            "describe_schema",
            "sparql_query",
            "describe_resource",
            "search_text",
            "search_semantic",
            "similar_entities",
            "check_query",
            "similar_queries",
            "link_entities",
            "recall",
            "community_reports",
            "read_chunks",
            "list_sources",
            "ingest_profile",
            "assert_facts",
            "register_source",
            "propose_claims",
            "withdraw_claims",
            "resolve_evidence",
            "find_evidence_dependents",
        ],
    },
    Profile {
        name: "admin",
        tools: &[
            "list_datasets",
            "describe_schema",
            "draft_shapes",
            "diff_schema",
            "sparql_query",
            "explain_query",
            "list_commits",
            "list_changes",
            "validate_shacl",
            "validate_shex",
            "format",
            "lint",
            "ontology_axioms",
            "entailment_diff",
            "review_claims",
            "resolve_identities",
            "sparql_update",
            "list_branches",
            "create_branch",
            "merge_branch",
            "delete_branch",
        ],
    },
];

/// The profile that offers every tool, the default.
pub const ALL: &str = "all";

/// The profile names, `all` first.
pub fn names() -> Vec<&'static str> {
    std::iter::once(ALL)
        .chain(PROFILES.iter().map(|p| p.name))
        .collect()
}

/// The tools that `profiles` and `tools` select together, or `None` when they select
/// every tool (neither is given, or a profile is `all`). `flag` names the flags in the
/// errors, which refuse an unknown profile or tool and list the valid names.
pub fn select(
    profiles: &[String],
    tools: &[String],
    (profile_flag, tools_flag): (&str, &str),
) -> anyhow::Result<Option<BTreeSet<String>>> {
    let known = super::schemas::all_tools();
    let mut out = BTreeSet::new();
    let mut every = profiles.is_empty() && tools.is_empty();
    for p in profiles {
        if p == ALL {
            every = true;
            continue;
        }
        let Some(profile) = PROFILES.iter().find(|q| q.name == p) else {
            anyhow::bail!(
                "{profile_flag}: unknown profile '{p}' (profiles: {})",
                names().join(", ")
            );
        };
        out.extend(
            profile
                .tools
                .iter()
                .filter(|t| known.contains(t))
                .map(|t| t.to_string()),
        );
    }
    for t in tools {
        if !known.contains(&t.as_str()) {
            anyhow::bail!(
                "{tools_flag}: unknown tool '{t}' (tools: {})",
                known.join(", ")
            );
        }
        out.insert(t.clone());
    }
    Ok((!every).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every tool of the build belongs to a profile, so a new tool cannot be left out
    /// of all of them by accident, and every name a profile lists is a tool of some
    /// build.
    #[test]
    fn profiles_cover_every_tool() {
        for t in super::super::schemas::all_tools() {
            assert!(
                PROFILES.iter().any(|p| p.tools.contains(&t)),
                "{t} is in no profile"
            );
        }
        // the names of tools that a feature can leave out
        let optional = [
            "search_text",
            "explain_inference",
            "entailment_diff",
            "validate_shacl",
            "validate_shex",
            "format",
            "lint",
            "graphql_query",
            "ontology_axioms",
        ];
        let known = super::super::schemas::all_tools();
        for p in PROFILES {
            for t in p.tools {
                assert!(
                    known.contains(t) || optional.contains(t),
                    "{}: unknown tool {t}",
                    p.name
                );
            }
        }
    }

    #[test]
    fn select_combines_and_refuses() {
        let s = |p: &[&str], t: &[&str]| {
            select(
                &p.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                &t.iter().map(|x| x.to_string()).collect::<Vec<_>>(),
                ("--profile", "--tools"),
            )
        };
        assert!(s(&[], &[]).unwrap().is_none());
        assert!(s(&["all"], &["recall"]).unwrap().is_none());
        let only = s(&["memory"], &["find_paths"]).unwrap().unwrap();
        assert!(only.contains("recall") && only.contains("find_paths"));
        assert!(!only.contains("sparql_update"));
        let e = s(&["nope"], &[]).unwrap_err().to_string();
        assert_eq!(
            e,
            "--profile: unknown profile 'nope' (profiles: all, explore, memory, admin)"
        );
        let e = s(&[], &["nope"]).unwrap_err().to_string();
        assert!(
            e.starts_with("--tools: unknown tool 'nope' (tools: list_datasets, "),
            "{e}"
        );
    }
}
