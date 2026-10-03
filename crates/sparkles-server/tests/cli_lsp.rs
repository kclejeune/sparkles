//! `sparkles lsp` as a child process, driven over stdin/stdout with framed JSON-RPC the
//! way an editor drives it: the handshake, diagnostics on open and change, formatting
//! and range formatting as one minimal edit, the formatter's warnings as diagnostics,
//! positions in UTF-16 or UTF-8 across characters outside the BMP and `\r\n` line
//! breaks, config discovery, and the exit status of `shutdown`/`exit`.

#![cfg(feature = "fmt")]

use serde_json::{Value, json};
use sparkles_fmt::{Language, Options};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_sparkles");

/// A language server child and what it sent that nobody has looked at yet.
struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: mpsc::Receiver<Value>,
    /// notifications read while waiting for a response
    queued: VecDeque<Value>,
    next_id: i64,
}

impl Client {
    fn start(dir: &Path, env: &[(&str, &str)]) -> Client {
        let mut child = Command::new(BIN)
            .args(["lsp", "--stdio"])
            .current_dir(dir)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        // read frames on a thread so a hung server fails the test instead of blocking it
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            loop {
                let mut len = None;
                loop {
                    let mut header = String::new();
                    if r.read_line(&mut header).unwrap_or(0) == 0 {
                        return;
                    }
                    let header = header.trim_end();
                    if header.is_empty() {
                        break;
                    }
                    if let Some(v) = header.strip_prefix("Content-Length: ") {
                        len = Some(v.parse::<usize>().unwrap());
                    }
                }
                let mut body = vec![0; len.expect("a Content-Length header")];
                r.read_exact(&mut body).unwrap();
                let msg: Value = serde_json::from_slice(&body).expect("a JSON-RPC message");
                if tx.send(msg).is_err() {
                    return;
                }
            }
        });
        Client {
            stdin: child.stdin.take(),
            child,
            rx,
            queued: VecDeque::new(),
            next_id: 1,
        }
    }

    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        let stdin = self.stdin.as_mut().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self) -> Value {
        self.rx
            .recv_timeout(Duration::from_secs(60))
            .expect("no message from the server")
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// Send a request and wait for its response (`result` or `error` object).
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let msg = self.recv();
            if msg["id"] == id && msg.get("method").is_none() {
                return msg;
            }
            self.queued.push_back(msg);
        }
    }

    /// The next diagnostics published for `uri`.
    fn diagnostics(&mut self, uri: &str) -> Value {
        if let Some(i) = self.queued.iter().position(|m| {
            m["method"] == "textDocument/publishDiagnostics" && m["params"]["uri"] == uri
        }) {
            return self.queued.remove(i).unwrap()["params"].clone();
        }
        loop {
            let msg = self.recv();
            if msg["method"] == "textDocument/publishDiagnostics" && msg["params"]["uri"] == uri {
                return msg["params"].clone();
            }
            self.queued.push_back(msg);
        }
    }

    /// The next diagnostics published for `uri`, without the lint's findings (the
    /// formatter's own, which these tests check).
    fn fmt_diagnostics(&mut self, uri: &str) -> Value {
        let mut p = self.diagnostics(uri);
        let kept: Vec<Value> = p["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["source"] != "sparkles lint")
            .cloned()
            .collect();
        p["diagnostics"] = Value::Array(kept);
        p
    }

    /// `p` without the lint's findings.
    fn fmt_diagnostics_of(&self, mut p: Value) -> Value {
        let kept: Vec<Value> = p["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["source"] != "sparkles lint")
            .cloned()
            .collect();
        p["diagnostics"] = Value::Array(kept);
        p
    }

    /// `initialize` offering `encodings`, then `initialized`; the server's capabilities.
    fn initialize(&mut self, encodings: &[&str]) -> Value {
        let r = self.request(
            "initialize",
            json!({"processId": null, "rootUri": null, "capabilities": {
                "general": {"positionEncodings": encodings}}}),
        );
        self.notify("initialized", json!({}));
        r["result"].clone()
    }

    fn open(&mut self, uri: &str, language_id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": language_id, "version": 1, "text": text}}),
        );
    }

    fn change(&mut self, uri: &str, version: i32, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": version},
                   "contentChanges": [{"text": text}]}),
        );
    }

    fn format(&mut self, uri: &str) -> Value {
        self.request(
            "textDocument/formatting",
            json!({"textDocument": {"uri": uri},
                   "options": {"tabSize": 8, "insertSpaces": false}}),
        )
    }

    /// `shutdown`, `exit`, and the exit status.
    fn shutdown(mut self) -> Option<i32> {
        let r = self.request("shutdown", Value::Null);
        assert_eq!(r["result"], Value::Null, "{r}");
        self.notify("exit", Value::Null);
        self.wait()
    }

    fn wait(mut self) -> Option<i32> {
        drop(self.stdin.take());
        self.child.wait().unwrap().code()
    }
}

