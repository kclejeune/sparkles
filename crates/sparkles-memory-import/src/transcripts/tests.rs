use super::*;
use std::path::PathBuf;

const CLAUDE: &str = r##"{"type":"summary","summary":"x"}
{"type":"user","sessionId":"s1","cwd":"/p","gitBranch":"main","version":"2.1.0","timestamp":"2026-10-01T10:00:00Z","message":{"role":"user","content":"Where is the staging database?"}}
{"type":"assistant","sessionId":"s1","timestamp":"2026-10-01T10:00:05Z","message":{"role":"assistant","model":"claude-x","content":[{"type":"thinking","thinking":"secret thoughts"},{"type":"text","text":"It runs on port 5433."},{"type":"tool_use","name":"Write","input":{"file_path":"/mem/staging-db.md","content":"..."}}]}}
{"type":"user","sessionId":"s1","timestamp":"2026-10-01T10:00:06Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"written"}]}}
{"type":"assistant","sessionId":"s1","timestamp":"2026-10-01T10:00:07Z","message":{"role":"assistant","content":[{"type":"text","text":"# Done\nSaved."}]}}
{"type":"user","isSidechain":true,"sessionId":"s1","timestamp":"2026-10-01T10:00:08Z","message":{"role":"user","content":"side"}}
"##;

const NO_TOOLS: Opts = Opts {
    tools: false,
    sidechain: false,
};
const TOOLS: Opts = Opts {
    tools: true,
    sidechain: false,
};

fn roles(s: &Session) -> Vec<&str> {
    s.turns.iter().map(|t| t.role.as_str()).collect()
}

#[test]
fn claude_turns() {
    let s = claude::parse(CLAUDE, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("s1"));
    assert_eq!(s.model.as_deref(), Some("claude-x"));
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert_eq!(s.writes, ["/mem/staging-db.md"]);
    assert_eq!(roles(&s), ["user", "assistant"]);
    let r = render(&s).concat();
    assert!(!r.contains("secret thoughts") && !r.contains("written") && !r.contains("side"));
    assert!(r.contains("\\# Done"), "{r}");
    assert!(r.starts_with("## user 2026-10-01T10:00:00Z\n\nWhere is"));
    let r = render(&claude::parse(CLAUDE, &TOOLS)).concat();
    assert!(
        r.contains("Tool call Write") && r.contains("## tool"),
        "{r}"
    );
}

#[test]
fn codex_turns() {
    let log = r#"{"timestamp":"2026-10-01T10:00:00Z","type":"session_meta","payload":{"id":"c1","cwd":"/p","cli_version":"0.50.0","git":{"branch":"dev"}}}
{"timestamp":"2026-10-01T10:00:00Z","type":"turn_context","payload":{"model":"gpt-x"}}
{"timestamp":"2026-10-01T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>cwd</environment_context>"}]}}
{"timestamp":"2026-10-01T10:00:02Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Run the tests"}]}}
{"timestamp":"2026-10-01T10:00:03Z","type":"response_item","payload":{"type":"reasoning","encrypted_content":"zzz"}}
{"timestamp":"2026-10-01T10:00:04Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"All pass."}]}}
"#;
    let s = codex::parse(log, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("c1"));
    assert_eq!(s.model.as_deref(), Some("gpt-x"));
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.turns[0].text, "Run the tests");
    assert_eq!(detect(&PathBuf::from("x.jsonl"), log), Format::Codex);
}

const GEMINI: &str = r##"{"sessionId":"g1","projectHash":"abc","startTime":"2026-10-01T10:00:00.000Z","lastUpdated":"2026-10-01T10:05:00.000Z","kind":"main","directories":["/p"]}
{"id":"m1","timestamp":"2026-10-01T10:00:01.000Z","type":"user","content":[{"text":"Where is the staging database?"}]}
{"id":"m2","timestamp":"2026-10-01T10:00:05.000Z","type":"gemini","content":"It runs on port 5433.","model":"gemini-x","thoughts":[{"subject":"s","description":"hidden thought"}],"toolCalls":[{"id":"t1","name":"read_file","args":{"path":"db.md"},"result":[{"functionResponse":{"id":"t1","name":"read_file","response":{"output":"port 5433"}}}]}]}
{"id":"m3","timestamp":"2026-10-01T10:00:06.000Z","type":"info","content":"Update available"}
{"id":"m4","timestamp":"2026-10-01T10:00:07.000Z","type":"user","content":"a wrong turn"}
{"$rewindTo":"m4"}
{"id":"m5","timestamp":"2026-10-01T10:00:09.000Z","type":"user","content":"Thanks"}
{"$set":{"summary":"db"}}
"##;

