//! Integration tests for F25 Phase 1 (versioned canonical git serialization profile) and
//! Phase 2 (git export of a branch).

use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad, Triple};
use sparkles::Dataset;
use sparkles::branch::BranchOptions;
use sparkles::git::{
    CANONICALIZATION_ALGORITHM, CanonicalGraphKind, CanonicalSyntax, CommitMapping, GitError,
    GitExportMode, GitExportOptions, MANIFEST_FORMAT_VERSION, MANIFEST_PROFILE_NAME,
    MANIFEST_REL_PATH, MAPPING_FORMAT_VERSION, ReadOptions, WriteOptions, collect_dataset_quads,
    dataset_matches_tree, datasets_isomorphic, default_mapping_path, import_git_tree,
    read_git_tree, read_git_tree_with, write_git_tree,
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
    let map_path = default_mapping_path(repo_commits.path()).unwrap();
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