/// The byte offset of an LSP position in `text`, counted independently of the server:
/// lines end at `\n`, `\r\n` or `\r`; `utf16` counts UTF-16 units, else bytes.
fn byte_of(text: &str, pos: &Value, utf16: bool) -> usize {
    let (line, character) = (
        pos["line"].as_u64().unwrap() as usize,
        pos["character"].as_u64().unwrap() as usize,
    );
    let mut starts = vec![0];
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\r' if chars.peek().map(|&(_, c)| c) == Some('\n') => {
                chars.next();
                starts.push(i + 2);
            }
            '\r' | '\n' => starts.push(i + 1),
            _ => {}
        }
    }
    let start = starts[line];
    let mut units = 0;
    for (i, c) in text[start..].char_indices() {
        if units >= character || c == '\n' || c == '\r' {
            return start + i;
        }
        units += if utf16 { c.len_utf16() } else { c.len_utf8() };
    }
    text.len()
}

/// The position of byte `b`, counted the same way.
fn position_of(text: &str, b: usize, utf16: bool) -> Value {
    let before = &text[..b];
    let line = before.replace("\r\n", "\n").matches(['\n', '\r']).count();
    let start = before.rfind(['\n', '\r']).map_or(0, |i| i + 1);
    let character: usize = before[start..]
        .chars()
        .map(|c| if utf16 { c.len_utf16() } else { c.len_utf8() })
        .sum();
    json!({"line": line, "character": character})
}

/// Apply LSP text edits (non-overlapping) to `text`.
fn apply(text: &str, edits: &Value, utf16: bool) -> String {
    let mut edits: Vec<(usize, usize, &str)> = edits
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                byte_of(text, &e["range"]["start"], utf16),
                byte_of(text, &e["range"]["end"], utf16),
                e["newText"].as_str().unwrap(),
            )
        })
        .collect();
    edits.sort_by_key(|e| std::cmp::Reverse(e.0));
    let mut out = text.to_string();
    for (start, end, insert) in edits {
        out.replace_range(start..end, insert);
    }
    out
}

fn uri(path: &Path) -> String {
    format!("file://{}", path.display()).replace(' ', "%20")
}

/// `sparkles fmt --stdin-filepath PATH` on `text`, run in `dir`.
fn fmt_cli(dir: &Path, path: &Path, text: &str) -> String {
    let mut child = Command::new(BIN)
        .arg("fmt")
        .arg("--stdin-filepath")
        .arg(path)
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
    let o = child.wait_with_output().unwrap();
    assert!(o.status.success());
    String::from_utf8(o.stdout).unwrap()
}

