//! Test-only capture of the actual clap definition. No installed CLI export is added.

use clap::{Command, CommandFactory};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

struct WalkBudget {
    remaining: usize,
    max_depth: usize,
    visited: usize,
    deepest: usize,
}

fn command_contract(command: &Command) -> Value {
    command_contract_with_limits(command, 32, 4096)
}

fn command_contract_with_limits(command: &Command, max_depth: usize, max_nodes: usize) -> Value {
    let mut budget = WalkBudget {
        remaining: max_nodes,
        max_depth,
        visited: 0,
        deepest: 0,
    };
    let result = walk_command(command, 0, &mut budget);
    eprintln!(
        "API CLI walk completed: {} nodes, depth {}",
        budget.visited, budget.deepest
    );
    result
}

fn walk_command(command: &Command, depth: usize, budget: &mut WalkBudget) -> Value {
    assert!(
        depth <= budget.max_depth,
        "CLI contract walk exceeded its depth budget"
    );
    assert!(
        budget.remaining > 0,
        "CLI contract walk exceeded its node budget"
    );
    budget.remaining -= 1;
    budget.visited += 1;
    budget.deepest = budget.deepest.max(depth);
    let mut flags = BTreeMap::new();
    let mut positionals = BTreeMap::new();
    for arg in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
        let range = arg.get_num_args().unwrap_or_default();
        let mut choices: Vec<_> = arg
            .get_value_parser()
            .possible_values()
            .into_iter()
            .flatten()
            .filter(|value| !value.is_hide_set())
            .map(|value| value.get_name().to_owned())
            .collect();
        choices.sort();
        let contract = json!({
            "required": arg.is_required_set(),
            "action": format!("{:?}", arg.get_action()),
            "arity": [range.min_values(), range.max_values()],
            "choices": choices,
        });
        if let Some(index) = arg.get_index() {
            let mut positional = contract.clone();
            positional["id"] = json!(arg.get_id().as_str());
            positional["index"] = json!(index);
            positionals.insert(index, positional);
        }
        for long in arg.get_long_and_visible_aliases().unwrap_or_default() {
            flags.insert(format!("--{long}"), contract.clone());
        }
        for short in arg.get_short_and_visible_aliases().unwrap_or_default() {
            flags.insert(format!("-{short}"), contract.clone());
        }
    }
    let commands: BTreeMap<_, _> = command
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| {
            (
                command.get_name().to_owned(),
                walk_command(command, depth + 1, budget),
            )
        })
        .collect();
    let mut aliases: Vec<_> = command.get_visible_aliases().collect();
    aliases.sort_unstable();
    json!({
        "name": command.get_name(),
        "aliases": aliases,
        "flags": flags,
        "positionals": positionals.into_values().collect::<Vec<_>>(),
        "commands": commands,
    })
}

fn tree() -> Value {
    // The existing group_options_tests uses this stack for the full derived CLI.
    // Keep construction, build and traversal distinct in capture diagnostics.
    std::thread::Builder::new()
        .name("api-cli-contract".into())
        .stack_size(crate::THREAD_STACK)
        .spawn(|| {
            eprintln!("API CLI stage: constructing real definition");
            let mut command = crate::Cli::command();
            eprintln!("API CLI stage: building real definition");
            command.build();
            eprintln!("API CLI stage: walking built definition");
            json!({"schema_version": 1, "profile": "standard", "command": command_contract(&command)})
        })
        .unwrap()
        .join()
        .unwrap()
}

fn classify_profile(default_enabled: bool, complete_standard: bool) -> bool {
    assert!(
        !default_enabled || complete_standard,
        "the default CLI feature profile changed; review the baseline instead of skipping its comparison"
    );
    complete_standard
}

fn standard_profile() -> bool {
    // Match Cargo.toml's complete default set. Extra features preserve this profile.
    classify_profile(
        cfg!(feature = "default"),
        cfg!(all(
            feature = "reasoning",
            feature = "shacl",
            feature = "shex",
            feature = "mimalloc",
            feature = "text",
            feature = "text-cjk-dict",
            feature = "geo",
            feature = "geo-epsg",
            feature = "otel",
            feature = "auth",
            feature = "mcp",
            feature = "memory",
            feature = "backup",
            feature = "backup-encryption",
            feature = "fmt",
            feature = "tls",
            feature = "graphql",
            feature = "pdf",
            feature = "pdf-ocr",
            feature = "office",
            feature = "embed-local",
            feature = "mapping-sqlite",
            feature = "mapping-parquet"
        )),
    )
}

