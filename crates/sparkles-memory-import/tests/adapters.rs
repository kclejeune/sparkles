//! The adapters against fixture trees of a home directory and a repository: never the
//! real `~/.claude` or `~/.codex` of the machine.

use sparkles_memory_import::vocab::*;
use sparkles_memory_import::{
    Adapter, Ctx, FileKind, Harness, Obj, Project, Request, Roots, Scope, redact, scan,
};
use std::path::{Path, PathBuf};

const STAGING: &str = "---\nname: staging-db\ndescription: Staging has its own Postgres on port 5433, separate from dev\nmetadata:\n  type: reference\nmodified: 2026-10-07T15:02:11Z\n---\nThe staging database runs in the `db-staging` container on port 5433. Migrations go\nthrough the deploy checklist first, see [[deploy-checklist]].\n";

struct Fixture {
    _dir: tempfile::TempDir,
    roots: Roots,
    repo: PathBuf,
    memdir: PathBuf,
}

fn write(p: &Path, text: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let repo = dir.path().join("src/shop");
    write(
        &repo.join(".git/config"),
        "[remote \"origin\"]\n\turl = git@github.com:acme/shop.git\n",
    );
    let roots = Roots {
        claude: home.join(".claude"),
        codex: home.join(".codex"),
        gemini: home.join(".gemini"),
        managed: dir.path().join("etc/claude-code"),
        home: home.clone(),
    };
    let project = Project::of(&repo);
    let name: String = project
        .dir
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let memdir = roots.claude.join("projects").join(name).join("memory");
    write(&memdir.join("staging-db.md"), STAGING);
    write(
        &memdir.join("MEMORY.md"),
        "# Memory\n\n- [Staging DB](staging-db.md) — staging runs Postgres on 5433\n- [Deploy checklist](deploy-checklist.md): the steps before a deploy\nSome prose line.\n",
    );
    write(
        &repo.join("CLAUDE.md"),
        "# Shop\n\nUse pnpm.\n@docs/testing.md\n@~/../../etc/passwd\n",
    );
    write(&repo.join("docs/testing.md"), "Run `pnpm test`.\n");
    write(
        &repo.join(".claude/rules/rust.md"),
        "---\npaths:\n  - \"src/**/*.rs\"\n---\nUse clippy.\n",
    );
    write(&repo.join("AGENTS.md"), "Codex: use pnpm.\n");
    write(&repo.join("web/AGENTS.md"), "Web: use vite.\n");
    write(&repo.join("GEMINI.md"), "Gemini notes.\n");
    write(
        &repo.join(".cursor/rules/ts.mdc"),
        "---\ndescription: TypeScript rules\nglobs: \"*.ts\"\nalwaysApply: false\n---\nStrict mode.\n",
    );
    write(&roots.claude.join("CLAUDE.md"), "I prefer short answers.\n");
    write(&roots.codex.join("AGENTS.md"), "User-wide Codex notes.\n");
    write(
        &roots.codex.join("memories/note.md"),
        "The user likes tea.\n",
    );
    write(&dir.path().join("etc/passwd"), "root:x:0:0\n");
    Fixture {
        _dir: dir,
        roots,
        repo,
        memdir,
    }
}

fn ctx() -> Ctx {
    Ctx {
        base: "https://example.org/memory/import/".into(),
        principal: "ana".into(),
        dataset_id: uuid::Uuid::nil(),
    }
}

const G: &str = "https://example.org/memory/import/ana/claude-code/github.com.acme.shop";

fn has(f: &sparkles_memory_import::FileImport, s: &str, p: &str, o: &Obj) -> bool {
    f.facts.iter().any(|x| x.s == s && x.p == p && &x.o == o)
}