#[test]
fn session_with_utf16_positions_crlf_and_a_config_file() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("my queries");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join(".sparklesfmt.toml"), "indent-width = 4\n").unwrap();
    let path = dir.join("sub/q.rq");
    let uri = uri(&path);

    let mut c = Client::start(d.path(), &[]);
    let caps = c.initialize(&["utf-16"]);
    assert_eq!(caps["capabilities"]["positionEncoding"], "utf-16", "{caps}");
    assert_eq!(caps["capabilities"]["documentFormattingProvider"], true);
    assert_eq!(
        caps["capabilities"]["documentRangeFormattingProvider"],
        true
    );
    assert_eq!(caps["capabilities"]["textDocumentSync"]["change"], 1);
    assert_eq!(caps["serverInfo"]["name"], "sparkles");

    // a syntax error (at the `2`) after characters outside the BMP, on a line after a `\r\n`
    let bad = "PREFIX ex: <http://example.org/>\r\nSELECT * { BIND(\"𝄞𝄞\" AS 2) }\r\n";
    c.open(&uri, "sparql", bad);
    let p = c.fmt_diagnostics(&uri);
    assert_eq!(p["version"], 1);
    let diags = p["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "{p}");
    assert_eq!(diags[0]["severity"], 1);
    assert_eq!(diags[0]["source"], "sparkles fmt");
    assert_eq!(diags[0]["code"], "syntax");
    assert!(
        diags[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("SPARQL syntax error: "),
        "{p}"
    );
    let two = bad.find(" 2)").unwrap() + 1;
    assert_eq!(
        diags[0]["range"]["start"],
        json!({"line": 1, "character": 26})
    );
    assert_eq!(diags[0]["range"]["start"], position_of(bad, two, true));
    assert_eq!(diags[0]["range"]["end"], position_of(bad, two + 1, true));
    // formatting a document with a syntax error: nothing to do (the diagnostic says why)
    let r = c.format(&uri);
    assert_eq!(r["result"], Value::Null, "{r}");
    // an incremental change (the server asks for full ones, but takes ranges), in UTF-16
    // units: the `2` becomes `?x`
    c.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [
            {"range": {"start": {"line": 1, "character": 26}, "end": {"line": 1, "character": 27}},
             "text": "?x"}]}),
    );
    assert_eq!(
        c.fmt_diagnostics(&uri),
        json!({"uri": uri, "version": 2, "diagnostics": []})
    );
    let r = c.format(&uri);
    let fixed = bad.replace(" 2)", " ?x)");
    assert_eq!(
        apply(&fixed, &r["result"], true),
        fmt_cli(d.path(), &path, &fixed)
    );

    // fixed, unformatted, with `\r\n` line breaks and characters outside the BMP
    let text = "prefix ex: <http://example.org/>\r\nselect ?s {?s ex:p \"𝄞 é\" .\r\n # 😀 note\r\n?s ex:q ?o}\r\n";
    c.change(&uri, 3, text);
    let p = c.fmt_diagnostics(&uri);
    assert_eq!(p["version"], 3);
    assert_eq!(p["diagnostics"], json!([]), "{p}");
    let expected = fmt_cli(d.path(), &path, text);
    assert!(
        expected.contains("\n    ?s ex:p"),
        "indent-width 4 from the config: {expected}"
    );
    let r = c.format(&uri);
    let edits = &r["result"];
    assert_eq!(edits.as_array().unwrap().len(), 1, "{r}");
    assert_eq!(apply(text, edits, true), expected);
    // range formatting formats the whole document, with the same edit
    let r = c.request(
        "textDocument/rangeFormatting",
        json!({"textDocument": {"uri": uri},
               "range": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 3}},
               "options": {"tabSize": 2, "insertSpaces": true}}),
    );
    assert_eq!(&r["result"], edits, "{r}");

    // the edit is minimal: only the changed middle of a document is replaced
    let tail = "\n# 😀 the end\n";
    let almost = format!("{}{tail}", expected.replace("    ?s ex:q", "  ?s   ex:q"));
    c.change(&uri, 4, &almost);
    assert_eq!(c.fmt_diagnostics(&uri)["version"], 4);
    let r = c.format(&uri);
    let e = &r["result"][0];
    assert_eq!(
        apply(&almost, &r["result"], true),
        format!("{expected}{tail}")
    );
    assert!(e["newText"].as_str().unwrap().len() <= 4, "{r}");

    // formatted already: no edits
    c.change(&uri, 5, &expected);
    assert_eq!(
        c.fmt_diagnostics(&uri),
        json!({"uri": uri, "version": 5, "diagnostics": []})
    );
    let r = c.format(&uri);
    assert_eq!(r["result"], json!([]), "{r}");

    // an edited config file applies at once
    std::fs::write(dir.join(".sparklesfmt.toml"), "indent-width = 2\n").unwrap();
    let r = c.format(&uri);
    assert_eq!(
        apply(&expected, &r["result"], true),
        fmt_cli(d.path(), &path, text)
    );
    // and a broken one is shown: a warning, and an error for formatting
    std::fs::write(dir.join(".sparklesfmt.toml"), "line-widht = 80\n").unwrap();
    c.change(&uri, 6, text);
    let p = c.fmt_diagnostics(&uri);
    assert_eq!(p["diagnostics"][0]["severity"], 2, "{p}");
    assert!(
        p["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .ends_with(".sparklesfmt.toml: error: line-widht: unknown option")
    );
    let r = c.format(&uri);
    assert_eq!(r["error"]["code"], -32803, "{r}");

    // closing clears the diagnostics
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri}}),
    );
    assert_eq!(c.fmt_diagnostics(&uri)["diagnostics"], json!([]));
    let r = c.format(&uri);
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("is not open"),
        "{r}"
    );
    let r = c.request("textDocument/hover", json!({}));
    assert_eq!(r["error"]["code"], -32601, "{r}");

    assert_eq!(c.shutdown(), Some(0));
}