pub(crate) fn compare_current(surface: &str, value: &Value) {
    let dir = tempfile::tempdir().unwrap();
    let current = dir.path().join("current.json");
    std::fs::write(&current, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    let output = std::process::Command::new("python3")
        .arg(Path::new(ROOT).join("scripts/api-compat.py"))
        .args(["--surface", surface, "--current"])
        .arg(&current)
        .args(["--version", env!("CARGO_PKG_VERSION")])
        .output()
        .expect("API compatibility tests require Python 3.11+ on PATH as python3");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn compatible_with_api_baseline() {
    if !standard_profile() {
        eprintln!(
            "standard CLI baseline comparison is inapplicable to this stripped profile; serializer fixtures still run"
        );
        return;
    }
    compare_current("cli", &tree());
}

/// Explicit one-shot capture writes only to a new external file, never a baseline.
#[test]
fn capture_standard_cli_contract() {
    let Some(path) = std::env::var_os("SPARKLES_API_CAPTURE") else {
        return;
    };
    assert!(
        standard_profile(),
        "capture requires the complete standard feature profile"
    );
    let path = PathBuf::from(path);
    assert!(path.is_absolute(), "capture path must be absolute");
    let path_parent = path.parent().unwrap().canonicalize().unwrap();
    assert!(
        !path_parent.starts_with(Path::new(ROOT).canonicalize().unwrap().join("testsuite")),
        "historical baselines cannot be overwritten by capture"
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    use std::io::Write;
    file.write_all(&serde_json::to_vec_pretty(&tree()).unwrap())
        .unwrap();
    file.write_all(b"\n").unwrap();
}

#[test]
fn serializer_preserves_visible_contract_and_ignores_prose_hidden_flags() {
    let mut command = Command::new("tool")
        .arg(
            clap::Arg::new("global")
                .long("global")
                .global(true)
                .visible_alias("alias"),
        )
        .arg(clap::Arg::new("private").long("private").hide(true))
        .subcommand(
            Command::new("read")
                .visible_alias("r")
                .arg(clap::Arg::new("input").index(1).required(true)),
        );
    command.build();
    let a = command_contract(&command);
    assert!(a["flags"]["--global"].is_object());
    assert!(a["flags"]["--alias"].is_object());
    assert!(a["flags"].get("--private").is_none());
    assert_eq!(a["commands"]["read"]["aliases"], json!(["r"]));
    assert_eq!(a["commands"]["read"]["positionals"][0]["required"], true);
    assert_eq!(a["commands"]["read"]["positionals"][0]["id"], "input");
    assert!(a["commands"]["read"]["flags"]["--global"].is_object());
    command = command.about("new human words");
    assert_eq!(a, command_contract(&command));
}

#[test]
fn same_shaped_positional_reorder_changes_binding_and_is_rejected() {
    fn command(reversed: bool) -> Command {
        let (first, second) = if reversed {
            ("new_name", "name")
        } else {
            ("name", "new_name")
        };
        let mut command = Command::new("tool")
            .arg(clap::Arg::new(first).index(1).required(true))
            .arg(clap::Arg::new(second).index(2).required(true));
        command.build();
        command
    }
    let before = command(false);
    let after = command(true);
    let old_values = before
        .clone()
        .try_get_matches_from(["tool", "old", "new"])
        .unwrap();
    let new_values = after
        .clone()
        .try_get_matches_from(["tool", "old", "new"])
        .unwrap();
    assert_eq!(old_values.get_one::<String>("name").unwrap(), "old");
    assert_eq!(new_values.get_one::<String>("name").unwrap(), "new");
    let old_contract = command_contract(&before);
    let new_contract = command_contract(&after);
    assert_eq!(old_contract["positionals"][0]["id"], "name");
    assert_eq!(new_contract["positionals"][0]["id"], "new_name");
    assert_ne!(old_contract, new_contract);

    let dir = tempfile::tempdir().unwrap();
    for (filename, command) in [("before.json", old_contract), ("after.json", new_contract)] {
        let tree = json!({"schema_version": 1, "profile": "standard", "command": command});
        std::fs::write(
            dir.path().join(filename),
            serde_json::to_vec(&tree).unwrap(),
        )
        .unwrap();
    }
    let output = std::process::Command::new("python3")
        .args(["-B", "-c", "import json,runpy,sys; api=runpy.run_path(sys.argv[1]); before=json.load(open(sys.argv[2])); after=json.load(open(sys.argv[3])); ds=api['compare_cli'](before,after); assert any(d.rule=='positional-changed' and d.severity=='break' for d in ds), ds; print(*ds,sep='\\n')"])
        .arg(Path::new(ROOT).join("scripts/api-compat.py"))
        .arg(dir.path().join("before.json"))
        .arg(dir.path().join("after.json"))
        .output()
        .expect("CLI compatibility fixtures require Python 3.11+ on PATH as python3");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("positional-changed"));
}

#[test]
fn default_profile_cannot_silently_become_a_stripped_skip() {
    assert!(classify_profile(true, true));
    assert!(classify_profile(false, true));
    assert!(!classify_profile(false, false));
    assert!(std::panic::catch_unwind(|| classify_profile(true, false)).is_err());
}

#[test]
fn generated_help_clones_are_finite_and_never_rebuilt_by_the_walker() {
    let mut command =
        Command::new("tool").subcommand(Command::new("one").subcommand(Command::new("two")));
    command.build();
    let value = command_contract_with_limits(&command, 8, 32);
    assert_eq!(value["commands"]["one"]["commands"]["two"]["name"], "two");
    let help = &value["commands"]["help"]["commands"]["one"];
    assert_eq!(help["commands"]["two"]["name"], "two");
    assert!(help["commands"].get("help").is_none());
}

#[test]
#[should_panic(expected = "depth budget")]
fn command_walk_refuses_excessive_depth_instead_of_truncating() {
    let mut command = Command::new("leaf");
    for _ in 0..8 {
        command = Command::new("branch").subcommand(command);
    }
    command_contract_with_limits(&command, 3, 32);
}

#[test]
#[should_panic(expected = "node budget")]
fn command_walk_refuses_excessive_width_instead_of_truncating() {
    let command = Command::new("tool")
        .subcommand(Command::new("one"))
        .subcommand(Command::new("two"));
    command_contract_with_limits(&command, 3, 2);
}