#[test]
fn gemini_recording() {
    let s = gemini::parse(GEMINI, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("g1"));
    assert_eq!(s.cwd.as_deref(), Some("/p"));
    assert_eq!(s.model.as_deref(), Some("gemini-x"));
    assert_eq!(roles(&s), ["user", "assistant", "user"]);
    assert_eq!(s.turns[0].text, "Where is the staging database?");
    assert_eq!(s.turns[2].text, "Thanks");
    let r = render(&s).concat();
    assert!(!r.contains("hidden thought") && !r.contains("Update available"));
    assert!(!r.contains("wrong turn"), "{r}");
    let t = gemini::parse(GEMINI, &TOOLS);
    let r = render(&t).concat();
    assert!(
        r.contains("Tool call read_file:") && r.contains("port 5433"),
        "{r}"
    );
    assert_eq!(
        detect(&PathBuf::from("/h/.gemini/tmp/x/chats/s.jsonl"), GEMINI),
        Format::Gemini
    );
}

#[test]
fn gemini_older_forms() {
    let old = r#"{"sessionId":"g0","projectHash":"abc","startTime":"2026-09-01T10:00:00Z","lastUpdated":"2026-09-01T10:01:00Z","messages":[{"id":"1","timestamp":"2026-09-01T10:00:01Z","type":"user","content":"hello"},{"id":"2","timestamp":"2026-09-01T10:00:02Z","type":"gemini","content":"hi there"}]}"#;
    let s = gemini::parse(old, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("g0"));
    assert_eq!(roles(&s), ["user", "assistant"]);
    let cp = r#"[{"role":"user","parts":[{"text":"save point"}]},{"role":"model","parts":[{"text":"saved"}]}]"#;
    let s = gemini::parse(cp, &NO_TOOLS);
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.turns[1].text, "saved");
}