#[test]
fn claude_code_memory_index_and_instructions() {
    let fx = fixture();
    let c = ctx();
    let patterns = redact::builtin();
    let req = Request {
        ctx: &c,
        roots: &fx.roots,
        adapters: vec![Adapter::ClaudeCode],
        project: Some(Project::of(&fx.repo)),
        user_scope: false,
        paths: vec![],
        instructions_only: false,
        patterns: &patterns,
    };
    let s = scan(&req);
    let graphs: Vec<&str> = s.files.iter().map(|f| f.graph.as_str()).collect();
    assert!(
        graphs.contains(&format!("{G}/memory/staging-db").as_str()),
        "{graphs:?}"
    );
    assert!(graphs.contains(&format!("{G}/index").as_str()));
    assert!(graphs.contains(&format!("{G}/instructions/CLAUDE.md").as_str()));
    assert!(graphs.contains(&format!("{G}/instructions/docs.testing.md").as_str()));
    assert!(graphs.contains(&format!("{G}/instructions/.claude.rules.rust.md").as_str()));
    assert_eq!(s.areas, vec![format!("{G}/")]);

    // the memory of §8.10.4
    let m = s.files.iter().find(|f| f.key == "staging-db").unwrap();
    assert_eq!(m.kind, FileKind::Memory);
    assert_eq!(m.rel_path, "staging-db.md");
    let e = m.entity.as_deref().unwrap();
    assert!(has(m, e, RDF_TYPE, &Obj::Iri(mem("ReferenceMemory"))));
    assert!(has(m, e, &mem("kind"), &Obj::lit("reference")));
    assert!(has(m, e, RDFS_LABEL, &Obj::lit("staging-db")));
    assert!(has(
        m,
        e,
        DCT_MODIFIED,
        &Obj::typed("2026-10-07T15:02:11Z", XSD_DATETIME)
    ));
    let checklist = c.memory_iri("claude-code", "github.com/acme/shop", "deploy-checklist");
    assert!(has(m, e, DCT_REFERENCES, &Obj::Iri(checklist.clone())));
    assert!(has(m, e, &mem("file"), &Obj::Iri(m.graph.clone())));
    let label = m
        .facts
        .iter()
        .find(|f| f.p == RDFS_LABEL && f.s == e)
        .unwrap();
    assert_eq!(label.quote.as_deref(), Some("name: staging-db"));
    let kind = m.facts.iter().find(|f| f.p == mem("kind")).unwrap();
    assert_eq!(kind.quote.as_deref(), Some("type: reference"));
    // the source
    assert!(has(
        m,
        &m.graph,
        &mem("filePath"),
        &Obj::lit("staging-db.md")
    ));
    assert!(
        m.facts
            .iter()
            .any(|f| f.s == m.graph && f.p == SPK_CONTENT_DIGEST)
    );
    let project = c.project_iri("github.com/acme/shop");
    assert!(has(
        m,
        &project,
        RDFS_LABEL,
        &Obj::lit("github.com/acme/shop")
    ));

    // the index: positions, and a dangling link that already has the memory's IRI
    let ix = s.files.iter().find(|f| f.kind == FileKind::Index).unwrap();
    let staging = c.memory_iri("claude-code", "github.com/acme/shop", "staging-db");
    assert_eq!(e, staging);
    assert!(has(
        ix,
        &staging,
        &mem("indexPosition"),
        &Obj::typed("1", XSD_INTEGER)
    ));
    assert!(has(
        ix,
        &staging,
        &mem("indexText"),
        &Obj::lit("staging runs Postgres on 5433")
    ));
    assert!(has(
        ix,
        &checklist,
        &mem("indexPosition"),
        &Obj::typed("2", XSD_INTEGER)
    ));

    // A65: the import is followed, the escape is not
    let cl = s
        .files
        .iter()
        .find(|f| f.key == "CLAUDE.md" && f.harness == Harness::ClaudeCode)
        .unwrap();
    let ce = cl.entity.as_deref().unwrap();
    let testing = c.instructions_iri("claude-code", "github.com/acme/shop", "docs.testing.md");
    assert!(has(cl, ce, &mem("imports"), &Obj::Iri(testing)));
    let dangling = c.dangling_iri("claude-code", "github.com/acme/shop", "~/../../etc/passwd");
    assert!(has(cl, ce, &mem("imports"), &Obj::Iri(dangling)));
    assert!(!s.files.iter().any(|f| f.path.ends_with("etc/passwd")));
    assert!(has(cl, ce, &mem("scope"), &Obj::Iri(Scope::Project.iri())));
    let rule = s
        .files
        .iter()
        .find(|f| f.key == ".claude.rules.rust.md")
        .unwrap();
    assert!(has(
        rule,
        rule.entity.as_deref().unwrap(),
        &mem("appliesTo"),
        &Obj::lit("src/**/*.rs")
    ));
    // the user scope is read only when asked
    assert!(!s.files.iter().any(|f| f.graph.contains("/user/")));
}

