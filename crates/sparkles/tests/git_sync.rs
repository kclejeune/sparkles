//! Integration tests for F25 Phase 1 (versioned canonical git serialization profile) and
//! Phase 2 (git export of a branch).

use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Term, Triple};
use sparkles::Dataset;
use sparkles::branch::{BranchOptions, MergeOptions};
use sparkles::git::{
    CANONICALIZATION_ALGORITHM, CanonicalGraphKind, CanonicalSyntax, CommitMapping, GitError,
    GitExportMode, GitExportOptions, MANIFEST_FORMAT_VERSION, MANIFEST_PROFILE_NAME,
    MANIFEST_REL_PATH, MAPPING_FORMAT_VERSION, ReadOptions, WriteOptions, collect_dataset_quads,
    dataset_matches_tree, datasets_isomorphic, default_mapping_path, import_git_tree,
    import_git_tree_with, read_git_tree, read_git_tree_with, try_datasets_isomorphic,
    verify_git_tree, verify_git_tree_with, write_git_tree,
};
use sparkles::guard::WriteOptions as StoreWriteOptions;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

fn nn(iri: &str) -> NamedNode {
    NamedNode::new(iri).unwrap()
}

fn sample_prefixes() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("ex".to_string(), "http://example.org/".to_string()),
        (
            "rdfs".to_string(),
            "http://www.w3.org/2000/01/rdf-schema#".to_string(),
        ),
        ("schema".to_string(), "http://schema.org/".to_string()),
    ])
}

/// Build a semantically rich dataset with:
/// - default graph + two named graphs + a blank-node-named graph
/// - blank nodes shared across the default graph and named graphs
/// - a blank node cycle and chain
/// - typed, language-tagged, and RDF 1.2 directional literals
/// - an RDF 1.2 triple term
fn sample_quads(b1: &str, b2: &str, b3: &str, bg: &str, reverse_order: bool) -> Vec<Quad> {
    let n1 = BlankNode::new(b1).unwrap();
    let n2 = BlankNode::new(b2).unwrap();
    let n3 = BlankNode::new(b3).unwrap();
    let ng = BlankNode::new(bg).unwrap();

    let rdf_type = nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let alice = nn("http://example.org/alice");
    let bob = nn("http://example.org/bob");
    let person = nn("http://example.org/Person");
    let knows = nn("http://example.org/knows");
    let label = nn("http://www.w3.org/2000/01/rdf-schema#label");
    let note = nn("http://example.org/note");
    let age = nn("http://example.org/age");
    let next = nn("http://example.org/next");
    let edge = nn("http://example.org/edge");
    let g1 = GraphName::NamedNode(nn("http://example.org/graphs/people"));
    let g2 = GraphName::NamedNode(nn("http://example.org/graphs/links"));
    let g_blank = GraphName::BlankNode(ng.clone());

    let triple_term = Triple::new(alice.clone(), knows.clone(), bob.clone());

    let mut quads = vec![
        // Default graph
        Quad::new(
            alice.clone(),
            rdf_type.clone(),
            person.clone(),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            label.clone(),
            Literal::new_language_tagged_literal("Alice", "en").unwrap(),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            age.clone(),
            Literal::from(37i64),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            knows.clone(),
            n1.clone(),
            GraphName::DefaultGraph,
        ),
        // Blank-node graph identity referenced in default graph
        Quad::new(
            ng.clone(),
            label.clone(),
            Literal::from("Blank graph metadata"),
            GraphName::DefaultGraph,
        ),
        // RDF 1.2 triple term + directional language literal
        Quad::new(
            bob.clone(),
            note.clone(),
            triple_term,
            GraphName::DefaultGraph,
        ),
        Quad::new(
            bob.clone(),
            label.clone(),
            Literal::new_directional_language_tagged_literal(
                "مرحبا",
                "ar",
                oxrdf::BaseDirection::Rtl,
            )
            .unwrap(),
            GraphName::DefaultGraph,
        ),
        // Named graph 1: shares blank node n1 with default graph and chains to n2 -> n3 -> n1
        Quad::new(n1.clone(), next.clone(), n2.clone(), g1.clone()),
        Quad::new(n2.clone(), next.clone(), n3.clone(), g1.clone()),
        Quad::new(n3.clone(), next.clone(), n1.clone(), g1.clone()),
        Quad::new(
            n2.clone(),
            label.clone(),
            Literal::from("Cycle node 2"),
            g1.clone(),
        ),
        // Named graph 2: shares n1 and n3 across graphs
        Quad::new(n1.clone(), edge.clone(), n3.clone(), g2.clone()),
        Quad::new(bob.clone(), rdf_type, person, g2),
        // Blank-node-named graph
        Quad::new(n3, edge, alice, g_blank),
    ];

    if reverse_order {
        quads.reverse();
    }
    quads
}

fn dir_snapshot_bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<_> = fs::read_dir(cur).unwrap().map(|e| e.unwrap()).collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let p = entry.path();
            if p.file_name().and_then(|s| s.to_str()) == Some(".git") {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&p).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

fn git_output(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn canonical_serialization_is_byte_deterministic_across_blank_node_renames_and_order() {
    for syntax in [CanonicalSyntax::Turtle, CanonicalSyntax::NQuads] {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let prefixes = sample_prefixes();

        let opts = WriteOptions {
            syntax,
            ..WriteOptions::default()
        };

        let m_a = write_git_tree(
            sample_quads("alpha", "beta", "gamma", "gnode", false),
            &prefixes,
            dir_a.path(),
            &opts,
        )
        .unwrap();
        let m_b = write_git_tree(
            sample_quads("z99", "z01", "z42", "gother", true),
            &prefixes,
            dir_b.path(),
            &opts,
        )
        .unwrap();

        assert_eq!(m_a.format_version, MANIFEST_FORMAT_VERSION);
        assert_eq!(m_a.profile, MANIFEST_PROFILE_NAME);
        assert_eq!(m_a.canonicalization, CANONICALIZATION_ALGORITHM);
        assert_eq!(m_a, m_b, "manifests must match for {syntax:?}");
        assert_eq!(
            dir_snapshot_bytes(dir_a.path()),
            dir_snapshot_bytes(dir_b.path()),
            "serialized directory bytes must be identical for {syntax:?}"
        );
    }
}

#[test]
fn blank_node_isomorphism_round_trip_through_dataset_import() {
    for syntax in [CanonicalSyntax::Turtle, CanonicalSyntax::NQuads] {
        let src_dir = tempfile::tempdir().unwrap();
        let tree_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        let src = Dataset::open(src_dir.path().join("src")).unwrap();
        for (p, iri) in sample_prefixes() {
            src.set_prefix(&p, &iri).unwrap();
        }
        let initial_quads = sample_quads("b_a", "b_b", "b_c", "b_g", false);
        src.transaction(|tx| {
            for q in &initial_quads {
                tx.insert(q.as_ref())?;
            }
            Ok(())
        })
        .unwrap();

        let manifest = src
            .write_git_tree(
                tree_dir.path(),
                &WriteOptions {
                    syntax,
                    ..WriteOptions::default()
                },
            )
            .unwrap();
        assert_eq!(manifest.graphs.len(), 4);
        assert_eq!(manifest.graphs[0].kind, CanonicalGraphKind::Default);
        assert_eq!(manifest.graphs[1].kind, CanonicalGraphKind::Named);
        assert_eq!(manifest.graphs[2].kind, CanonicalGraphKind::Named);
        assert_eq!(manifest.graphs[3].kind, CanonicalGraphKind::Blank);

        // Read back the canonical tree and verify isomorphism with source dataset.
        let tree = read_git_tree(tree_dir.path()).unwrap();
        assert_eq!(tree.quads.len(), initial_quads.len());
        assert!(
            dataset_matches_tree(&src, &tree).unwrap(),
            "read tree must be isomorphic to source dataset for {syntax:?}"
        );

        // Import into a brand new dataset and verify both quad isomorphism and prefixes.
        let dst = Dataset::open(dst_dir.path().join("dst")).unwrap();
        import_git_tree(&dst, tree_dir.path()).unwrap();

        let src_quads = collect_dataset_quads(&src).unwrap();
        let dst_quads = collect_dataset_quads(&dst).unwrap();
        assert_eq!(src_quads.len(), dst_quads.len());
        assert!(
            datasets_isomorphic(src_quads, dst_quads),
            "imported dataset must preserve exact quads and cross-graph blank-node isomorphism for {syntax:?}"
        );
        for (p, iri) in sample_prefixes() {
            assert_eq!(dst.prefixes().get(&p), Some(&iri));
        }

        // Re-exporting the imported dataset must reproduce byte-for-byte identical files!
        let reexport_dir = tempfile::tempdir().unwrap();
        dst.write_git_tree(
            reexport_dir.path(),
            &WriteOptions {
                syntax,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            dir_snapshot_bytes(tree_dir.path()),
            dir_snapshot_bytes(reexport_dir.path()),
            "round-trip re-export must be byte-for-byte identical for {syntax:?}"
        );
    }
}

#[test]
fn refuses_unknown_manifest_and_mapping_format_versions() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join(".sparkles-git")).unwrap();

    for bad_version in [0u32, 2, 7, 999] {
        // Deliberately omit v1 fields to verify version refusal fires before schema parsing.
        let manifest = format!(r#"{{"format_version": {bad_version}, "new_v2_schema": true}}"#);
        fs::write(dir.path().join(MANIFEST_REL_PATH), manifest).unwrap();
        let err = read_git_tree(dir.path()).unwrap_err();
        assert!(
            matches!(
                err,
                GitError::UnsupportedFormatVersion {
                    found,
                    supported,
                    ..
                } if found == bad_version && supported == MANIFEST_FORMAT_VERSION
            ),
            "expected UnsupportedFormatVersion({bad_version}), got {err:?}"
        );

        let map_path = dir.path().join("mapping.json");
        let mapping = format!(r#"{{"format_version": {bad_version}, "entries": []}}"#);
        fs::write(&map_path, mapping).unwrap();
        let map_err = CommitMapping::read(&map_path).unwrap_err();
        assert!(
            matches!(
                map_err,
                GitError::UnsupportedFormatVersion {
                    found,
                    supported,
                    ..
                } if found == bad_version && supported == MAPPING_FORMAT_VERSION
            ),
            "expected mapping UnsupportedFormatVersion({bad_version}), got {map_err:?}"
        );
    }
}

#[test]
fn refuses_path_traversal_symlinks_and_malformed_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::memory();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.write_git_tree(dir.path(), &WriteOptions::default())
        .unwrap();

    // 1. Path traversal in manifest entry
    for bad_path in [
        "../escape.ttl",
        "graphs/../../escape.ttl",
        "/etc/passwd",
        "graphs\\default.ttl",
        "outside/default.ttl",
    ] {
        let mut raw: serde_json::Value =
            serde_json::from_slice(&fs::read(dir.path().join(MANIFEST_REL_PATH)).unwrap()).unwrap();
        raw["graphs"][0]["path"] = serde_json::Value::String(bad_path.to_string());
        let bad_dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(bad_dir.path().join(".sparkles-git")).unwrap();
        fs::write(
            bad_dir.path().join(MANIFEST_REL_PATH),
            serde_json::to_vec(&raw).unwrap(),
        )
        .unwrap();
        let err = read_git_tree(bad_dir.path()).unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "expected UnsafePath for {bad_path:?}, got {err:?}"
        );
    }

    // 2. Symlink escape inside graphs/
    #[cfg(unix)]
    {
        let sym_dir = tempfile::tempdir().unwrap();
        ds.write_git_tree(sym_dir.path(), &WriteOptions::default())
            .unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("outside.ttl");
        let original = fs::read(sym_dir.path().join("graphs/default.ttl")).unwrap();
        fs::write(&outside_file, &original).unwrap();
        fs::remove_file(sym_dir.path().join("graphs/default.ttl")).unwrap();
        std::os::unix::fs::symlink(&outside_file, sym_dir.path().join("graphs/default.ttl"))
            .unwrap();

        let err = read_git_tree(sym_dir.path()).unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "expected UnsafePath for symlink graph file, got {err:?}"
        );
    }

    // 3. Checksum mismatch rejected when verify_checksums = true, tolerated when false
    let tamper_dir = tempfile::tempdir().unwrap();
    ds.write_git_tree(tamper_dir.path(), &WriteOptions::default())
        .unwrap();
    fs::write(
        tamper_dir.path().join("graphs/default.ttl"),
        "<http://example.org/s> <http://example.org/p> 2 .\n",
    )
    .unwrap();
    let err = read_git_tree(tamper_dir.path()).unwrap_err();
    assert!(matches!(err, GitError::InvalidManifest(_)));
    let relaxed = read_git_tree_with(
        tamper_dir.path(),
        &ReadOptions {
            verify_checksums: false,
            ..ReadOptions::default()
        },
    )
    .unwrap();
    assert_eq!(relaxed.quads.len(), 1);
}