#[test]
fn cursor_jsonl() {
    let log = r#"{"role":"user","message":{"content":[{"type":"text","text":"<attached_files>x</attached_files>\n<user_query>\nHow do I run the tests?\n</user_query>"}]}}
{"role":"assistant","message":{"content":[{"type":"text","text":"Run cargo test."},{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}
{"type":"turn_ended"}
"#;
    let s = cursor::parse(log, &NO_TOOLS);
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.turns[0].text, "How do I run the tests?");
    assert_eq!(s.turns[1].text, "Run cargo test.");
    let t = cursor::parse(log, &TOOLS);
    assert!(t.turns[1].text.contains("Tool call Shell"), "{t:?}");
    assert_eq!(
        detect(
            &PathBuf::from("/h/.cursor/projects/p/agent-transcripts/a/a.jsonl"),
            log
        ),
        Format::Cursor
    );
}

#[test]
fn cursor_text() {
    let log = "user:\n<user_query>\nWhere is the config?\n</user_query>\n\nA:\n[Thinking] the user wants a path\n  more thinking\nThe config is in config.toml.\n[Tool call] Read\n  path: config.toml\n[Tool result] Read\n  port = 1\nIt sets the port.\n\nuser:\n<user_query>\nThanks\n</user_query>\n";
    let s = cursor::parse(log, &NO_TOOLS);
    assert_eq!(roles(&s), ["user", "assistant", "user"]);
    assert_eq!(s.turns[0].text, "Where is the config?");
    assert_eq!(
        s.turns[1].text,
        "The config is in config.toml.\n\nIt sets the port."
    );
    assert_eq!(s.turns[2].text, "Thanks");
    let t = cursor::parse(log, &TOOLS);
    assert!(
        t.turns
            .iter()
            .any(|x| x.role == "tool" && x.text.contains("port = 1")),
        "{t:?}"
    );
}

#[test]
fn copilot_events() {
    let log = r#"{"type":"session.start","id":"e1","timestamp":"2026-10-01T10:00:00.000Z","data":{"sessionId":"cp1","copilotVersion":"1.0.67","selectedModel":"gpt-x","startTime":"2026-10-01T10:00:00.000Z","context":{"cwd":"/p","gitRoot":"/p","branch":"main"}}}
{"type":"user.message","id":"e2","timestamp":"2026-10-01T10:00:01.000Z","data":{"content":"Add a retry to the client"}}
{"type":"assistant.message","id":"e3","timestamp":"2026-10-01T10:00:05.000Z","data":{"content":"I added a retry loop.","reasoningText":"private","toolRequests":[{"name":"edit","arguments":{"path":"client.rs"}}]}}
{"type":"tool.execution_complete","id":"e4","timestamp":"2026-10-01T10:00:06.000Z","data":{"result":{"content":"edited client.rs"}}}
{"type":"system.message","data":{"content":"sys"}}
"#;
    let s = copilot::parse(log, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("cp1"));
    assert_eq!(s.cwd.as_deref(), Some("/p"));
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert_eq!(s.model.as_deref(), Some("gpt-x"));
    assert_eq!(roles(&s), ["user", "assistant"]);
    let r = render(&s).concat();
    assert!(!r.contains("private") && !r.contains("edited client.rs") && !r.contains("sys"));
    let t = copilot::parse(log, &TOOLS);
    assert_eq!(roles(&t), ["user", "assistant", "tool"]);
    assert!(t.turns[1].text.contains("Tool call edit"), "{t:?}");
    // the members inline, without `data`
    let inline = r#"{"type":"user.message","timestamp":"2026-10-01T10:00:01Z","content":"hi"}"#;
    assert_eq!(copilot::parse(inline, &NO_TOOLS).turns[0].text, "hi");
    assert_eq!(
        detect(
            &PathBuf::from("/h/.copilot/session-state/x/events.jsonl"),
            log
        ),
        Format::Copilot
    );
}

#[test]
fn aider_history() {
    let log = "\n# aider chat started at 2026-10-01 10:00:00\n\n> Aider v0.86.1\n> Repo-map: using 4096 tokens\n\n#### Rename the helper\n#### and update its callers\n\nI renamed `helper` to `assist`.\n\n```python\nassist()\n```\n\n> Applied edit to util.py\n> Commit abc123 rename helper\n\n#### Thanks\n\nYou are welcome.\n\n# aider chat started at 2026-10-02 09:30:00\n\n#### Next day\n\nOk.\n";
    let s = aider::parse(log, &NO_TOOLS);
    assert_eq!(
        roles(&s),
        [
            "user",
            "assistant",
            "user",
            "assistant",
            "user",
            "assistant"
        ]
    );
    assert_eq!(s.turns[0].text, "Rename the helper\nand update its callers");
    assert_eq!(s.turns[0].time.as_deref(), Some("2026-10-01T10:00:00"));
    assert!(s.turns[1].text.contains("assist()"), "{:?}", s.turns[1]);
    assert!(!s.turns[1].text.contains("Applied edit"));
    assert_eq!(s.turns[4].time.as_deref(), Some("2026-10-02T09:30:00"));
    assert_eq!(s.started.as_deref(), Some("2026-10-01T10:00:00"));
    assert_eq!(s.ended.as_deref(), Some("2026-10-02T09:30:00"));
    let t = aider::parse(log, &TOOLS);
    assert!(
        t.turns
            .iter()
            .any(|x| x.role == "tool" && x.text.contains("Applied edit")),
        "{t:?}"
    );
    assert_eq!(detect(&PathBuf::from("x.md"), log), Format::Aider);
}

#[test]
fn generic_shapes() {
    // JSON lines
    let jl = r#"{"role":"user","content":"What is 2+2?","timestamp":"2026-10-01T10:00:00Z"}
{"role":"assistant","content":[{"type":"text","text":"4"}]}
{"role":"system","content":"ignored"}
"#;
    let s = generic::parse(jl, &NO_TOOLS);
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.turns[1].text, "4");
    // one JSON object with a list of messages
    let obj = r#"{"id":"x1","messages":[{"author":"human","text":"hi"},{"author":"bot","text":"hello"}]}"#;
    let s = generic::parse(obj, &NO_TOOLS);
    assert_eq!(s.id.as_deref(), Some("x1"));
    assert_eq!(roles(&s), ["user", "assistant"]);
    // headings
    let md = "# Chat export\n\n## User\n\nWhere is the key?\n\n## Assistant\n\nIn the vault.\n";
    let s = generic::parse(md, &NO_TOOLS);
    assert_eq!(roles(&s), ["user", "assistant"]);
    assert_eq!(s.turns[1].text, "In the vault.");
    // speaker lines
    let txt = "Human: one\ntwo\nAI: three\n";
    let s = generic::parse(txt, &NO_TOOLS);
    assert_eq!(s.turns[0].text, "one\ntwo");
    assert_eq!(s.turns[1].text, "three");
    // nothing recognizable
    assert!(
        generic::parse("just notes\nabout nothing\n", &NO_TOOLS)
            .turns
            .is_empty()
    );
    assert!(generic::parse("{\"a\":1}", &NO_TOOLS).turns.is_empty());
    assert_eq!(detect(&PathBuf::from("chat.md"), md), Format::Generic);
}

#[test]
fn parts() {
    let turns: Vec<String> = (0..10)
        .map(|i| format!("## user\n\n{}\n\n", "x".repeat(100 + i)))
        .collect();
    let p = split(&turns, 300, 8);
    assert!(p.iter().all(|x| x.len() <= 300 + 80));
    assert_eq!(p.concat().matches("## user").count(), 10);
    let p = split(&turns, 300, 2);
    assert_eq!(p.len(), 2);
    assert!(p[1].contains("more turns"), "{}", p[1]);
}

/// Every parser reads any bytes without panicking.
#[test]
fn damaged_logs_never_panic() {
    let samples = [
        CLAUDE,
        GEMINI,
        "{\"role\":",
        "[1,2,",
        "\u{0}\u{1}",
        "## user\n",
        "> \n####\n",
    ];
    for f in [
        Format::Claude,
        Format::Codex,
        Format::Gemini,
        Format::Cursor,
        Format::Copilot,
        Format::Aider,
        Format::Generic,
    ] {
        for s in samples {
            for n in 0..s.len().min(400) {
                if s.is_char_boundary(n) {
                    let _ = f.parse(&s[..n], &TOOLS);
                }
            }
        }
    }
}