#[test]
fn utf8_positions_when_the_client_offers_them() {
    let d = tempfile::tempdir().unwrap();
    let mut c = Client::start(d.path(), &[]);
    let caps = c.initialize(&["utf-8", "utf-16"]);
    assert_eq!(caps["capabilities"]["positionEncoding"], "utf-8", "{caps}");
    // an untitled document: language by content, default options
    let uri = "untitled:Untitled-1";
    let bad = "SELECT * {\r\n  BIND(\"𝄞é\" AS 2) }";
    c.open(uri, "plaintext", bad);
    let p = c.diagnostics(uri);
    let two = bad.find(" 2)").unwrap() + 1;
    assert_eq!(
        p["diagnostics"][0]["range"]["start"],
        json!({"line": 1, "character": 19})
    );
    assert_eq!(
        p["diagnostics"][0]["range"]["start"],
        position_of(bad, two, false)
    );
    let text = "select * {?s ?p \"𝄞é\"}\r\n";
    c.change(uri, 2, text);
    let r = c.format(uri);
    let expected = sparkles_fmt::format(text, Language::Sparql, &Options::default())
        .unwrap()
        .text;
    assert_eq!(apply(text, &r["result"], false), expected);
    assert_eq!(c.shutdown(), Some(0));
}

/// Every language: formatted when this build formats it, refused with the formatter's
/// message otherwise (no diagnostics either way for a valid document).
#[test]
fn every_language_the_library_formats() {
    let d = tempfile::tempdir().unwrap();
    let mut c = Client::start(d.path(), &[]);
    c.initialize(&[]);
    for (lang, ext, text) in [
        (Language::Sparql, "rq", "select * {?s ?p ?o}"),
        (
            Language::Turtle,
            "ttl",
            "@prefix ex: <http://example.org/> .\nex:a   ex:b ex:c .",
        ),
        (
            Language::TriG,
            "trig",
            "GRAPH <http://example.org/g> { <http://example.org/a> <http://example.org/b> 1 }",
        ),
        (
            Language::NTriples,
            "nt",
            "<http://example.org/a>   <http://example.org/b> \"c\"@EN .",
        ),
        (
            Language::NQuads,
            "nq",
            "<http://example.org/a> <http://example.org/b>  <http://example.org/c> <http://example.org/g> .",
        ),
        (
            Language::JsonLd,
            "jsonld",
            r#"{"http://example.org/b": "c", "@id": "http://example.org/a"}"#,
        ),
    ] {
        let uri = uri(&d.path().join(format!("doc.{ext}")));
        // by the editor's language id and, for a second document, by the extension
        for (uri, id) in [
            (uri.clone(), lang.name()),
            (format!("{uri}.x.{ext}"), "plaintext"),
        ] {
            c.open(&uri, id, text);
            let p = c.diagnostics(&uri);
            assert_eq!(p["diagnostics"], json!([]), "{lang:?}: {p}");
            let r = c.format(&uri);
            if lang.is_implemented() {
                let expected = sparkles_fmt::format(text, lang, &Options::default())
                    .unwrap()
                    .text;
                assert_eq!(apply(text, &r["result"], true), expected, "{lang:?}");
            } else {
                assert_eq!(r["error"]["code"], -32803, "{lang:?}: {r}");
                assert_eq!(
                    r["error"]["message"],
                    format!("{} formatting is not available yet", lang.name()),
                    "{r}"
                );
            }
        }
    }
    // RDF/XML is never formatted
    let uri = uri(&d.path().join("x.rdf"));
    c.open(&uri, "xml", "<rdf:RDF/>");
    let r = c.format(&uri);
    assert!(
        r["error"]["message"].as_str().unwrap().contains("RDF/XML"),
        "{r}"
    );
    assert_eq!(c.shutdown(), Some(0));
}

/// The formatter's warnings, as (code, severity, start) of each diagnostic.
fn warnings(p: &Value) -> Vec<(String, u64, Value)> {
    p["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["source"] == "sparkles fmt")
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["severity"].as_u64().unwrap(),
                d["range"]["start"].clone(),
            )
        })
        .collect()
}