#[test]
fn codex_generic_user_scope_and_marker() {
    let fx = fixture();
    let c = ctx();
    let patterns = redact::builtin();
    let generated = fx.repo.join("AGENTS.sparkles.md");
    write(
        &generated,
        &format!("{}\nGenerated.\n", sparkles_memory_import::MARKER),
    );
    let req = Request {
        ctx: &c,
        roots: &fx.roots,
        adapters: vec![Adapter::Codex, Adapter::Generic, Adapter::ClaudeCode],
        project: Some(Project::of(&fx.repo.join("web"))),
        user_scope: true,
        paths: vec![generated.clone()],
        instructions_only: false,
        patterns: &patterns,
    };
    let s = scan(&req);
    let codex = "https://example.org/memory/import/ana/codex/";
    let keys: Vec<(&str, &str)> = s
        .files
        .iter()
        .filter(|f| f.graph.starts_with(codex))
        .map(|f| (f.graph.as_str(), f.rel_path.as_str()))
        .collect();
    assert!(keys.contains(&(
        "https://example.org/memory/import/ana/codex/github.com.acme.shop/instructions/AGENTS.md",
        "AGENTS.md"
    )), "{keys:?}");
    assert!(
        keys.iter()
            .any(|(g, _)| g.ends_with("/instructions/web.AGENTS.md"))
    );
    assert!(
        keys.iter().any(|(g, _)| *g
            == "https://example.org/memory/import/ana/codex/user/instructions/AGENTS.md")
    );
    let gen_mem = s
        .files
        .iter()
        .find(|f| f.graph == "https://example.org/memory/import/ana/codex/user/memory/note.md")
        .expect("Codex's memories are imported");
    assert!(has(
        gen_mem,
        gen_mem.entity.as_deref().unwrap(),
        RDF_TYPE,
        &Obj::Iri(mem("GeneratedMemory"))
    ));
    let gemini = s
        .files
        .iter()
        .find(|f| f.harness == Harness::GeminiCli)
        .unwrap();
    assert!(
        gemini
            .graph
            .ends_with("/gemini-cli/github.com.acme.shop/instructions/GEMINI.md")
    );
    let cursor = s
        .files
        .iter()
        .find(|f| f.harness == Harness::Cursor)
        .unwrap();
    let ce = cursor.entity.as_deref().unwrap();
    assert!(has(cursor, ce, &mem("appliesTo"), &Obj::lit("*.ts")));
    assert!(has(
        cursor,
        ce,
        &mem("alwaysApply"),
        &Obj::typed("false", XSD_BOOLEAN)
    ));
    assert!(has(cursor, ce, &mem("scope"), &Obj::Iri(Scope::Rule.iri())));
    // the user's CLAUDE.md
    assert!(s.files.iter().any(|f| f.graph
        == "https://example.org/memory/import/ana/claude-code/user/instructions/CLAUDE.md"));
    // A73: the generated file is skipped wherever it is
    assert!(
        s.skipped
            .iter()
            .any(|k| k.path == generated && k.reason == "generated"),
        "{:?}",
        s.skipped
    );
    assert!(!s.files.iter().any(|f| f.path == generated));
}

#[test]
fn redaction_digest_and_renames() {
    let fx = fixture();
    let c = ctx();
    let patterns = redact::builtin();
    let token = format!("ghp_{}", "x".repeat(36));
    write(
        &fx.memdir.join("tokens.md"),
        &format!("---\nname: tokens\ndescription: the CI token is {token}\n---\nbody\n"),
    );
    let req = Request {
        ctx: &c,
        roots: &fx.roots,
        adapters: vec![Adapter::ClaudeCode],
        project: Some(Project::of(&fx.repo)),
        user_scope: false,
        paths: vec![],
        instructions_only: true,
        patterns: &patterns,
    };
    // instructions only: no memory file and no index
    assert!(
        scan(&req)
            .files
            .iter()
            .all(|f| f.kind == FileKind::Instructions)
    );
    let req = Request {
        instructions_only: false,
        ..req
    };
    let s = scan(&req);
    let t = s.files.iter().find(|f| f.key == "tokens").unwrap();
    assert_eq!(t.redactions, vec!["github-token"]);
    for f in &t.facts {
        if let Obj::Literal { value, .. } = &f.o {
            assert!(!value.contains(&token));
        }
        assert!(!f.quote.as_deref().unwrap_or("").contains(&token));
    }
    assert!(has(
        t,
        t.entity.as_deref().unwrap(),
        SCHEMA_DESCRIPTION,
        &Obj::lit("the CI token is [redacted:github-token]")
    ));
    assert!(has(
        t,
        &t.graph,
        &mem("redactions"),
        &Obj::typed("1", XSD_INTEGER)
    ));
    // the digest is of the bytes on disk
    let bytes = std::fs::read(fx.memdir.join("tokens.md")).unwrap();
    assert_eq!(t.digest, sparkles_memory_import::ids::digest(&bytes));
    // a memory moved into a subdirectory keeps its key and graph; only the path changes
    let before = s
        .files
        .iter()
        .find(|f| f.key == "staging-db")
        .unwrap()
        .clone();
    std::fs::create_dir_all(fx.memdir.join("infra")).unwrap();
    std::fs::rename(
        fx.memdir.join("staging-db.md"),
        fx.memdir.join("infra/staging-db.md"),
    )
    .unwrap();
    let s2 = scan(&req);
    let after = s2.files.iter().find(|f| f.key == "staging-db").unwrap();
    assert_eq!(after.graph, before.graph);
    assert_eq!(after.entity, before.entity);
    assert_eq!(after.rel_path, "infra/staging-db.md");
    assert_eq!(after.digest, before.digest);
}