#[test]
fn per_commit_and_squashed_git_export_preserve_metadata_trailers_and_mapping() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.set_prefix("ex", "http://example.org/").unwrap();

    // Commit 1 on main (with author and message)
    let (_, r1) = ds
        .transaction_receipt_with(
            StoreWriteOptions {
                message: Some(Arc::from("Initial ontology terms")),
                author: Some(Arc::from("Alice Ontologist <alice@example.org>")),
                ..Default::default()
            },
            |tx| {
                tx.insert(
                    Quad::new(
                        nn("http://example.org/ConceptA"),
                        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                        nn("http://example.org/Concept"),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                tx.insert(
                    Quad::new(
                        nn("http://example.org/temp"),
                        nn("http://example.org/p"),
                        Literal::from("ephemeral"),
                        GraphName::NamedNode(nn("http://example.org/graphs/temp")),
                    )
                    .as_ref(),
                )?;
                Ok(())
            },
        )
        .unwrap();

    // Commit 2 on main: remove named graph temp, add named graph vocab
    let (_, r2) = ds
        .transaction_receipt_with(
            StoreWriteOptions {
                message: Some(Arc::from("Promote vocabulary graph and drop temp")),
                author: Some(Arc::from("bob")),
                ..Default::default()
            },
            |tx| {
                tx.remove(
                    Quad::new(
                        nn("http://example.org/temp"),
                        nn("http://example.org/p"),
                        Literal::from("ephemeral"),
                        GraphName::NamedNode(nn("http://example.org/graphs/temp")),
                    )
                    .as_ref(),
                )?;
                tx.insert(
                    Quad::new(
                        nn("http://example.org/ConceptB"),
                        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                        nn("http://example.org/Concept"),
                        GraphName::NamedNode(nn("http://example.org/graphs/vocab")),
                    )
                    .as_ref(),
                )?;
                Ok(())
            },
        )
        .unwrap();

    // Create a feature branch `feature-vocab` and add commit 3
    ds.create_branch("feature-vocab", &BranchOptions::default())
        .unwrap();
    let feat = ds.branch("feature-vocab").unwrap();
    let (_, r3) = feat
        .transaction_receipt_with(
            StoreWriteOptions {
                message: Some(Arc::from("Add ConceptC on feature branch")),
                author: Some(Arc::from("carol@example.org")),
                ..Default::default()
            },
            |tx| {
                tx.insert(
                    Quad::new(
                        nn("http://example.org/ConceptC"),
                        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                        nn("http://example.org/Concept"),
                        GraphName::NamedNode(nn("http://example.org/graphs/vocab")),
                    )
                    .as_ref(),
                )?;
                Ok(())
            },
        )
        .unwrap();

    // Flush change logs so authors are guaranteed visible on disk before export.
    ds.store().flush_change_log().unwrap();
    feat.store().flush_change_log().unwrap();

    // --- 1. Per-commit export of `feature-vocab` (replays commits 1, 2, 3) ---
    let repo_commits = tempfile::tempdir().unwrap();
    let opts = GitExportOptions {
        branch: Some("feature-vocab".to_string()),
        mode: GitExportMode::Commits,
        syntax: CanonicalSyntax::Turtle,
        extra_trailers: vec![("Reviewed-By".into(), "Ontology Bot".into())],
        ..GitExportOptions::default()
    };
    let report = ds.export_git(repo_commits.path(), &opts).unwrap();
    assert_eq!(report.commits_exported, 3);
    assert_eq!(report.commits_skipped, 0);
    assert_eq!(report.git_branch, "feature-vocab");
    assert_eq!(report.mapping.format_version, MAPPING_FORMAT_VERSION);
    assert_eq!(report.mapping.entries.len(), 3);

    // Check bidirectional mapping lookups and persistence.
    let map_path = default_mapping_path(repo_commits.path(), "feature-vocab").unwrap();
    assert_eq!(report.mapping_path, map_path);
    let loaded_map = CommitMapping::read(&map_path).unwrap();
    assert_eq!(loaded_map, report.mapping);

    let sha1 = loaded_map
        .git_sha_for_seq(r1.commit.seq)
        .unwrap()
        .to_string();
    let sha2 = loaded_map
        .git_sha_for_seq(r2.commit.seq)
        .unwrap()
        .to_string();
    let sha3 = loaded_map
        .git_sha_for_seq(r3.commit.seq)
        .unwrap()
        .to_string();
    assert_eq!(
        loaded_map.sparkles_commit(&sha1).unwrap().sparkles_seq,
        r1.commit.seq
    );
    assert_eq!(
        loaded_map.sparkles_commit(&sha2).unwrap().sparkles_seq,
        r2.commit.seq
    );
    assert_eq!(
        loaded_map.sparkles_commit(&sha3).unwrap().sparkles_seq,
        r3.commit.seq
    );
    assert_eq!(report.head_git_sha, sha3);

    // Check git log commit count, authors, timestamps, messages, and trailers.
    let log_shas: Vec<String> = git_output(repo_commits.path(), &["rev-list", "--reverse", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(log_shas, vec![sha1.clone(), sha2.clone(), sha3.clone()]);

    let show1 = git_output(
        repo_commits.path(),
        &["show", "-s", "--format=%an|%ae|%at%n%B", &sha1],
    );
    assert!(show1.contains("Alice Ontologist|alice@example.org|"));
    assert!(show1.contains("Initial ontology terms"));
    assert!(show1.contains(&format!("Sparkles-Dataset-Id: {}", ds.dataset_id())));
    assert!(show1.contains("Sparkles-Branch: feature-vocab"));
    assert!(show1.contains(&format!("Sparkles-Commit: {}", r1.commit.seq)));
    assert!(show1.contains("Sparkles-Export-Mode: commits"));
    assert!(show1.contains("Reviewed-By: Ontology Bot"));

    let show2 = git_output(
        repo_commits.path(),
        &["show", "-s", "--format=%an|%ae%n%B", &sha2],
    );
    assert!(show2.contains("bob|bob@sparkles.local"));
    assert!(show2.contains("Promote vocabulary graph and drop temp"));

    let show3 = git_output(
        repo_commits.path(),
        &["show", "-s", "--format=%an|%ae%n%B", &sha3],
    );
    assert!(show3.contains("carol@example.org|carol@example.org"));
    assert!(show3.contains("Add ConceptC on feature branch"));

    // Verify commit 2 removed the deleted temp graph file from the git tree.
    let tree1_files = git_output(
        repo_commits.path(),
        &["ls-tree", "-r", "--name-only", &sha1],
    );
    let tree2_files = git_output(
        repo_commits.path(),
        &["ls-tree", "-r", "--name-only", &sha2],
    );
    assert!(
        tree1_files.contains("temp-"),
        "commit 1 should contain temp graph: {tree1_files}"
    );
    assert!(
        !tree2_files.contains("temp-"),
        "commit 2 should remove temp graph: {tree2_files}"
    );
    assert!(
        tree2_files.contains("vocab-"),
        "commit 2 should add vocab graph: {tree2_files}"
    );

    // Working tree at HEAD must match the feature branch dataset and have clean git status.
    let status = git_output(repo_commits.path(), &["status", "--porcelain"]);
    assert!(
        status.trim().is_empty(),
        "expected clean git status, got: {status}"
    );
    let head_tree = read_git_tree(repo_commits.path()).unwrap();
    assert!(dataset_matches_tree(&feat, &head_tree).unwrap());

    // Deterministic SHA reproducibility: exporting into a second fresh repo yields exact same SHAs!
    let repo_commits_2 = tempfile::tempdir().unwrap();
    let report_2 = ds.export_git(repo_commits_2.path(), &opts).unwrap();
    assert_eq!(report_2.mapping.entries, report.mapping.entries);

    // Incremental continuation: commit 4 on feature-vocab appends only 1 new commit and keeps shas 1..=3.
    let (_, r4) = feat
        .transaction_receipt_with(
            StoreWriteOptions {
                message: Some(Arc::from("Add ConceptD incrementally")),
                ..Default::default()
            },
            |tx| {
                tx.insert(
                    Quad::new(
                        nn("http://example.org/ConceptD"),
                        nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                        nn("http://example.org/Concept"),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                Ok(())
            },
        )
        .unwrap();
    let incr = ds.export_git(repo_commits.path(), &opts).unwrap();
    assert_eq!(incr.commits_skipped, 3);
    assert_eq!(incr.commits_exported, 1);
    assert_eq!(incr.mapping.entries.len(), 4);
    assert_eq!(
        incr.mapping.git_sha_for_seq(r1.commit.seq),
        Some(sha1.as_str())
    );
    assert_eq!(
        incr.mapping.git_sha_for_seq(r2.commit.seq),
        Some(sha2.as_str())
    );
    assert_eq!(
        incr.mapping.git_sha_for_seq(r3.commit.seq),
        Some(sha3.as_str())
    );
    assert!(incr.mapping.git_sha_for_seq(r4.commit.seq).is_some());

    // --- 2. Squashed export of `feature-vocab` (in N-Quads) ---
    let repo_squash = tempfile::tempdir().unwrap();
    let squash_report = ds
        .export_git(
            repo_squash.path(),
            &GitExportOptions {
                branch: Some("feature-vocab".to_string()),
                git_branch: Some("release/v1".to_string()),
                mode: GitExportMode::Squash,
                syntax: CanonicalSyntax::NQuads,
                squash_message: Some("Squashed vocabulary release v1".to_string()),
                ..GitExportOptions::default()
            },
        )
        .unwrap();
    assert_eq!(squash_report.commits_exported, 1);
    assert_eq!(squash_report.git_branch, "release/v1");
    assert_eq!(squash_report.mapping.entries.len(), 1);
    let entry = &squash_report.mapping.entries[0];
    assert_eq!(entry.mode, GitExportMode::Squash);
    assert_eq!(entry.sparkles_seq, r4.commit.seq);
    assert_eq!(entry.squashed_from, Some(r1.commit.seq));

    let squash_shas: Vec<String> = git_output(repo_squash.path(), &["rev-list", "HEAD"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(squash_shas.len(), 1);
    assert_eq!(squash_shas[0], entry.git_sha);

    let squash_msg = git_output(repo_squash.path(), &["show", "-s", "--format=%B", "HEAD"]);
    assert!(squash_msg.contains("Squashed vocabulary release v1"));
    assert!(squash_msg.contains("Sparkles-Export-Mode: squash"));
    assert!(squash_msg.contains(&format!("Sparkles-Squashed-From: {}", r1.commit.seq)));
    assert!(squash_msg.contains(&format!("Sparkles-Commit: {}", r4.commit.seq)));

    let squash_tree = read_git_tree(repo_squash.path()).unwrap();
    assert_eq!(squash_tree.manifest.syntax, CanonicalSyntax::NQuads);
    assert!(dataset_matches_tree(&feat, &squash_tree).unwrap());
}

// ================================================================================
// Round 1 regression tests (F1–F12)
// ================================================================================

#[test]
fn golden_canonical_turtle_is_byte_identical_across_features() {
    let dir = tempfile::tempdir().unwrap();
    let prefixes = BTreeMap::from([
        ("ex".to_string(), "http://example.org/".to_string()),
        (
            "xsd".to_string(),
            "http://www.w3.org/2001/XMLSchema#".to_string(),
        ),
    ]);
    let rdf_type = nn("http://www.w3.org/1999/02/22-rdf-syntax-ns#type");
    let long_s = nn("http://example.org/veryLongSubjectIdentifierForCrossFeatureDeterminismCheck");
    let long_p =
        nn("http://example.org/veryLongPredicateIdentifierExceedingTypicalLineWidthLimits");
    let long_o = Literal::from(
        "A long literal value that would wrap under width-sensitive formatters but must remain byte-identical across builds",
    );
    let alice = nn("http://example.org/alice");
    let tag = nn("http://example.org/tag");
    let age = nn("http://example.org/age");
    let raw_int = nn("http://example.org/rawInt");
    let active = nn("http://example.org/active");

    let quads = vec![
        Quad::new(long_s, long_p, long_o, GraphName::DefaultGraph),
        Quad::new(
            alice.clone(),
            rdf_type,
            nn("http://example.org/Person"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            age,
            Literal::from(37i64),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            raw_int,
            Literal::new_typed_literal("+01", nn("http://www.w3.org/2001/XMLSchema#integer")),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            active,
            Literal::from(true),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            tag.clone(),
            Literal::from("alpha"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            alice.clone(),
            tag.clone(),
            Literal::from("beta"),
            GraphName::DefaultGraph,
        ),
        Quad::new(alice, tag, Literal::from("gamma"), GraphName::DefaultGraph),
    ];

    let manifest = write_git_tree(quads, &prefixes, dir.path(), &WriteOptions::default()).unwrap();
    let actual = fs::read_to_string(dir.path().join("graphs/default.ttl")).unwrap();

    // Pinned canonical sparkles-git-v1 Turtle bytes: must match identically both with
    // default features (`fmt` disabled) and with `--features fmt`.
    let expected = concat!(
        "PREFIX ex: <http://example.org/>\n",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>\n",
        "\n",
        "ex:alice\n",
        "  a ex:Person ;\n",
        "  ex:active true ;\n",
        "  ex:age 37 ;\n",
        "  ex:rawInt \"+01\"^^xsd:integer ;\n",
        "  ex:tag\n",
        "    \"alpha\" ,\n",
        "    \"beta\" ,\n",
        "    \"gamma\" ;\n",
        ".\n",
        "\n",
        "ex:veryLongSubjectIdentifierForCrossFeatureDeterminismCheck ex:veryLongPredicateIdentifierExceedingTypicalLineWidthLimits \"A long literal value that would wrap under width-sensitive formatters but must remain byte-identical across builds\" .\n",
    );
    assert_eq!(actual, expected);
    assert_eq!(manifest.graphs.len(), 1);
    assert_eq!(manifest.graphs[0].quads, 8);
    verify_git_tree(dir.path()).unwrap();
}

#[cfg(unix)]
#[test]
fn write_and_read_reject_symlinks_and_fifos_without_touching_external_files() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    let victim_ttl = outside.path().join("victim.ttl");
    let victim_tmp = outside.path().join("victim.tmp");
    let victim_manifest = outside.path().join("victim_manifest.json");
    fs::write(&victim_ttl, b"DO NOT DELETE").unwrap();
    fs::write(&victim_tmp, b"DO NOT OVERWRITE TMP").unwrap();
    fs::write(&victim_manifest, b"DO NOT OVERWRITE MANIFEST").unwrap();

    // 1. Symlinked `graphs/` with EMPTY dataset must not prune external `.ttl` files.
    let dir_empty = tempfile::tempdir().unwrap();
    symlink(outside.path(), dir_empty.path().join("graphs")).unwrap();
    let err = write_git_tree(
        Vec::<Quad>::new(),
        &BTreeMap::new(),
        dir_empty.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");
    assert_eq!(fs::read(&victim_ttl).unwrap(), b"DO NOT DELETE");

    // 2. Symlinked `.sparkles-git/` directory rejected before writing manifest.
    let dir_meta = tempfile::tempdir().unwrap();
    symlink(outside.path(), dir_meta.path().join(".sparkles-git")).unwrap();
    let err = write_git_tree(
        Vec::<Quad>::new(),
        &BTreeMap::new(),
        dir_meta.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");

    // 3. Symlinked `.sparkles-git/manifest.json` rejected without touching target.
    let dir_mf = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir_mf.path().join(".sparkles-git")).unwrap();
    symlink(&victim_manifest, dir_mf.path().join(MANIFEST_REL_PATH)).unwrap();
    let err = write_git_tree(
        Vec::<Quad>::new(),
        &BTreeMap::new(),
        dir_mf.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");
    assert_eq!(
        fs::read(&victim_manifest).unwrap(),
        b"DO NOT OVERWRITE MANIFEST"
    );

    // 4. Pre-existing predictable `.tmp` symlink is NEVER followed (exclusive temp files).
    let dir_tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir_tmp.path().join("graphs")).unwrap();
    fs::create_dir_all(dir_tmp.path().join(".sparkles-git")).unwrap();
    symlink(&victim_tmp, dir_tmp.path().join("graphs/default.tmp")).unwrap();
    symlink(
        &victim_tmp,
        dir_tmp.path().join(".sparkles-git/manifest.tmp"),
    )
    .unwrap();
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q],
        &BTreeMap::new(),
        dir_tmp.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    assert_eq!(fs::read(&victim_tmp).unwrap(), b"DO NOT OVERWRITE TMP");

    // 5. Symlinked root directory rejected on read/verify.
    let real_tree = tempfile::tempdir().unwrap();
    write_git_tree(
        Vec::<Quad>::new(),
        &BTreeMap::new(),
        real_tree.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    let link_parent = tempfile::tempdir().unwrap();
    let root_link = link_parent.path().join("linked-root");
    symlink(real_tree.path(), &root_link).unwrap();
    let err = verify_git_tree(&root_link).unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");

    // 6. FIFO under `graphs/default.ttl` rejected immediately without blocking.
    let fifo_dir = tempfile::tempdir().unwrap();
    let q2 = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(2i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q2],
        &BTreeMap::new(),
        fifo_dir.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    let ttl_path = fifo_dir.path().join("graphs/default.ttl");
    fs::remove_file(&ttl_path).unwrap();
    let mkfifo_status = Command::new("mkfifo").arg(&ttl_path).status().unwrap();
    assert!(mkfifo_status.success());
    let err = verify_git_tree(fifo_dir.path()).unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");
}

#[test]
fn export_never_hijacks_enclosing_repo_commits_staged_files_or_touches_unmanaged_files() {
    let outer = tempfile::tempdir().unwrap();
    // Create an enclosing Git repository with a commit and a staged change.
    assert!(
        Command::new("git")
            .args([
                "-C",
                outer.path().to_str().unwrap(),
                "init",
                "-q",
                "-b",
                "main"
            ])
            .status()
            .unwrap()
            .success()
    );
    fs::write(outer.path().join("README.md"), "outer repo\n").unwrap();
    assert!(
        Command::new("git")
            .args(["-C", outer.path().to_str().unwrap(), "add", "README.md"])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("git")
            .args([
                "-C",
                outer.path().to_str().unwrap(),
                "-c",
                "user.name=Outer",
                "-c",
                "user.email=outer@example.org",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "outer initial",
            ])
            .status()
            .unwrap()
            .success()
    );
    let outer_head_before = git_output(outer.path(), &["rev-parse", "HEAD"]);
    fs::write(outer.path().join("STAGED.txt"), "do not commit me\n").unwrap();
    assert!(
        Command::new("git")
            .args(["-C", outer.path().to_str().unwrap(), "add", "STAGED.txt"])
            .status()
            .unwrap()
            .success()
    );

    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    // 1. Export into a nested subdirectory of `outer`: must initialize its own repo and
    //    leave `outer` completely untouched.
    let nested_out = outer.path().join("exported-ontology");
    let report = ds
        .export_git(&nested_out, &GitExportOptions::default())
        .unwrap();
    assert_eq!(report.commits_exported, 1);
    assert!(nested_out.join(".git").is_dir());
    assert_eq!(
        git_output(outer.path(), &["rev-parse", "HEAD"]),
        outer_head_before
    );
    assert_eq!(
        git_output(outer.path(), &["diff", "--cached", "--name-only"]).trim(),
        "STAGED.txt"
    );

    // 2. Dirty index inside the target repo itself must be refused cleanly.
    fs::write(nested_out.join("unrelated.txt"), "staged\n").unwrap();
    assert!(
        Command::new("git")
            .args(["-C", nested_out.to_str().unwrap(), "add", "unrelated.txt"])
            .status()
            .unwrap()
            .success()
    );
    let dirty_err = ds
        .export_git(&nested_out, &GitExportOptions::default())
        .unwrap_err();
    assert!(
        dirty_err.to_string().contains("pre-existing staged"),
        "expected dirty index refusal, got: {dirty_err}"
    );
    // Unstage unrelated.txt and add unmanaged files under `graphs/` and `.sparkles-git/`.
    assert!(
        Command::new("git")
            .args([
                "-C",
                nested_out.to_str().unwrap(),
                "reset",
                "-q",
                "HEAD",
                "--",
                "unrelated.txt"
            ])
            .status()
            .unwrap()
            .success()
    );
    fs::write(nested_out.join("graphs/unmanaged-notes.ttl"), "# keep me\n").unwrap();
    fs::write(nested_out.join(".sparkles-git/notes.txt"), "keep me\n").unwrap();

    // Add a failing pre-commit hook and a `.gitattributes` CRLF rule; neither may break
    // export or alter committed blob bytes.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let hook = nested_out.join(".git/hooks/pre-commit");
        fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        let mut perms = fs::metadata(&hook).unwrap().permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&hook, perms).unwrap();
    }
    fs::write(nested_out.join(".gitattributes"), "*.ttl text eol=crlf\n").unwrap();

    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 2 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let report2 = ds
        .export_git(&nested_out, &GitExportOptions::default())
        .unwrap();
    assert_eq!(report2.commits_exported, 1);

    // Unmanaged files were neither deleted nor committed.
    assert!(nested_out.join("graphs/unmanaged-notes.ttl").is_file());
    assert!(nested_out.join(".sparkles-git/notes.txt").is_file());
    let committed_files = git_output(&nested_out, &["ls-tree", "-r", "--name-only", "HEAD"]);
    assert!(!committed_files.contains("unmanaged-notes.ttl"));
    assert!(!committed_files.contains("notes.txt"));
    assert!(!committed_files.contains("unrelated.txt"));

    // Committed blob bytes for graphs/default.ttl must be LF-exact (no CRLF filter mutation).
    let blob = Command::new("git")
        .args([
            "-C",
            nested_out.to_str().unwrap(),
            "cat-file",
            "-p",
            "HEAD:graphs/default.ttl",
        ])
        .output()
        .unwrap();
    assert!(blob.status.success());
    assert_eq!(
        blob.stdout,
        fs::read(nested_out.join("graphs/default.ttl")).unwrap()
    );
    assert!(!blob.stdout.contains(&b'\r'));
}

#[test]
fn branch_handle_export_matches_owner_export_and_preserves_merge_provenance() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.set_prefix("ex", "http://example.org/").unwrap();

    // Commit 1 on main (no manual flush_change_log: author must still be recovered!).
    ds.transaction_receipt_with(
        StoreWriteOptions {
            message: Some(Arc::from("Base commit on main")),
            author: Some(Arc::from("Alice <alice@example.org>")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/a"),
                    nn("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();

    ds.create_branch("side", &BranchOptions::default()).unwrap();
    let side = ds.branch("side").unwrap();
    side.transaction_receipt_with(
        StoreWriteOptions {
            message: Some(Arc::from("Side branch addition")),
            author: Some(Arc::from("Bob <bob@example.org>")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/b"),
                    nn("http://example.org/p"),
                    Literal::from(2i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();

    // Branch `feat` from `main`, add commit on `feat`, then merge `side` into `feat`.
    ds.create_branch("feat", &BranchOptions::default()).unwrap();
    let feat = ds.branch("feat").unwrap();
    feat.transaction_receipt_with(
        StoreWriteOptions {
            message: Some(Arc::from("Feat own commit")),
            author: Some(Arc::from("Carol <carol@example.org>")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/c"),
                    nn("http://example.org/p"),
                    Literal::from(3i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();

    let merge_outcome = ds.merge("side", "feat", &MergeOptions::default()).unwrap();
    assert!(matches!(
        merge_outcome,
        sparkles::branch::MergeOutcome::Merged(_)
    ));

    // Export via owning dataset with `branch: Some("feat")` AND directly via `feat.export_git`.
    let repo_owner = tempfile::tempdir().unwrap();
    let repo_handle = tempfile::tempdir().unwrap();
    let r_owner = ds
        .export_git(
            repo_owner.path(),
            &GitExportOptions {
                branch: Some("feat".to_string()),
                ..GitExportOptions::default()
            },
        )
        .unwrap();
    let r_handle = feat
        .export_git(repo_handle.path(), &GitExportOptions::default())
        .unwrap();

    assert_eq!(r_owner.commits_exported, 3);
    assert_eq!(r_handle.commits_exported, 3);
    assert_eq!(r_handle.mapping.entries, r_owner.mapping.entries);
    assert_eq!(r_handle.head_git_sha, r_owner.head_git_sha);

    // Check inherited commit 1 recovered Alice's author WITHOUT manual flush_change_log,
    // and check merge commit 3 carries `Sparkles-Merged-From`.
    let show1 = git_output(
        repo_handle.path(),
        &[
            "show",
            "-s",
            "--format=%an|%ae%n%B",
            &r_handle.mapping.entries[0].git_sha,
        ],
    );
    assert!(show1.contains("Alice|alice@example.org"), "{show1}");
    assert!(show1.contains("Sparkles-Origin-Commit: main@1"), "{show1}");

    let show3 = git_output(
        repo_handle.path(),
        &[
            "show",
            "-s",
            "--format=%B",
            &r_handle.mapping.entries[2].git_sha,
        ],
    );
    assert!(
        show3.contains("Sparkles-Merged-From: side@2"),
        "expected Sparkles-Merged-From trailer, got: {show3}"
    );
}

#[test]
fn multi_branch_export_and_continuation_guards_prevent_history_duplication() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.create_branch("feat", &BranchOptions::default()).unwrap();
    let feat = ds.branch("feat").unwrap();
    feat.load_str(
        "PREFIX ex: <http://example.org/> ex:f ex:p 2 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    let repo = tempfile::tempdir().unwrap();

    // 1. Export `main` then `feat` into the SAME repo.
    let r_main1 = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(r_main1.commits_exported, 1);

    let r_feat1 = ds
        .export_git(
            repo.path(),
            &GitExportOptions {
                branch: Some("feat".into()),
                ..GitExportOptions::default()
            },
        )
        .unwrap();
    assert_eq!(r_feat1.commits_exported, 2);
    assert_ne!(r_main1.mapping_path, r_feat1.mapping_path);

    // `feat` must have exactly 2 commits (clean orphan, NOT 1 main + 2 duplicated!).
    let feat_shas: Vec<String> = git_output(repo.path(), &["rev-list", "refs/heads/feat"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(feat_shas.len(), 2);

    // 2. Add commit 2 to `main` and re-export `main`: must preserve `main`'s mapping and
    //    append ONLY 1 new commit (not re-commit commit 1!).
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 3 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let r_main2 = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(r_main2.commits_skipped, 1);
    assert_eq!(r_main2.commits_exported, 1);
    assert_eq!(r_main2.mapping.entries[0].git_sha, r_main1.head_git_sha);

    // 3. Syntax mismatch on continuation must be refused instead of silently skipping.
    let syntax_err = ds
        .export_git(
            repo.path(),
            &GitExportOptions {
                syntax: CanonicalSyntax::NQuads,
                ..GitExportOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        syntax_err.to_string().contains("syntax"),
        "expected syntax mismatch refusal, got: {syntax_err}"
    );

    // 4. Shortened range (`to_seq: Some(1)`) or shifted `from_seq: Some(2)` must be refused.
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                to_seq: Some(1),
                ..GitExportOptions::default()
            },
        )
        .is_err()
    );
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                from_seq: Some(2),
                ..GitExportOptions::default()
            },
        )
        .is_err()
    );

    // 5. Mixing Squash onto a Commits-mapped branch must be refused.
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                mode: GitExportMode::Squash,
                ..GitExportOptions::default()
            },
        )
        .is_err()
    );

    // 6. Advancing Git HEAD manually outside Sparkles must be refused on next export.
    assert!(
        Command::new("git")
            .args([
                "-C",
                repo.path().to_str().unwrap(),
                "-c",
                "user.name=External",
                "-c",
                "user.email=ext@example.org",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "external commit",
            ])
            .status()
            .unwrap()
            .success()
    );
    let diverged_err = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        diverged_err.to_string().contains("does not match"),
        "expected diverged HEAD refusal, got: {diverged_err}"
    );
}

#[test]
fn import_git_tree_does_not_mutate_prefixes_on_dry_run_or_failed_write() {
    let tree_dir = tempfile::tempdir().unwrap();
    let prefixes = BTreeMap::from([("fresh".to_string(), "http://example.org/fresh/".to_string())]);
    let q = Quad::new(
        nn("http://example.org/fresh/s"),
        nn("http://example.org/fresh/p"),
        Literal::from("v"),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q],
        &prefixes,
        tree_dir.path(),
        &WriteOptions::default(),
    )
    .unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();

    // 1. Dry run must not mutate persistent prefixes or quads.
    let dry_err = import_git_tree_with(
        &ds,
        tree_dir.path(),
        &ReadOptions::default(),
        StoreWriteOptions {
            dry_run: Some(sparkles::preview::DryRun::default()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(dry_err, GitError::Store(sparkles::Error::DryRun(_))),
        "expected DryRun error, got: {dry_err:?}"
    );
    assert!(
        !ds.prefixes().contains_key("fresh"),
        "dry_run must not add prefixes"
    );
    assert!(collect_dataset_quads(&ds).unwrap().is_empty());

    // 2. Rejected write (protected branch) must not mutate prefixes.
    ds.set_branch_protected("main", true).unwrap();
    let err = import_git_tree(&ds, tree_dir.path()).unwrap_err();
    assert!(matches!(err, GitError::Store(_)), "{err:?}");
    assert!(
        !ds.prefixes().contains_key("fresh"),
        "failed import must not add prefixes"
    );

    // 3. Unprotected real import succeeds and adds both quads and prefixes.
    ds.set_branch_protected("main", false).unwrap();
    let receipt = import_git_tree(&ds, tree_dir.path()).unwrap();
    assert!(receipt.committed);
    assert_eq!(
        ds.prefixes().get("fresh").map(String::as_str),
        Some("http://example.org/fresh/")
    );
    assert_eq!(collect_dataset_quads(&ds).unwrap().len(), 1);
}

#[test]
fn composite_literal_blank_node_links_round_trip_losslessly_and_deterministically() {
    let cdt_list = nn("http://w3id.org/awslabs/neptune/SPARQL-CDTs/List");
    let cdt_map = nn("http://w3id.org/awslabs/neptune/SPARQL-CDTs/Map");
    let g1 = GraphName::NamedNode(nn("http://example.org/g1"));

    let build_quads = |bn_label: &str| {
        let bn = BlankNode::new(bn_label).unwrap();
        vec![
            Quad::new(
                nn("http://example.org/s"),
                nn("http://example.org/ref"),
                bn.clone(),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                bn,
                nn("http://example.org/name"),
                Literal::from("shared-node"),
                GraphName::DefaultGraph,
            ),
            Quad::new(
                nn("http://example.org/holder"),
                nn("http://example.org/list"),
                Literal::new_typed_literal(format!("[_:{bn_label}, 42]"), cdt_list.clone()),
                g1.clone(),
            ),
            Quad::new(
                nn("http://example.org/holder"),
                nn("http://example.org/map"),
                Literal::new_typed_literal(format!("{{\"node\" : _:{bn_label}}}"), cdt_map.clone()),
                g1.clone(),
            ),
        ]
    };

    for syntax in [CanonicalSyntax::Turtle, CanonicalSyntax::NQuads] {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let opts = WriteOptions {
            syntax,
            ..WriteOptions::default()
        };
        write_git_tree(
            build_quads("localAlpha"),
            &BTreeMap::new(),
            dir_a.path(),
            &opts,
        )
        .unwrap();
        write_git_tree(
            build_quads("localZeta99"),
            &BTreeMap::new(),
            dir_b.path(),
            &opts,
        )
        .unwrap();

        // Must not leak source-local blank node labels into canonical bytes!
        assert_eq!(
            dir_snapshot_bytes(dir_a.path()),
            dir_snapshot_bytes(dir_b.path()),
            "CDT blank-node export must be byte-identical across label renames ({syntax:?})"
        );
        for bytes in dir_snapshot_bytes(dir_a.path()).values() {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("localAlpha"), "leaked source label: {text}");
        }

        // Import into a Sparkles dataset and verify the blank node inside `cdt:List` and
        // `cdt:Map` in `ex:g1` resolves to the EXACT SAME stored blank node as the default
        // graph quad!
        let dst = Dataset::memory();
        import_git_tree(&dst, dir_a.path()).unwrap();
        let linked = dst
            .ask(
                "PREFIX cdt: <http://w3id.org/awslabs/neptune/SPARQL-CDTs/>
                 ASK {
                   <http://example.org/s> <http://example.org/ref> ?b .
                   ?b <http://example.org/name> \"shared-node\" .
                   GRAPH <http://example.org/g1> {
                     <http://example.org/holder> <http://example.org/list> ?l ;
                                                 <http://example.org/map> ?m .
                   }
                   FILTER(cdt:head(?l) = ?b)
                   FILTER(cdt:get(?m, \"node\") = ?b)
                 }",
            )
            .unwrap();
        assert!(
            linked,
            "cross-graph CDT blank-node link broke on round-trip ({syntax:?})"
        );
    }
}

#[test]
fn resource_bounds_and_canonicalization_budget_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();

    // 1. Exceeding WriteOptions::max_quads fails cleanly.
    let quads: Vec<Quad> = (0..5)
        .map(|i| {
            Quad::new(
                nn(&format!("http://example.org/s{i}")),
                nn("http://example.org/p"),
                Literal::from(i as i64),
                GraphName::DefaultGraph,
            )
        })
        .collect();
    let err = write_git_tree(
        quads.clone(),
        &BTreeMap::new(),
        dir.path(),
        &WriteOptions {
            max_quads: 3,
            ..WriteOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, GitError::Format(_)), "{err:?}");

    // 2. Symmetric blank-node poison graph (K_10 clique) fails immediately in preflight
    //    instead of allocating 10! permutations in oxrdf.
    let mut poison = Vec::new();
    let edge = nn("http://example.org/edge");
    for i in 0..10 {
        for j in 0..10 {
            if i != j {
                poison.push(Quad::new(
                    BlankNode::new(format!("k{i}")).unwrap(),
                    edge.clone(),
                    BlankNode::new(format!("k{j}")).unwrap(),
                    GraphName::DefaultGraph,
                ));
            }
        }
    }
    let poison_dir = tempfile::tempdir().unwrap();
    let err = write_git_tree(
        poison,
        &BTreeMap::new(),
        poison_dir.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, GitError::Format(ref m) if m.contains("budget exceeded")),
        "expected canonicalization budget refusal, got: {err:?}"
    );

    // 3. Oversized manifest / graph file / max_quads fail cleanly on verify/read.
    write_git_tree(
        quads,
        &BTreeMap::new(),
        dir.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    assert!(
        verify_git_tree_with(
            dir.path(),
            &ReadOptions {
                max_manifest_bytes: 16,
                ..ReadOptions::default()
            }
        )
        .is_err()
    );
    assert!(
        verify_git_tree_with(
            dir.path(),
            &ReadOptions {
                max_graph_file_bytes: 16,
                ..ReadOptions::default()
            }
        )
        .is_err()
    );
    assert!(
        verify_git_tree_with(
            dir.path(),
            &ReadOptions {
                max_quads: 2,
                ..ReadOptions::default()
            }
        )
        .is_err()
    );
}

#[test]
fn strict_verify_rejects_layout_violations_and_range_and_metadata_edge_cases() {
    let dir = tempfile::tempdir().unwrap();
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q],
        &BTreeMap::new(),
        dir.path(),
        &WriteOptions::default(),
    )
    .unwrap();

    // 1. Unlisted file inside `graphs/` rejected by strict verify.
    fs::write(
        dir.path().join("graphs/stray.ttl"),
        "<http://e/a> <http://e/b> 1 .\n",
    )
    .unwrap();
    assert!(verify_git_tree(dir.path()).is_err());
    fs::remove_file(dir.path().join("graphs/stray.ttl")).unwrap();
    assert!(verify_git_tree(dir.path()).is_ok());

    // 2. Duplicate quad inside a graph file rejected even when checksums are disabled.
    fs::write(
        dir.path().join("graphs/default.ttl"),
        "<http://example.org/s> <http://example.org/p> 1 .\n<http://example.org/s> <http://example.org/p> 1 .\n",
    )
    .unwrap();
    let dup_err = verify_git_tree_with(
        dir.path(),
        &ReadOptions {
            verify_checksums: false,
            strict_layout: true,
            ..ReadOptions::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(dup_err, GitError::InvalidManifest(ref m) if m.contains("duplicate quad")),
        "{dup_err:?}"
    );

    // 3. Export range validation (from > to, to > head) and edge-case commit messages/authors.
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.transaction_receipt_with(
        StoreWriteOptions {
            message: Some(Arc::from("   \n  ")),
            author: Some(Arc::from("<<<>>>")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/s"),
                    nn("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();
    ds.transaction_receipt_with(
        StoreWriteOptions {
            message: Some(Arc::from("# leading hash subject")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/s"),
                    nn("http://example.org/p"),
                    Literal::from(2i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();

    let repo = tempfile::tempdir().unwrap();
    // Invalid bounds rejected cleanly.
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                from_seq: Some(2),
                to_seq: Some(1),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                to_seq: Some(99),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );
    // Reserved Sparkles-* extra trailer rejected.
    assert!(
        ds.export_git(
            repo.path(),
            &GitExportOptions {
                extra_trailers: vec![("Sparkles-Commit".into(), "999".into())],
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );

    // Valid export succeeds with fallback subject for whitespace message, sanitized author,
    // and verbatim `# leading hash subject`.
    let report = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(report.commits_exported, 2);
    let msg1 = git_output(
        repo.path(),
        &[
            "show",
            "-s",
            "--format=%s",
            &report.mapping.entries[0].git_sha,
        ],
    );
    assert!(
        msg1.trim().starts_with("Sparkles commit 1"),
        "whitespace-only message must fall back to synthetic subject, got: {msg1:?}"
    );
    let msg2 = git_output(
        repo.path(),
        &[
            "show",
            "-s",
            "--format=%s",
            &report.mapping.entries[1].git_sha,
        ],
    );
    assert_eq!(msg2.trim(), "# leading hash subject");
}

// ================================================================================
// Round 2 regression tests (R2-1 through R2-16)
// ================================================================================

#[cfg(unix)]
#[test]
fn root_symlink_aliases_with_trailing_slash_or_dot_are_rejected_everywhere() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("sentinel.txt");
    fs::write(&sentinel, b"UNTOUCHED").unwrap();
    let outside_tree = tempfile::tempdir().unwrap();
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q.clone()],
        &BTreeMap::new(),
        outside_tree.path(),
        &WriteOptions::default(),
    )
    .unwrap();

    let holder = tempfile::tempdir().unwrap();
    let link_write = holder.path().join("link-write");
    symlink(outside.path(), &link_write).unwrap();
    let link_read = holder.path().join("link-read");
    symlink(outside_tree.path(), &link_read).unwrap();

    // Create a real subdir inside `outside` to test `link/sub/..` alias too.
    fs::create_dir(outside.path().join("sub")).unwrap();

    for alias in [
        format!("{}/", link_write.display()),
        format!("{}/.", link_write.display()),
        format!("{}/./", link_write.display()),
        format!("{}/sub/..", link_write.display()),
        format!("{}/new-child", link_write.display()),
    ] {
        let p = std::path::PathBuf::from(&alias);
        let err = write_git_tree(
            vec![q.clone()],
            &BTreeMap::new(),
            &p,
            &WriteOptions::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "write_git_tree must reject symlink alias {alias:?}, got {err:?}"
        );
    }
    assert!(!outside.path().join(".sparkles-git").exists());
    assert!(!outside.path().join("graphs").exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"UNTOUCHED");

    for alias in [
        format!("{}/", link_read.display()),
        format!("{}/.", link_read.display()),
    ] {
        let err = verify_git_tree(Path::new(&alias)).unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "verify_git_tree must reject symlink alias {alias:?}, got {err:?}"
        );
    }

    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    for alias in [
        format!("{}/", link_write.display()),
        format!("{}/.", link_write.display()),
    ] {
        let err = ds
            .export_git(Path::new(&alias), &GitExportOptions::default())
            .unwrap_err();
        assert!(
            err.to_string().contains("symlink") || err.to_string().contains("unsafe path"),
            "export_git must reject symlink alias {alias:?}, got {err}"
        );
    }
    assert!(!outside.path().join(".git").exists());
    assert_eq!(fs::read(&sentinel).unwrap(), b"UNTOUCHED");
}

#[test]
fn write_and_export_refuse_unmanaged_collisions_noncanonical_prior_and_unsupported_manifest() {
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );

    // 1. Fresh directory with unmanaged `graphs/default.ttl` must refuse overwrite.
    let dir_unmanaged = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir_unmanaged.path().join("graphs")).unwrap();
    fs::write(
        dir_unmanaged.path().join("graphs/default.ttl"),
        "# user-owned unmanaged file\n",
    )
    .unwrap();
    let err = write_git_tree(
        vec![q.clone()],
        &BTreeMap::new(),
        dir_unmanaged.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, GitError::UnsafePath { .. }), "{err:?}");
    assert_eq!(
        fs::read_to_string(dir_unmanaged.path().join("graphs/default.ttl")).unwrap(),
        "# user-owned unmanaged file\n"
    );
    assert!(!dir_unmanaged.path().join(".sparkles-git").exists());

    // 2. Git export into directory with unmanaged `graphs/default.ttl` must refuse without
    //    overwriting or committing it.
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let repo_unmanaged = tempfile::tempdir().unwrap();
    fs::create_dir_all(repo_unmanaged.path().join("graphs")).unwrap();
    fs::write(
        repo_unmanaged.path().join("graphs/default.ttl"),
        "# precious unmanaged\n",
    )
    .unwrap();
    assert!(
        ds.export_git(repo_unmanaged.path(), &GitExportOptions::default())
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(repo_unmanaged.path().join("graphs/default.ttl")).unwrap(),
        "# precious unmanaged\n"
    );

    // 3. Prior manifest naming a non-canonical path (`graphs/precious.ttl`) must be refused
    //    and must NOT prune `graphs/precious.ttl`.
    let dir_noncanon = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir_noncanon.path().join(".sparkles-git")).unwrap();
    fs::create_dir_all(dir_noncanon.path().join("graphs")).unwrap();
    fs::write(
        dir_noncanon.path().join("graphs/precious.ttl"),
        "# do not delete\n",
    )
    .unwrap();
    let crafted = serde_json::json!({
        "format_version": 1,
        "profile": MANIFEST_PROFILE_NAME,
        "syntax": "turtle",
        "canonicalization": CANONICALIZATION_ALGORITHM,
        "prefixes": {},
        "graphs": [{
            "kind": "default",
            "path": "graphs/precious.ttl",
            "quads": 1,
            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
        }]
    });
    fs::write(
        dir_noncanon.path().join(MANIFEST_REL_PATH),
        serde_json::to_vec_pretty(&crafted).unwrap(),
    )
    .unwrap();
    let err = write_git_tree(
        Vec::<Quad>::new(),
        &BTreeMap::new(),
        dir_noncanon.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, GitError::InvalidManifest(_)), "{err:?}");
    assert!(dir_noncanon.path().join("graphs/precious.ttl").is_file());

    // 4. Unsupported prior manifest version (`format_version: 99`) must be refused without
    //    overwriting it.
    let dir_future = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir_future.path().join(".sparkles-git")).unwrap();
    fs::write(
        dir_future.path().join(MANIFEST_REL_PATH),
        r#"{"format_version": 99}"#,
    )
    .unwrap();
    let err = write_git_tree(
        vec![q],
        &BTreeMap::new(),
        dir_future.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(
            err,
            GitError::UnsupportedFormatVersion {
                found: 99,
                supported: 1,
                ..
            }
        ),
        "expected UnsupportedFormatVersion(99), got {err:?}"
    );
    assert_eq!(
        fs::read_to_string(dir_future.path().join(MANIFEST_REL_PATH)).unwrap(),
        r#"{"format_version": 99}"#
    );
}

#[test]
fn cdt_null_encoding_never_collides_with_internal_namespace_iri() {
    let cdt_list = nn("http://w3id.org/awslabs/neptune/SPARQL-CDTs/List");
    let cdt_map = nn("http://w3id.org/awslabs/neptune/SPARQL-CDTs/Map");
    let tricky_iri = "urn:sparkles-internal:git-cdt-canon:null";
    let bn = BlankNode::new("shared").unwrap();

    let quads_with_iri = vec![
        Quad::new(
            nn("http://example.org/s"),
            nn("http://example.org/ref"),
            bn.clone(),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn("http://example.org/holder"),
            nn("http://example.org/list"),
            Literal::new_typed_literal(
                format!("[_:shared, <{tricky_iri}>, null]"),
                cdt_list.clone(),
            ),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn("http://example.org/holder"),
            nn("http://example.org/map"),
            Literal::new_typed_literal(
                format!("{{\"a\" : <{tricky_iri}>, \"b\" : null, \"c\" : _:shared}}"),
                cdt_map.clone(),
            ),
            GraphName::DefaultGraph,
        ),
    ];

    let quads_all_null = vec![
        Quad::new(
            nn("http://example.org/s"),
            nn("http://example.org/ref"),
            bn,
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn("http://example.org/holder"),
            nn("http://example.org/list"),
            Literal::new_typed_literal("[_:shared, null, null]", cdt_list.clone()),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn("http://example.org/holder"),
            nn("http://example.org/map"),
            Literal::new_typed_literal(
                "{\"a\" : null, \"b\" : null, \"c\" : _:shared}",
                cdt_map.clone(),
            ),
            GraphName::DefaultGraph,
        ),
    ];

    // Isomorphism helper must distinguish the tricky IRI from CDT null.
    assert!(
        !try_datasets_isomorphic(quads_with_iri.clone(), quads_all_null).unwrap(),
        "dataset with <urn:sparkles-internal:git-cdt-canon:null> must NOT be isomorphic to null"
    );

    for syntax in [CanonicalSyntax::Turtle, CanonicalSyntax::NQuads] {
        let dir = tempfile::tempdir().unwrap();
        write_git_tree(
            quads_with_iri.clone(),
            &BTreeMap::new(),
            dir.path(),
            &WriteOptions {
                syntax,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        let tree = read_git_tree(dir.path()).unwrap();

        // Inspect decoded literals directly (independent of `try_datasets_isomorphic`).
        let mut saw_list = false;
        let mut saw_map = false;
        for q in &tree.quads {
            if let Term::Literal(l) = &q.object {
                if l.datatype() == cdt_list.as_ref() {
                    assert!(
                        l.value().contains(&format!("<{tricky_iri}>")),
                        "list lost IRI in {syntax:?}: {}",
                        l.value()
                    );
                    assert!(
                        l.value().contains("null"),
                        "list lost null in {syntax:?}: {}",
                        l.value()
                    );
                    saw_list = true;
                } else if l.datatype() == cdt_map.as_ref() {
                    assert!(
                        l.value().contains(&format!("\"a\" : <{tricky_iri}>")),
                        "map lost IRI in {syntax:?}: {}",
                        l.value()
                    );
                    assert!(
                        l.value().contains("\"b\" : null"),
                        "map lost null in {syntax:?}: {}",
                        l.value()
                    );
                    saw_map = true;
                }
            }
        }
        assert!(saw_list && saw_map);
    }
}

#[test]
fn export_refuses_bare_repo_linked_worktree_in_progress_merge_and_head_branch() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    // 1. Bare repository must be refused cleanly without mutating it.
    let bare = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q", "--bare", bare.path().to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    let err = ds
        .export_git(bare.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(err.to_string().contains("bare"), "got: {err}");
    assert!(!bare.path().join(".git").exists());
    assert!(!bare.path().join(".sparkles-git").exists());
    assert!(!bare.path().join("graphs").exists());

    // 2. Linked worktree must be refused cleanly.
    let parent_repo = tempfile::tempdir().unwrap();
    ds.export_git(parent_repo.path(), &GitExportOptions::default())
        .unwrap();
    let wt_holder = tempfile::tempdir().unwrap();
    let linked_wt = wt_holder.path().join("linked");
    assert!(
        Command::new("git")
            .args([
                "-C",
                parent_repo.path().to_str().unwrap(),
                "worktree",
                "add",
                "-q",
                "-b",
                "wt-branch",
                linked_wt.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
    let wt_err = ds
        .export_git(
            &linked_wt,
            &GitExportOptions {
                git_branch: Some("wt-branch".into()),
                ..GitExportOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        wt_err.to_string().contains("worktree") || wt_err.to_string().contains("gitfile"),
        "expected linked worktree refusal, got: {wt_err}"
    );

    // 3. Clean-tree in-progress merge (`MERGE_HEAD` present with no diff) must be refused.
    let merge_repo = tempfile::tempdir().unwrap();
    let r1 = ds
        .export_git(merge_repo.path(), &GitExportOptions::default())
        .unwrap();
    // Simulate a clean-tree in-progress merge state (no staged/unstaged diff, MERGE_HEAD set).
    fs::write(
        merge_repo.path().join(".git/MERGE_HEAD"),
        format!("{}\n", r1.head_git_sha),
    )
    .unwrap();
    let merge_err = ds
        .export_git(merge_repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        merge_err.to_string().contains("MERGE_HEAD"),
        "expected MERGE_HEAD refusal, got: {merge_err}"
    );

    // 4. Reserved Git branch name `HEAD` rejected on both a fresh repo and an existing
    //    unborn repo on another branch.
    let fresh_head = tempfile::tempdir().unwrap();
    assert!(
        ds.export_git(
            fresh_head.path(),
            &GitExportOptions {
                git_branch: Some("HEAD".into()),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );
    let unborn_repo = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args([
                "init",
                "-q",
                "-b",
                "other",
                unborn_repo.path().to_str().unwrap()
            ])
            .status()
            .unwrap()
            .success()
    );
    assert!(
        ds.export_git(
            unborn_repo.path(),
            &GitExportOptions {
                git_branch: Some("HEAD".into()),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );
    assert!(!unborn_repo.path().join(".git/refs/heads/HEAD").exists());
}

#[test]
fn gitattributes_filters_never_corrupt_worktree_across_branch_switch_or_noop_export() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:main ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.create_branch("feat", &BranchOptions::default()).unwrap();
    let feat = ds.branch("feat").unwrap();
    feat.load_str(
        "PREFIX ex: <http://example.org/> ex:feat ex:p 2 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    let repo = tempfile::tempdir().unwrap();
    // Pre-initialize with a repo-local CRLF rule in `.gitattributes`.
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main", repo.path().to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    fs::write(repo.path().join(".gitattributes"), "*.ttl text eol=crlf\n").unwrap();

    // Export `main`, then `feat`, then re-export `main` (up-to-date no-op branch switch).
    ds.export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    verify_git_tree(repo.path()).unwrap();
    assert!(
        !fs::read(repo.path().join("graphs/default.ttl"))
            .unwrap()
            .contains(&b'\r')
    );

    ds.export_git(
        repo.path(),
        &GitExportOptions {
            branch: Some("feat".into()),
            ..GitExportOptions::default()
        },
    )
    .unwrap();
    verify_git_tree(repo.path()).unwrap();
    assert!(
        !fs::read(repo.path().join("graphs/default.ttl"))
            .unwrap()
            .contains(&b'\r')
    );

    let noop = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(noop.commits_exported, 0);
    verify_git_tree(repo.path()).unwrap();
    assert!(
        !fs::read(repo.path().join("graphs/default.ttl"))
            .unwrap()
            .contains(&b'\r'),
        "no-op branch switch must keep LF-exact bytes despite .gitattributes eol=crlf"
    );
}

#[test]
fn deleted_branch_handle_refuses_export_after_name_reuse() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:base ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.create_branch("topic", &BranchOptions::default())
        .unwrap();
    let stale_handle = ds.branch("topic").unwrap();
    stale_handle
        .load_str(
            "PREFIX ex: <http://example.org/> ex:oldTopic ex:p 2 .",
            sparkles::io::RdfFormat::Turtle,
        )
        .unwrap();

    // Delete `topic` and create a brand-new `topic` with different data.
    ds.delete_branch("topic", true).unwrap();
    ds.create_branch("topic", &BranchOptions::default())
        .unwrap();
    let new_handle = ds.branch("topic").unwrap();
    new_handle
        .load_str(
            "PREFIX ex: <http://example.org/> ex:newTopic ex:p 3 .",
            sparkles::io::RdfFormat::Turtle,
        )
        .unwrap();

    let repo = tempfile::tempdir().unwrap();
    let err = stale_handle
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("no longer exists"),
        "expected deleted-branch refusal, got: {err}"
    );
}

#[test]
fn squash_continuation_enforces_monotonic_progression_and_empty_default_export_grows() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();

    // 1. Default per-commit export on a fresh empty store (commit 0 only), then grow with
    //    commit 1 using the same default options.
    let empty_repo = tempfile::tempdir().unwrap();
    let r0 = ds
        .export_git(empty_repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(r0.commits_exported, 1);
    assert_eq!(r0.mapping.entries[0].sparkles_seq, 0);

    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let r1 = ds
        .export_git(empty_repo.path(), &GitExportOptions::default())
        .unwrap();
    assert_eq!(r1.commits_skipped, 1);
    assert_eq!(r1.commits_exported, 1);
    assert_eq!(r1.mapping.entries.len(), 2);
    assert_eq!(r1.mapping.entries[0].sparkles_seq, 0);
    assert_eq!(r1.mapping.entries[1].sparkles_seq, 1);

    // Add commits 2 and 3 for squash continuation checks.
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 2 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 3 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    let sq_repo = tempfile::tempdir().unwrap();
    let sq1 = ds
        .export_git(
            sq_repo.path(),
            &GitExportOptions {
                mode: GitExportMode::Squash,
                ..GitExportOptions::default()
            },
        )
        .unwrap();
    assert_eq!(sq1.commits_exported, 1);
    assert_eq!(sq1.mapping.entries[0].sparkles_seq, 3);

    // Older `to_seq` must be refused.
    assert!(
        ds.export_git(
            sq_repo.path(),
            &GitExportOptions {
                mode: GitExportMode::Squash,
                to_seq: Some(2),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );

    // Same head with shifted `from_seq` must be refused.
    assert!(
        ds.export_git(
            sq_repo.path(),
            &GitExportOptions {
                mode: GitExportMode::Squash,
                from_seq: Some(2),
                to_seq: Some(3),
                ..GitExportOptions::default()
            }
        )
        .is_err()
    );

    // Valid forward squash (`seq 3 -> seq 4`) succeeds and appends 1 commit.
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 4 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let sq2 = ds
        .export_git(
            sq_repo.path(),
            &GitExportOptions {
                mode: GitExportMode::Squash,
                ..GitExportOptions::default()
            },
        )
        .unwrap();
    assert_eq!(sq2.commits_exported, 1);
    assert_eq!(sq2.mapping.entries.len(), 2);
    assert_eq!(sq2.mapping.entries[1].sparkles_seq, 4);
}

#[test]
fn import_aborts_before_publishing_quads_when_prefix_persistence_fails() {
    let tree_dir = tempfile::tempdir().unwrap();
    let prefixes = BTreeMap::from([("p".to_string(), "http://example.org/p/".to_string())]);
    let bn = BlankNode::new("b0").unwrap();
    let quads = vec![
        Quad::new(
            bn.clone(),
            nn("http://example.org/p/name"),
            Literal::from("node"),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            nn("http://example.org/p/root"),
            nn("http://example.org/p/ref"),
            bn,
            GraphName::DefaultGraph,
        ),
    ];
    write_git_tree(quads, &prefixes, tree_dir.path(), &WriteOptions::default()).unwrap();

    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("db");
    let ds = Dataset::open(&db_path).unwrap();

    // Block `prefixes.json` persistence by creating a directory at `prefixes.json`.
    let prefixes_path = db_path.join("prefixes.json");
    if prefixes_path.exists() {
        fs::remove_file(&prefixes_path).unwrap();
    }
    fs::create_dir(&prefixes_path).unwrap();

    let err = import_git_tree(&ds, tree_dir.path()).unwrap_err();
    assert!(matches!(err, GitError::Store(_)), "{err:?}");
    assert!(
        collect_dataset_quads(&ds).unwrap().is_empty(),
        "failed prefix persistence must NOT leave committed quads behind"
    );
    assert!(!ds.prefixes().contains_key("p"));

    // Remove the blocking directory and retry: exactly one set of blank nodes is imported.
    fs::remove_dir(&prefixes_path).unwrap();
    let receipt = import_git_tree(&ds, tree_dir.path()).unwrap();
    assert!(receipt.committed);
    assert_eq!(collect_dataset_quads(&ds).unwrap().len(), 2);
    assert_eq!(
        ds.prefixes().get("p").map(String::as_str),
        Some("http://example.org/p/")
    );
}

#[test]
fn corrupt_changelog_segment_fails_export_before_publishing_instead_of_dropping_authors() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("db");
    let ds = Dataset::open(&db_path).unwrap();
    for (i, author) in [
        (1i64, "Alice <alice@example.org>"),
        (2i64, "Bob <bob@example.org>"),
        (3i64, "Carol <carol@example.org>"),
    ] {
        ds.transaction_receipt_with(
            StoreWriteOptions {
                author: Some(Arc::from(author)),
                message: Some(Arc::from(format!("Commit {i}"))),
                ..Default::default()
            },
            |tx| {
                tx.insert(
                    Quad::new(
                        nn(&format!("http://example.org/s{i}")),
                        nn("http://example.org/p"),
                        Literal::from(i),
                        GraphName::DefaultGraph,
                    )
                    .as_ref(),
                )?;
                Ok(())
            },
        )
        .unwrap();
    }
    ds.store().flush_change_log().unwrap();

    // Corrupt the flushed change-log segment file on disk (`<db>/changes/*.log`).
    let cl_dir = db_path.join("changes");
    let mut corrupted = false;
    for entry in fs::read_dir(&cl_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) == Some("log") {
            let mut bytes = fs::read(&path).unwrap();
            for b in &mut bytes {
                *b ^= 0xff;
            }
            fs::write(&path, bytes).unwrap();
            corrupted = true;
        }
    }
    assert!(corrupted, "expected a flushed .log file under changes/");

    let repo = tempfile::tempdir().unwrap();
    let err = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("corrupt")
            || err.to_string().contains("change-log")
            || err.to_string().contains("EOF")
            || err.to_string().contains("invalid"),
        "expected change-log metadata error, got: {err}"
    );
    // Must not have published any Git commits.
    let head = Command::new("git")
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(
        !head.status.success(),
        "export must fail before publishing commits when change-log metadata is corrupt"
    );
}

#[test]
fn canonicalization_budget_catches_graph_position_distinct_predicates_triple_terms_and_cdt_expansion()
 {
    // 1. Cyclic bipartite graph: 10 ambiguous subject blank nodes `u0..u9` linked to 10
    //    indistinguishable ambiguous blank-node graphs `v0..v9` via 10 DISTINCT predicates
    //    `ex:p0..ex:p9`: `Quad(u_i, p_k, "x", v_{(i + k) % 10})`.
    //    Every `u_i` shares one 1-hop signature and every `v_j` shares one 1-hop signature.
    //    In oxrdf, `position == "g"` omits the predicate, so all 10 `v_j` merge into one
    //    permutation bucket (`10!`) even though each edge from `u_i` has a distinct predicate.
    let mut g_poison = Vec::new();
    for i in 0..10usize {
        let u = BlankNode::new(format!("u{i}")).unwrap();
        for k in 0..10usize {
            let v = BlankNode::new(format!("v{}", (i + k) % 10)).unwrap();
            g_poison.push(Quad::new(
                u.clone(),
                nn(&format!("http://example.org/p{k}")),
                Literal::from("x"),
                GraphName::BlankNode(v),
            ));
        }
    }
    let dir1 = tempfile::tempdir().unwrap();
    let err1 = write_git_tree(
        g_poison,
        &BTreeMap::new(),
        dir1.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err1, GitError::Format(ref m) if m.contains("budget exceeded")),
        "expected g-position bucket refusal, got: {err1:?}"
    );

    // 2. Symmetric K_10 clique nested inside RDF 1.2 triple terms must be caught in preflight.
    let mut t_poison = Vec::new();
    let root = nn("http://example.org/r");
    let edge = nn("http://example.org/e");
    let inner_p = nn("http://example.org/p");
    for i in 0..10 {
        for j in 0..10 {
            if i != j {
                let tr = Triple::new(
                    BlankNode::new(format!("k{i}")).unwrap(),
                    inner_p.clone(),
                    BlankNode::new(format!("k{j}")).unwrap(),
                );
                t_poison.push(Quad::new(
                    root.clone(),
                    edge.clone(),
                    Term::Triple(Box::new(tr)),
                    GraphName::DefaultGraph,
                ));
            }
        }
    }
    let dir2 = tempfile::tempdir().unwrap();
    let err2 = write_git_tree(
        t_poison,
        &BTreeMap::new(),
        dir2.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err2, GitError::Format(ref m) if m.contains("budget exceeded")),
        "expected RDF 1.2 triple-term budget refusal, got: {err2:?}"
    );

    // 3. Single quad whose `cdt:List` expands beyond `WriteOptions::max_quads` must refuse
    //    during CDT expansion.
    let cdt_list = nn("http://w3id.org/awslabs/neptune/SPARQL-CDTs/List");
    let elems = (0..20)
        .map(|i| i.to_string())
        .chain(std::iter::once("_:b0".to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    let cdt_quad = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/list"),
        Literal::new_typed_literal(format!("[{elems}]"), cdt_list),
        GraphName::DefaultGraph,
    );
    let dir3 = tempfile::tempdir().unwrap();
    let err3 = write_git_tree(
        vec![cdt_quad],
        &BTreeMap::new(),
        dir3.path(),
        &WriteOptions {
            max_quads: 5,
            ..WriteOptions::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(err3, GitError::Format(ref m) if m.contains("quad limit")),
        "expected CDT expansion quad-limit refusal, got: {err3:?}"
    );
}

#[test]
fn write_and_mapping_byte_limits_refuse_cleanly_before_publishing() {
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from("a payload longer than sixteen bytes"),
        GraphName::DefaultGraph,
    );

    // 1. Per-graph write limit leaves no partial files behind.
    let dir_g = tempfile::tempdir().unwrap();
    let err_g = write_git_tree(
        vec![q.clone()],
        &BTreeMap::new(),
        dir_g.path(),
        &WriteOptions {
            max_graph_file_bytes: 16,
            ..WriteOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err_g, GitError::Format(_)), "{err_g:?}");
    assert!(!dir_g.path().join("graphs/default.ttl").exists());
    assert!(!dir_g.path().join(MANIFEST_REL_PATH).exists());

    // 2. Manifest write limit leaves no graph or manifest files persisted.
    let dir_m = tempfile::tempdir().unwrap();
    let err_m = write_git_tree(
        vec![q],
        &BTreeMap::new(),
        dir_m.path(),
        &WriteOptions {
            max_manifest_bytes: 16,
            ..WriteOptions::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err_m, GitError::Format(_)), "{err_m:?}");
    assert!(!dir_m.path().join("graphs/default.ttl").exists());
    assert!(!dir_m.path().join(MANIFEST_REL_PATH).exists());

    // 3. Mapping byte ceiling refuses export BEFORE moving the Git ref.
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let repo = tempfile::tempdir().unwrap();
    let err_map = ds
        .export_git(
            repo.path(),
            &GitExportOptions {
                max_mapping_bytes: 64,
                ..GitExportOptions::default()
            },
        )
        .unwrap_err();
    assert!(
        err_map.to_string().contains("maximum mapping size"),
        "expected mapping size limit error, got: {err_map}"
    );
    let head = Command::new("git")
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(
        !head.status.success(),
        "over-limit mapping must refuse BEFORE moving refs/heads/main"
    );
}

#[test]
fn strict_verify_rejects_noncanonical_blank_node_labels_in_graph_and_quads() {
    // 1. Noncanonical blank-node label `_:local0` in subject position with matching SHA-256
    //    and quad count must still be rejected by strict verification.
    let dir = tempfile::tempdir().unwrap();
    let bn = BlankNode::new("x").unwrap();
    let q = Quad::new(
        bn,
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q],
        &BTreeMap::new(),
        dir.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    let ttl_path = dir.path().join("graphs/default.ttl");
    let noncanon_bytes = b"_:local0 <http://example.org/p> 1 .\n";
    fs::write(&ttl_path, noncanon_bytes).unwrap();
    let mut sha = sparkles_core::jobs::Sha256Stream::new();
    sha.update(noncanon_bytes);
    let digest = sha.finalize_hex();
    let mf_path = dir.path().join(MANIFEST_REL_PATH);
    let mut mf: serde_json::Value = serde_json::from_slice(&fs::read(&mf_path).unwrap()).unwrap();
    mf["graphs"][0]["sha256"] = serde_json::Value::String(digest);
    fs::write(&mf_path, serde_json::to_vec_pretty(&mf).unwrap()).unwrap();

    let err = verify_git_tree(dir.path()).unwrap_err();
    assert!(
        matches!(err, GitError::InvalidManifest(ref m) if m.contains("non-canonical blank-node")),
        "expected non-canonical blank-node label refusal, got: {err:?}"
    );

    // 2. Leading-zero blank-node graph label `c14n00` rejected by strict manifest check.
    let dir2 = tempfile::tempdir().unwrap();
    let q2 = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::BlankNode(BlankNode::new("bg").unwrap()),
    );
    write_git_tree(
        vec![q2],
        &BTreeMap::new(),
        dir2.path(),
        &WriteOptions::default(),
    )
    .unwrap();
    let mf2_path = dir2.path().join(MANIFEST_REL_PATH);
    let mut mf2: serde_json::Value = serde_json::from_slice(&fs::read(&mf2_path).unwrap()).unwrap();
    let mut h = sparkles_core::jobs::Sha256Stream::new();
    h.update(b"c14n00");
    let ghash = h.finalize_hex();
    let old_path = dir2.path().join(mf2["graphs"][0]["path"].as_str().unwrap());
    let new_rel = format!("graphs/bnode-c14n00-{}.ttl", &ghash[..16]);
    fs::rename(&old_path, dir2.path().join(&new_rel)).unwrap();
    mf2["graphs"][0]["label"] = serde_json::Value::String("c14n00".into());
    mf2["graphs"][0]["path"] = serde_json::Value::String(new_rel);
    fs::write(&mf2_path, serde_json::to_vec_pretty(&mf2).unwrap()).unwrap();

    let err2 = verify_git_tree(dir2.path()).unwrap_err();
    assert!(
        matches!(err2, GitError::InvalidManifest(ref m) if m.contains("non-canonical blank-node")),
        "expected c14n00 rejection, got: {err2:?}"
    );
}

// ================================================================================
// Round 3 regression tests (D1–D12)
// ================================================================================

#[cfg(unix)]
#[test]
fn symlinked_ancestor_with_existing_or_missing_leaf_rejected_everywhere() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    let real_sub = outside.path().join("real-sub");
    fs::create_dir(&real_sub).unwrap();
    let real_tree = outside.path().join("real-tree");
    let q = Quad::new(
        nn("http://example.org/s"),
        nn("http://example.org/p"),
        Literal::from(1i64),
        GraphName::DefaultGraph,
    );
    write_git_tree(
        vec![q.clone()],
        &BTreeMap::new(),
        &real_tree,
        &WriteOptions::default(),
    )
    .unwrap();

    let holder = tempfile::tempdir().unwrap();
    let link = holder.path().join("link-parent");
    symlink(outside.path(), &link).unwrap();

    // Existing leaf under symlinked ancestor (`link/real-sub`) AND missing leaf under
    // existing subdir beneath symlinked ancestor (`link/real-sub/missing-leaf`).
    for target in [link.join("real-sub"), link.join("real-sub/missing-leaf")] {
        let err = write_git_tree(
            vec![q.clone()],
            &BTreeMap::new(),
            &target,
            &WriteOptions::default(),
        )
        .unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "write_git_tree must reject symlinked ancestor {target:?}, got {err:?}"
        );
    }
    assert!(!real_sub.join(".sparkles-git").exists());
    assert!(!real_sub.join("missing-leaf").exists());

    let err_v = verify_git_tree(&link.join("real-tree")).unwrap_err();
    assert!(
        matches!(err_v, GitError::UnsafePath { .. }),
        "verify_git_tree must reject symlinked ancestor, got {err_v:?}"
    );

    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    for target in [link.join("real-sub"), link.join("real-sub/missing-repo")] {
        let err_e = sparkles::git::export_branch_to_git(&ds, &target, &GitExportOptions::default())
            .unwrap_err();
        assert!(
            matches!(err_e, GitError::UnsafePath { .. }),
            "export_git must reject symlinked ancestor {target:?}, got {err_e:?}"
        );
    }
    assert!(!real_sub.join(".git").exists());

    // Custom mapping parent beneath a symlinked ancestor must also be rejected.
    let valid_repo = tempfile::tempdir().unwrap();
    for map_dest in [
        link.join("real-sub/map.json"),
        link.join("real-sub/missing-dir/map.json"),
    ] {
        let err_m = sparkles::git::export_branch_to_git(
            &ds,
            valid_repo.path(),
            &GitExportOptions {
                mapping_path: Some(map_dest.clone()),
                ..GitExportOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err_m, GitError::UnsafePath { .. }),
            "export_git must reject mapping under symlinked ancestor {map_dest:?}, got {err_m:?}"
        );
    }
}

#[test]
fn custom_mapping_rejects_published_paths_pending_journal_and_missing_dotdot() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    let repo = tempfile::tempdir().unwrap();
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main", repo.path().to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );

    let bad_destinations = [
        repo.path().join(".sparkles-git/manifest.json"),
        repo.path().join("graphs/default.ttl"),
        repo.path().join(".git/sparkles/pending/custom.json"),
        repo.path().join(".git/index"),
        repo.path().join("missing-dir/../mapping.json"),
    ];
    for dest in bad_destinations {
        let err = sparkles::git::export_branch_to_git(
            &ds,
            repo.path(),
            &GitExportOptions {
                mapping_path: Some(dest.clone()),
                ..GitExportOptions::default()
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, GitError::UnsafePath { .. }),
            "expected UnsafePath for overlapping/invalid mapping {dest:?}, got {err:?}"
        );
        let head = Command::new("git")
            .args([
                "-C",
                repo.path().to_str().unwrap(),
                "rev-parse",
                "--verify",
                "--quiet",
                "HEAD",
            ])
            .output()
            .unwrap();
        assert!(
            !head.status.success(),
            "rejected mapping {dest:?} must not publish any Git commit"
        );
    }
}

#[cfg(unix)]
#[test]
fn required_clean_and_smudge_filters_never_run_while_raw_byte_edits_are_caught() {
    let db_dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(db_dir.path().join("db")).unwrap();
    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 1 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();

    let repo = tempfile::tempdir().unwrap();
    let sentinel = repo.path().join("FILTER_RAN_SENTINEL");
    assert!(
        Command::new("git")
            .args(["init", "-q", "-b", "main", repo.path().to_str().unwrap()])
            .status()
            .unwrap()
            .success()
    );
    fs::write(
        repo.path().join(".gitattributes"),
        "*.ttl filter=sentinel\n",
    )
    .unwrap();
    let hook_cmd = format!("sh -c 'touch {} && exit 1'", sentinel.display());
    for (k, v) in [
        ("filter.sentinel.required", "true"),
        ("filter.sentinel.clean", hook_cmd.as_str()),
        ("filter.sentinel.smudge", hook_cmd.as_str()),
    ] {
        assert!(
            Command::new("git")
                .args(["-C", repo.path().to_str().unwrap(), "config", k, v])
                .status()
                .unwrap()
                .success()
        );
    }

    // Export 1 and incremental Export 2 must succeed without ever invoking the clean/smudge
    // filter (which would create SENTINEL and exit 1).
    ds.export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert!(!sentinel.exists(), "clean/smudge filter must never run");

    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 2 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    ds.export_git(repo.path(), &GitExportOptions::default())
        .unwrap();
    assert!(
        !sentinel.exists(),
        "clean/smudge filter must never run on incremental export"
    );
    verify_git_tree(repo.path()).unwrap();

    // Mutate raw line endings (`\n` -> `\r\n`) on `graphs/default.ttl`: our raw byte check
    // must detect and refuse the local edit without calling the filter.
    let ttl_path = repo.path().join("graphs/default.ttl");
    let raw = fs::read_to_string(&ttl_path).unwrap();
    let crlf = raw.replace('\n', "\r\n");
    assert_ne!(raw, crlf);
    fs::write(&ttl_path, crlf).unwrap();

    ds.load_str(
        "PREFIX ex: <http://example.org/> ex:s ex:p 3 .",
        sparkles::io::RdfFormat::Turtle,
    )
    .unwrap();
    let err = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("uncommitted modifications"),
        "expected raw-byte dirty worktree refusal, got: {err}"
    );
    assert!(
        !sentinel.exists(),
        "clean filter must not run even when worktree is dirty"
    );
}

#[test]
fn single_byte_valid_utf8_author_flip_in_flushed_changelog_fails_crc() {
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("db");
    let ds = Dataset::open(&db_path).unwrap();
    ds.transaction_receipt_with(
        StoreWriteOptions {
            author: Some(Arc::from("AliceExample <alice@example.org>")),
            message: Some(Arc::from("Initial commit")),
            ..Default::default()
        },
        |tx| {
            tx.insert(
                Quad::new(
                    nn("http://example.org/s1"),
                    nn("http://example.org/p"),
                    Literal::from(1i64),
                    GraphName::DefaultGraph,
                )
                .as_ref(),
            )?;
            Ok(())
        },
    )
    .unwrap();
    ds.store().flush_change_log().unwrap();

    // Flip ONE ASCII byte inside the author name (`AliceExample` -> `ZliceExample`), keeping
    // the frame length, UTF-8 validity, and sequence number completely intact so ONLY CRC
    // verification catches the damage.
    let cl_dir = db_path.join("changes");
    let needle = b"AliceExample";
    let mut flipped = false;
    for entry in fs::read_dir(&cl_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) == Some("log") {
            let mut bytes = fs::read(&path).unwrap();
            if let Some(pos) = bytes.windows(needle.len()).position(|w| w == needle) {
                bytes[pos] = b'Z';
                fs::write(&path, bytes).unwrap();
                flipped = true;
            }
        }
    }
    assert!(
        flipped,
        "expected to find author bytes in flushed .log segment"
    );

    let repo = tempfile::tempdir().unwrap();
    let err = ds
        .export_git(repo.path(), &GitExportOptions::default())
        .unwrap_err();
    assert!(
        err.to_string().contains("CRC") || err.to_string().contains("corrupt"),
        "expected CRC corruption refusal on valid-UTF-8 author bit-flip, got: {err}"
    );
    let head = Command::new("git")
        .args([
            "-C",
            repo.path().to_str().unwrap(),
            "rev-parse",
            "--verify",
            "--quiet",
            "HEAD",
        ])
        .output()
        .unwrap();
    assert!(
        !head.status.success(),
        "CRC-corrupted changelog must fail before publishing any Git commit"
    );
}

#[test]
fn duplicate_input_quads_cannot_bypass_canonicalization_budget() {
    // Symmetric K_10 clique of blank nodes exceeds `max_ambiguous_group = 8`.
    // Adding `i` duplicate copies of one incident edge per node `k_i` gives every node a
    // distinct un-deduplicated 1-hop signature, which previously fooled preflight before
    // `oxrdf::Dataset` deduplicated them back into a symmetric clique.
    let mut quads = Vec::new();
    let p = nn("http://example.org/p");
    for i in 0..10usize {
        for j in 0..10usize {
            if i != j {
                let q = Quad::new(
                    BlankNode::new(format!("k{i}")).unwrap(),
                    p.clone(),
                    BlankNode::new(format!("k{j}")).unwrap(),
                    GraphName::DefaultGraph,
                );
                quads.push(q.clone());
                if j == (i + 1) % 10 {
                    for _ in 0..i {
                        quads.push(q.clone());
                    }
                }
            }
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let err = write_git_tree(
        quads,
        &BTreeMap::new(),
        dir.path(),
        &WriteOptions::default(),
    )
    .unwrap_err();
    assert!(
        matches!(err, GitError::Format(ref m) if m.contains("budget exceeded")),
        "duplicate input quads must not bypass canonicalization budget, got: {err:?}"
    );
}