#[test]
fn formatter_warnings_are_diagnostics() {
    let d = tempfile::tempdir().unwrap();
    let mut c = Client::start(d.path(), &[]);
    c.initialize(&["utf-16"]);
    // an undeclared prefix (information) and a comment the formatter moves (warning),
    // both after characters outside the BMP
    let uri_q = uri(&d.path().join("q.rq"));
    let q = "PREFIX ex: <http://example.org/>\nSELECT * { ?s ex:p \"𝄞\" . ?s dc:x \"😀\"^^ # moved\n ex:t }\n";
    c.open(&uri_q, "sparql", q);
    let all = c.diagnostics(&uri_q);
    let (dc, comment) = (q.find("dc:x").unwrap(), q.find("# moved").unwrap());
    // the lint's error on the undeclared prefix takes the place of the formatter's note
    let undefined: Vec<&Value> = all["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "undefined-prefix" || d["code"] == "undeclared-prefix")
        .collect();
    assert_eq!(undefined.len(), 1, "{all}");
    assert_eq!(undefined[0]["source"], "sparkles lint");
    assert_eq!(undefined[0]["severity"], 1);
    assert_eq!(undefined[0]["range"]["start"], position_of(q, dc, true));
    assert!(
        undefined[0]["message"]
            .as_str()
            .unwrap()
            .contains("the prefix dc: is not declared"),
        "{all}"
    );
    let p = c.fmt_diagnostics_of(all);
    assert_eq!(
        warnings(&p),
        [(
            "comment-moved".into(),
            2,
            json!({"line": 1, "character": 41})
        )],
        "{p}"
    );
    assert_eq!(
        p["diagnostics"][0]["range"]["start"],
        position_of(q, comment, true)
    );
    // formatting still works, with the warnings
    let r = c.format(&uri_q);
    assert_eq!(r["result"].as_array().unwrap().len(), 1, "{r}");
    // fixed: declared, and the comment where it stays
    let fixed = "PREFIX dc: <http://purl.org/dc/terms/>\nPREFIX ex: <http://example.org/>\nSELECT * { ?s ex:p \"𝄞\" . ?s dc:x \"😀\"^^ex:t } # stays\n";
    c.change(&uri_q, 2, fixed);
    assert_eq!(
        c.fmt_diagnostics(&uri_q),
        json!({"uri": uri_q, "version": 2, "diagnostics": []})
    );
    // back again, then closed: cleared
    c.change(&uri_q, 3, q);
    assert_eq!(warnings(&c.diagnostics(&uri_q)).len(), 1);
    c.notify(
        "textDocument/didClose",
        json!({"textDocument": {"uri": uri_q}}),
    );
    assert_eq!(c.diagnostics(&uri_q)["diagnostics"], json!([]));

    // Turtle: a moved comment after an emoji
    let uri_t = uri(&d.path().join("g.ttl"));
    let t = "PREFIX ex: <http://example.org/>\nex:a ex:b \"😀\"^^ # moved\n ex:t .\n";
    c.open(&uri_t, "turtle", t);
    let p = c.fmt_diagnostics(&uri_t);
    assert_eq!(
        warnings(&p),
        [(
            "comment-moved".into(),
            2,
            json!({"line": 1, "character": 17})
        )],
        "{p}"
    );
    assert_eq!(
        p["diagnostics"][0]["range"]["start"],
        position_of(t, t.find('#').unwrap(), true)
    );
    c.change(
        &uri_t,
        2,
        "PREFIX ex: <http://example.org/>\nex:a ex:b \"😀\"^^ex:t . # stays\n",
    );
    assert_eq!(c.fmt_diagnostics(&uri_t)["diagnostics"], json!([]));

    // a warning without a position (a key this build does not act on yet) goes at 0:0
    let sub = d.path().join("conventional");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(
        sub.join(".sparklesfmt.toml"),
        "turtle-layout = \"conventional\"\n",
    )
    .unwrap();
    let uri_c = uri(&sub.join("c.ttl"));
    c.open(
        &uri_c,
        "turtle",
        "PREFIX ex: <http://example.org/>\nex:a ex:b ex:c .\n",
    );
    let p = c.fmt_diagnostics(&uri_c);
    if sparkles_fmt::turtle::print::CONVENTIONAL_IMPLEMENTED {
        assert_eq!(p["diagnostics"], json!([]), "{p}");
    } else {
        assert_eq!(
            warnings(&p),
            [(
                "option-not-implemented".into(),
                3,
                json!({"line": 0, "character": 0})
            )],
            "{p}"
        );
    }
    assert_eq!(c.shutdown(), Some(0));
}

#[test]
fn a_refused_output_is_a_warning_and_an_error() {
    let d = tempfile::tempdir().unwrap();
    let mut c = Client::start(d.path(), &[("SPARKLES_FMT_FAULT", "drop-token")]);
    c.initialize(&[]);
    let uri = uri(&d.path().join("q.rq"));
    c.open(&uri, "sparql", "SELECT ?s WHERE { ?s ?p ?o }");
    let p = c.diagnostics(&uri);
    let diag = &p["diagnostics"][0];
    assert_eq!(diag["severity"], 2, "{p}");
    assert_eq!(diag["code"], "unsafe-format", "{p}");
    assert_eq!(
        diag["message"],
        "formatter refused its own output (algebra differs); input left unchanged; please report"
    );
    let r = c.format(&uri);
    assert_eq!(r["error"]["code"], -32803, "{r}");
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("algebra differs")
    );
    assert_eq!(c.shutdown(), Some(0));
}

