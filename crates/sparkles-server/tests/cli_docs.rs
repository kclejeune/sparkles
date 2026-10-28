//! `sparkles completions`, `sparkles man` and `sparkles openapi` as the real binary
//! (spec X03).

use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[test]
fn completions_for_each_shell() {
    let zsh = run(&["completions", "zsh"]);
    assert!(zsh.status.success());
    assert!(out(&zsh).starts_with("#compdef sparkles"));
    // subcommands and their flags are completed
    assert!(out(&zsh).contains("--auth-config"));
    // `memory sync --no-ingest` exists only with the memory feature
    #[cfg(feature = "memory")]
    assert!(out(&zsh).contains("--no-ingest"));
    assert!(out(&zsh).contains("--layer"));
    for (shell, marker) in [
        ("bash", "complete -F _sparkles"),
        ("fish", "complete -c sparkles"),
        ("elvish", "edit:completion:arg-completer[sparkles]"),
        ("powershell", "Register-ArgumentCompleter"),
    ] {
        let o = run(&["completions", shell]);
        assert!(o.status.success(), "{shell}");
        assert!(out(&o).contains(marker), "{shell}");
    }
    let bad = run(&["completions", "nushell"]);
    assert_eq!(bad.status.code(), Some(2));
}

#[test]
fn man_pages() {
    let dir = tempfile::tempdir().unwrap();
    let man = dir.path().join("man");
    let o = run(&["man", "--dir", man.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let mut pages = vec![
        "sparkles.1",
        "sparkles-serve.1",
        "sparkles-completions.1",
        "sparkles-settings-edit.1",
        "sparkles-settings-apply.1",
    ];
    // these commands exist only with their features
    if cfg!(feature = "auth") {
        pages.push("sparkles-auth-login.1");
    }
    if cfg!(feature = "memory") {
        pages.extend([
            "sparkles-memory-consolidate.1",
            "sparkles-memory-maintenance.1",
        ]);
    }
    for page in pages {
        let text = std::fs::read_to_string(man.join(page)).unwrap_or_else(|_| panic!("{page}"));
        assert!(text.contains(".TH"), "{page}");
    }
    assert!(!man.join("sparkles-help.1").exists());
    let stdout = run(&["man"]);
    assert!(out(&stdout).contains(".SH SUBCOMMANDS"));
}

#[test]
fn openapi_document() {
    let o = run(&["openapi"]);
    assert!(o.status.success());
    let doc: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(doc["openapi"], "3.1.0");
    assert!(doc["paths"]["/$/datasets"]["get"].is_object());
    let y = run(&["openapi", "--format", "yaml"]);
    assert!(out(&y).contains("\nopenapi: \"3.1.0\"\n"));
}

#[test]
fn help_and_unknown_option_exit_codes() {
    assert_eq!(run(&["--help"]).status.code(), Some(0));
    assert_eq!(run(&["--item60-unknown-option"]).status.code(), Some(2));
}