#[test]
fn exit_without_shutdown_fails() {
    let d = tempfile::tempdir().unwrap();
    let mut c = Client::start(d.path(), &[]);
    c.initialize(&[]);
    c.notify("exit", Value::Null);
    assert_eq!(c.wait(), Some(1));
    // the client going away is an error too
    let mut c = Client::start(d.path(), &[]);
    c.initialize(&[]);
    assert_eq!(c.wait(), Some(1));
}

/// `sparkles lint` in the language server: findings as diagnostics with the config
/// file's `[lint]` levels, a quick fix per safe finding, and `source.fixAll.sparkles`.
#[test]
fn lint_findings_and_fixes() {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join(".sparklesfmt.toml"),
        "[lint]\nunused-prefix = \"error\"\nsingle-use-variable = \"off\"\n",
    )
    .unwrap();
    let mut c = Client::start(d.path(), &[]);
    let caps = c.initialize(&["utf-16"]);
    assert_eq!(
        caps["capabilities"]["codeActionProvider"]["codeActionKinds"],
        json!(["quickfix", "source.fixAll.sparkles"]),
        "{caps}"
    );
    let uri = uri(&d.path().join("q.rq"));
    let q = "PREFIX ex: <http://example.org/>\nPREFIX foaf: <http://xmlns.com/foaf/0.1/>\nSELECT ?s ?n { ?s foaf:name ?n ; foaf:knows ?o FILTER(lang(?n) = \"en\" || ?n = \"😀\"@en-us) }\n";
    c.open(&uri, "sparql", q);
    let p = c.diagnostics(&uri);
    let lint: Vec<(String, u64)> = p["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["source"] == "sparkles lint")
        .map(|d| {
            (
                d["code"].as_str().unwrap().to_string(),
                d["severity"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        lint,
        [
            ("unused-prefix".to_string(), 1),
            ("language-tag-case".to_string(), 2)
        ],
        "{p}"
    );
    let tag = q.find("@en-us").unwrap();
    let diag = p["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "language-tag-case")
        .unwrap()
        .clone();
    assert_eq!(diag["range"]["start"], position_of(q, tag, true));
    assert_eq!(diag["range"]["end"], position_of(q, tag + 6, true));

    // the quick fix on the tag
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri}, "range": diag["range"],
               "context": {"diagnostics": [diag]}}),
    );
    let actions = r["result"].as_array().unwrap();
    let quick = actions
        .iter()
        .find(|a| a["kind"] == "quickfix")
        .unwrap_or_else(|| panic!("{r}"));
    assert_eq!(quick["title"], "Write @en-US");
    let edits = &quick["edit"]["changes"][&uri];
    assert_eq!(apply(q, edits, true), q.replace("@en-us", "@en-US"));
    // fix all: the unused prefix goes too
    let r = c.request(
        "textDocument/codeAction",
        json!({"textDocument": {"uri": uri},
               "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}},
               "context": {"diagnostics": [], "only": ["source.fixAll"]}}),
    );
    let actions = r["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{r}");
    assert_eq!(actions[0]["kind"], "source.fixAll.sparkles");
    let fixed = apply(q, &actions[0]["edit"]["changes"][&uri], true);
    assert_eq!(
        fixed,
        q.replace("PREFIX ex: <http://example.org/>\n", "")
            .replace("@en-us", "@en-US")
    );
    c.change(&uri, 2, &fixed);
    let p = c.diagnostics(&uri);
    assert_eq!(p["diagnostics"], json!([]), "{p}");
    assert_eq!(c.shutdown(), Some(0));
}
