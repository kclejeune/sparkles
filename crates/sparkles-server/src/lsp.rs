//! `sparkles lsp`: a language server over stdin and stdout (`lsp-server`, `lsp-types`)
//! for the languages `sparkles fmt` formats. It offers `textDocument/formatting` and
//! `textDocument/rangeFormatting` (the whole document, returned as the smallest edit), and
//! publishes syntax errors and the formatter's warnings as diagnostics. Each document is formatted with the options of
//! the `.sparklesfmt.toml` nearest to its file, as `sparkles fmt` would; there are no
//! editor-side settings.
//!
//! SPARQL, Turtle and TriG documents are also linted (`sparkles lint`, with the config
//! file's `[lint]` table), and the findings are published with the formatter's. A
//! finding with a safe fix offers it as a quick fix (`textDocument/codeAction`), and
//! `source.fixAll.sparkles` applies them all.
//!
//! Completion offers prefixes (spec C20 §8): declarations after `PREFIX` or `@prefix`,
//! and prefix names where a prefixed name starts, adding the declaration a completed
//! name needs. The prefixes come from [`sources`]. The lint's `undefined-prefix` finding
//! gets a quick fix that declares a known prefix.
//!
//! A document's language comes from its `languageId`, else its extension, else its
//! content. Languages this build does not format yet are refused when asked to format
//! and get no diagnostics.

mod complete;
mod sources;
mod text;

use crate::fmt::{config, report};
use anyhow::{Context, Result, bail};
use clap::Args;
use lsp_server::{Connection, ErrorCode, Message, Notification, Request, RequestId, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Exit, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, Formatting, RangeFormatting, Request as _,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams, CompletionItem,
    CompletionItemKind, CompletionParams, CompletionResponse, CompletionTextEdit, Diagnostic,
    DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentRangeFormattingParams,
    NumberOrString, Position, PublishDiagnosticsParams, Range, TextEdit, Uri, WorkspaceEdit,
};
use serde_json::{Value, json};
use sparkles_fmt::lint::{self, LintOptions, Linted, Severity};
use sparkles_fmt::{Detection, FormatError, Formatted, Language, Options};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use text::Encoding;

#[derive(Args, Debug)]
pub struct LspArgs {
    /// Talk over stdin and stdout (the only transport; accepted because editors pass it)
    #[arg(long)]
    pub stdio: bool,
}

/// How long one formatting may take before it gives up.
const DEADLINE: Duration = Duration::from_secs(10);

/// The code action kind that applies every safe lint fix.
const FIX_ALL: &str = "source.fixAll.sparkles";

/// Serve until the client says `exit`. Exiting without `shutdown` first, or the client
/// closing the connection, is an error (exit status 1, as the protocol asks).
pub fn run(args: LspArgs) -> Result<()> {
    let _ = args.stdio;
    let (conn, io) = Connection::stdio();
    let served = serve(&conn);
    drop(conn);
    // on an error the reader may still wait for input: leave without it
    served?;
    io.join().context("the client connection")
}

fn serve(conn: &Connection) -> Result<()> {
    let (id, params) = conn.initialize_start()?;
    let offered = |name: &str| {
        params["capabilities"]["general"]["positionEncodings"]
            .as_array()
            .is_some_and(|a| a.iter().any(|e| e == name))
    };
    let encoding = if offered("utf-8") {
        Encoding::Utf8
    } else {
        Encoding::Utf16
    };
    conn.initialize_finish(
        id,
        json!({
            "capabilities": {
                "positionEncoding": encoding.name(),
                // open/close and the full text on every change
                "textDocumentSync": {"openClose": true, "change": 1},
                "documentFormattingProvider": true,
                "documentRangeFormattingProvider": true,
                "codeActionProvider": {
                    "codeActionKinds": ["quickfix", FIX_ALL],
                },
                "completionProvider": {},
            },
            "serverInfo": {"name": "sparkles", "version": env!("CARGO_PKG_VERSION")},
        }),
    )?;
    let mut server = Server {
        encoding,
        docs: HashMap::new(),
        stale: BTreeSet::new(),
        sources: sources::Sources::default(),
    };
    loop {
        // diagnostics wait until the client has nothing more queued, so a burst of
        // changes is checked once
        let msg = match conn.receiver.try_recv() {
            Ok(msg) => msg,
            Err(_) => {
                server.publish(conn)?;
                match conn.receiver.recv() {
                    Ok(msg) => msg,
                    Err(_) => bail!("the client closed the connection without exit"),
                }
            }
        };
        match msg {
            Message::Request(req) => {
                if conn.handle_shutdown(&req)? {
                    return Ok(());
                }
                let resp = server.request(req);
                conn.sender.send(resp.into())?;
            }
            Message::Notification(n) if n.method == Exit::METHOD => {
                bail!("exit notification before shutdown")
            }
            Message::Notification(n) => server.notification(n),
            Message::Response(_) => {}
        }
    }
}

/// An open document.
struct Doc {
    text: String,
    language_id: String,
    version: i32,
    /// the formatting of `text` and the options (or config error) it was made with
    result: Option<(Result<Options, String>, Result<Formatted, Refusal>)>,
    /// the lint findings of `text` and the rule levels they were made with; `None`
    /// inside for a language lint does not take
    linted: Option<(Result<LintOptions, String>, Option<Linted>)>,
}

/// Why a document was not formatted.
#[derive(Clone, Debug)]
enum Refusal {
    /// the formatter's own answer
    Format(Language, FormatError),
    /// the nearest config file is broken (its message names it)
    Config(String),
    /// no language, or one this build does not format
    Language(String),
    /// the formatter panicked (a bug)
    Crashed,
}

impl Refusal {
    fn message(&self) -> String {
        match self {
            Refusal::Format(_, FormatError::Unsupported { message, .. }) => format!(
                "the formatter cannot handle this yet ({message}); input left unchanged; please report"
            ),
            Refusal::Format(lang, FormatError::Syntax { message, .. }) => format!(
                "{} syntax error: {}",
                lang.display_name(),
                report::short_message(message)
            ),
            Refusal::Format(_, e) => e.to_string(),
            Refusal::Config(m) | Refusal::Language(m) => m.clone(),
            Refusal::Crashed => {
                "the formatter crashed on this document; input left unchanged; please report"
                    .to_string()
            }
        }
    }
}

struct Server {
    encoding: Encoding,
    docs: HashMap<Uri, Doc>,
    /// documents whose diagnostics are out of date
    stale: BTreeSet<Uri>,
    /// the prefixes of servers that config files name
    sources: sources::Sources,
}

impl Server {
    fn notification(&mut self, n: Notification) {
        match n.method.as_str() {
            DidOpenTextDocument::METHOD => {
                let Ok(p) = n.extract::<DidOpenTextDocumentParams>(DidOpenTextDocument::METHOD)
                else {
                    return;
                };
                let d = p.text_document;
                self.docs.insert(
                    d.uri.clone(),
                    Doc {
                        text: d.text,
                        language_id: d.language_id,
                        version: d.version,
                        result: None,
                        linted: None,
                    },
                );
                // a server that the config file names is read now, in the background
                self.sources.known(&editor(&d.uri));
                self.stale.insert(d.uri);
            }
            DidChangeTextDocument::METHOD => {
                let Ok(p) = n.extract::<DidChangeTextDocumentParams>(DidChangeTextDocument::METHOD)
                else {
                    return;
                };
                let Some(doc) = self.docs.get_mut(&p.text_document.uri) else {
                    return;
                };
                for change in p.content_changes {
                    match change.range {
                        // full sync, as announced
                        None => doc.text = change.text,
                        // a client sending ranges anyway
                        Some(r) => {
                            let at = |p: Position| {
                                text::offset(&doc.text, p.line, p.character, self.encoding)
                            };
                            let (start, end) = (at(r.start), at(r.end));
                            doc.text.replace_range(start..end.max(start), &change.text);
                        }
                    }
                }
                doc.version = p.text_document.version;
                doc.result = None;
                doc.linted = None;
                self.stale.insert(p.text_document.uri);
            }
            DidCloseTextDocument::METHOD => {
                let Ok(p) = n.extract::<DidCloseTextDocumentParams>(DidCloseTextDocument::METHOD)
                else {
                    return;
                };
                self.docs.remove(&p.text_document.uri);
                // cleared when diagnostics are next published
                self.stale.insert(p.text_document.uri);
            }
            _ => {}
        }
    }

    fn request(&mut self, req: Request) -> Response {
        let id = req.id.clone();
        let uri = match req.method.as_str() {
            Formatting::METHOD => req
                .extract::<DocumentFormattingParams>(Formatting::METHOD)
                .map(|(_, p)| p.text_document.uri),
            // the whole document, as the minimal edit: a formatted range would depend on
            // what lies around it
            RangeFormatting::METHOD => req
                .extract::<DocumentRangeFormattingParams>(RangeFormatting::METHOD)
                .map(|(_, p)| p.text_document.uri),
            Completion::METHOD => {
                return match req.extract::<CompletionParams>(Completion::METHOD) {
                    Ok((_, p)) => self.completion(id, &p),
                    Err(_) => {
                        Response::new_err(id, ErrorCode::InvalidParams as i32, "bad params".into())
                    }
                };
            }
            CodeActionRequest::METHOD => {
                return match req.extract::<CodeActionParams>(CodeActionRequest::METHOD) {
                    Ok((_, p)) => self.code_actions(id, &p),
                    Err(_) => {
                        Response::new_err(id, ErrorCode::InvalidParams as i32, "bad params".into())
                    }
                };
            }
            m => {
                return Response::new_err(
                    id,
                    ErrorCode::MethodNotFound as i32,
                    format!("unknown method {m}"),
                );
            }
        };
        let Ok(uri) = uri else {
            return Response::new_err(id, ErrorCode::InvalidParams as i32, "bad params".into());
        };
        self.format(id, &uri)
    }

    /// The edits that format the document `uri`: none when it is formatted, one minimal
    /// edit otherwise. A syntax error answers `null` (its diagnostic says why); any other
    /// refusal is an error the editor shows.
    fn format(&mut self, id: RequestId, uri: &Uri) -> Response {
        let encoding = self.encoding;
        let Some(doc) = self.docs.get_mut(uri) else {
            return Response::new_err(
                id,
                ErrorCode::InvalidParams as i32,
                format!("{} is not open", uri.as_str()),
            );
        };
        doc.format(uri);
        match &doc.result.as_ref().expect("formatted").1 {
            Ok(f) => {
                let edits: Vec<TextEdit> = text::minimal_edit(&doc.text, &f.text)
                    .map(|e| TextEdit {
                        range: range(&doc.text, e.start, e.end, encoding),
                        new_text: e.insert,
                    })
                    .into_iter()
                    .collect();
                Response::new_ok(id, edits)
            }
            Err(Refusal::Format(_, FormatError::Syntax { .. })) => {
                Response::new_ok(id, Value::Null)
            }
            Err(r) => Response::new_err(id, ErrorCode::RequestFailed as i32, r.message()),
        }
    }

    /// The prefixes to complete at a position (C20 §8). A document in a language without
    /// prefixes, or one that is not open, gets none.
    fn completion(&mut self, id: RequestId, p: &CompletionParams) -> Response {
        let encoding = self.encoding;
        let pos = &p.text_document_position;
        let uri = &pos.text_document.uri;
        let Some(doc) = self.docs.get(uri) else {
            return Response::new_ok(id, CompletionResponse::Array(Vec::new()));
        };
        let lang = match language(&doc.language_id, uri.as_str(), &doc.text) {
            Ok(l) if complete::completes(l) => l,
            _ => return Response::new_ok(id, CompletionResponse::Array(Vec::new())),
        };
        let text = &doc.text;
        let at = text::offset(text, pos.position.line, pos.position.character, encoding);
        let known = self.sources.known(&editor(uri));
        let edit = |e: &complete::Edit| TextEdit {
            range: range(text, e.start, e.end, encoding),
            new_text: e.insert.clone(),
        };
        let items: Vec<CompletionItem> = complete::complete(text, at, lang, &known)
            .into_iter()
            .map(|i| CompletionItem {
                label: i.label.clone(),
                kind: Some(CompletionItemKind::MODULE),
                detail: Some(i.iri.clone()),
                filter_text: Some(i.label.clone()),
                text_edit: Some(CompletionTextEdit::Edit(edit(&i.edit))),
                additional_text_edits: i.declare.as_ref().map(|d| vec![edit(d)]),
                ..CompletionItem::default()
            })
            .collect();
        Response::new_ok(id, CompletionResponse::Array(items))
    }

    /// The quick fixes of the lint findings in the requested range, and the action that
    /// applies every safe fix.
    fn code_actions(&mut self, id: RequestId, p: &CodeActionParams) -> Response {
        let encoding = self.encoding;
        let uri = &p.text_document.uri;
        let Some(doc) = self.docs.get_mut(uri) else {
            return Response::new_ok(id, Vec::<CodeActionOrCommand>::new());
        };
        let Some(linted) = doc.lint(uri).clone() else {
            return Response::new_ok(id, Vec::<CodeActionOrCommand>::new());
        };
        let text = doc.text.clone();
        let wanted = |k: &str| {
            p.context
                .only
                .as_ref()
                .is_none_or(|only| only.iter().any(|o| k.starts_with(o.as_str())))
        };
        let edit = |edits: Vec<TextEdit>| WorkspaceEdit {
            changes: Some(std::iter::once((uri.clone(), edits)).collect()),
            ..WorkspaceEdit::default()
        };
        let mut actions = Vec::new();
        if wanted("quickfix") {
            for d in &linted.diagnostics {
                let Some(fix) = &d.fix else { continue };
                if !lint::rule(d.rule).is_some_and(|r| r.safe_fix) {
                    continue;
                }
                let r = range(&text, d.start, d.end, encoding);
                if r.end < p.range.start || p.range.end < r.start {
                    continue;
                }
                let edits = fix
                    .edits
                    .iter()
                    .map(|e| TextEdit {
                        range: range(&text, e.start, e.end, encoding),
                        new_text: e.insert.clone(),
                    })
                    .collect();
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: fix.title.clone(),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lint_diagnostic(&text, d, encoding)]),
                    edit: Some(edit(edits)),
                    is_preferred: Some(true),
                    ..CodeAction::default()
                }));
            }
            // an undefined prefix that a source knows: declare it (C20 §8); the IRI comes
            // from outside the document, so `source.fixAll` leaves it out
            let mut known = None;
            let mut offered = BTreeSet::new();
            for d in linted
                .diagnostics
                .iter()
                .filter(|d| d.rule == "undefined-prefix")
            {
                let r = range(&text, d.start, d.end, encoding);
                if r.end < p.range.start || p.range.end < r.start {
                    continue;
                }
                let span = &text[d.start..d.end];
                let label = &span[..span.find(':').unwrap_or(span.len())];
                if !offered.insert(label.to_string()) {
                    continue;
                }
                let known = known.get_or_insert_with(|| self.sources.known(&editor(uri)));
                let Some(e) = complete::declare(&text, linted.language, label, known) else {
                    continue;
                };
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: format!("Declare {label}: as <{}>", known[label]),
                    kind: Some(CodeActionKind::QUICKFIX),
                    diagnostics: Some(vec![lint_diagnostic(&text, d, encoding)]),
                    edit: Some(edit(vec![TextEdit {
                        range: range(&text, e.start, e.end, encoding),
                        new_text: e.insert,
                    }])),
                    is_preferred: Some(true),
                    ..CodeAction::default()
                }));
            }
        }
        let fixable = linted
            .diagnostics
            .iter()
            .any(|d| d.fix.is_some() && lint::rule(d.rule).is_some_and(|r| r.safe_fix));
        if fixable && wanted(FIX_ALL) {
            let opts = doc.lint_options(uri);
            if let (Ok(opts), Some(lang)) = (opts, Some(linted.language))
                && let Ok(fixed) = lint::fix(&text, lang, &opts)
                && let Some(e) = text::minimal_edit(&text, &fixed.text)
            {
                actions.push(CodeActionOrCommand::CodeAction(CodeAction {
                    title: "Fix all safe lint problems".to_string(),
                    kind: Some(CodeActionKind::from(FIX_ALL.to_string())),
                    edit: Some(edit(vec![TextEdit {
                        range: range(&text, e.start, e.end, encoding),
                        new_text: e.insert,
                    }])),
                    ..CodeAction::default()
                }));
            }
        }
        Response::new_ok(id, actions)
    }

    /// Publish the diagnostics of every document that changed since they were last sent
    /// (an empty list for a closed one).
    fn publish(&mut self, conn: &Connection) -> Result<()> {
        for uri in std::mem::take(&mut self.stale) {
            let (diagnostics, version) = match self.docs.get_mut(&uri) {
                Some(doc) => (doc.diagnostics(&uri, self.encoding), Some(doc.version)),
                None => (Vec::new(), None),
            };
            let params = PublishDiagnosticsParams {
                uri,
                diagnostics,
                version,
            };
            conn.sender
                .send(Notification::new(PublishDiagnostics::METHOD.to_string(), params).into())?;
        }
        Ok(())
    }
}

impl Doc {
    /// The formatted document, computed again only when the text or the options changed
    /// (a config file is read every time, so an edit to it applies at once).
    fn format(&mut self, uri: &Uri) -> &Result<Formatted, Refusal> {
        let opts = options(uri);
        if !matches!(&self.result, Some((o, _)) if *o == opts) {
            let r = format_doc(uri, &self.language_id, &self.text, &opts);
            self.result = Some((opts, r));
        }
        &self.result.as_ref().expect("just set").1
    }

    /// The rule levels of a document, from the nearest config file's `[lint]` table.
    fn lint_options(&self, uri: &Uri) -> Result<LintOptions, String> {
        match file_path(uri.as_str()).as_deref().and_then(Path::parent) {
            Some(dir) => config::lint_for_dir(dir),
            None => Ok(LintOptions::default()),
        }
    }

    /// The lint findings of the document, computed again only when the text or the rule
    /// levels changed. `None` for a language lint does not take, or when linting failed.
    fn lint(&mut self, uri: &Uri) -> &Option<Linted> {
        let opts = self.lint_options(uri);
        if !matches!(&self.linted, Some((o, _)) if *o == opts) {
            let linted = match (&opts, language(&self.language_id, uri.as_str(), &self.text)) {
                (Ok(o), Ok(lang)) if lint::lints(lang) => {
                    let mut o = o.clone();
                    o.deadline = Some(Instant::now() + DEADLINE);
                    let text = &self.text;
                    std::panic::catch_unwind(|| lint::lint(text, lang, &o))
                        .ok()
                        .and_then(Result::ok)
                }
                _ => None,
            };
            self.linted = Some((opts, linted));
        }
        &self.linted.as_ref().expect("just set").1
    }

    /// The formatter's diagnostics, then the lint findings (syntax errors once).
    fn diagnostics(&mut self, uri: &Uri, enc: Encoding) -> Vec<Diagnostic> {
        let mut out = self.format_diagnostics(uri, enc);
        if let Some(l) = self.lint(uri).clone() {
            // the lint's own findings replace the formatter's note on undeclared prefixes
            out.retain(|d| d.code != Some(NumberOrString::String("undeclared-prefix".to_string())));
            out.extend(
                l.diagnostics
                    .iter()
                    .filter(|d| d.rule != "syntax")
                    .map(|d| lint_diagnostic(&self.text, d, enc)),
            );
        }
        out
    }

    /// The formatter's warnings for a formatted document; otherwise a syntax error, the
    /// formatter refusing the document, or a broken config file.
    fn format_diagnostics(&mut self, uri: &Uri, enc: Encoding) -> Vec<Diagnostic> {
        self.format(uri);
        let text = &self.text;
        let refusal = match &self.result.as_ref().expect("formatted").1 {
            Ok(f) => {
                return f
                    .warnings
                    .iter()
                    .map(|w| {
                        // no position: the start of the document
                        let start = match w.line {
                            0 => 0,
                            line => sparkles_fmt::offset_of(text, line, w.column),
                        };
                        let severity = warning_severity(w.code);
                        diagnostic(text, start, enc, severity, Some(w.code), w.message.clone())
                    })
                    .collect();
            }
            Err(r) => r,
        };
        let (severity, start) = match refusal {
            Refusal::Format(_, FormatError::Syntax { offset, .. }) => {
                (DiagnosticSeverity::ERROR, *offset)
            }
            Refusal::Format(_, FormatError::Unsupported { line, column, .. }) => (
                DiagnosticSeverity::WARNING,
                sparkles_fmt::offset_of(text, *line, *column),
            ),
            Refusal::Format(_, FormatError::Unsafe { .. })
            | Refusal::Config(_)
            | Refusal::Crashed => (DiagnosticSeverity::WARNING, 0),
            // nothing to point at in the document
            Refusal::Format(..) | Refusal::Language(_) => return Vec::new(),
        };
        let code = match refusal {
            Refusal::Format(_, e) => Some(e.code()),
            Refusal::Config(_) => Some("config"),
            _ => None,
        };
        vec![diagnostic(
            text,
            start,
            enc,
            severity,
            code,
            refusal.message(),
        )]
    }
}

/// How much a formatter warning matters: a moved comment means the output may surprise
/// the reader, so it is a warning; the others (`undeclared-prefix`,
/// `option-not-implemented`) are information. (`comments-dropped` and `unstable-labels`
/// come only from `canonicalize`, which the server never sets.)
fn warning_severity(code: &str) -> DiagnosticSeverity {
    match code {
        "comment-moved" => DiagnosticSeverity::WARNING,
        _ => DiagnosticSeverity::INFORMATION,
    }
}

/// A lint finding as an LSP diagnostic, over its whole range.
fn lint_diagnostic(text: &str, d: &lint::Diagnostic, enc: Encoding) -> Diagnostic {
    let severity = match d.severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    };
    Diagnostic {
        range: range(text, d.start, d.end, enc),
        severity: Some(severity),
        code: Some(NumberOrString::String(d.rule.to_string())),
        source: Some("sparkles lint".to_string()),
        message: d.message.clone(),
        ..Diagnostic::default()
    }
}

/// A diagnostic on the character at byte `start` (empty at a line break or the end).
fn diagnostic(
    text: &str,
    start: usize,
    enc: Encoding,
    severity: DiagnosticSeverity,
    code: Option<&str>,
    message: String,
) -> Diagnostic {
    let mut start = start.min(text.len());
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    let end = text[start..]
        .chars()
        .next()
        .filter(|c| !matches!(c, '\n' | '\r'))
        .map_or(start, |c| start + c.len_utf8());
    Diagnostic {
        range: range(text, start, end, enc),
        severity: Some(severity),
        code: code.map(|c| NumberOrString::String(c.to_string())),
        source: Some("sparkles fmt".to_string()),
        message,
        ..Diagnostic::default()
    }
}

fn range(text: &str, start: usize, end: usize, enc: Encoding) -> Range {
    let pos = |b: usize| {
        let (line, character) = text::position(text, b, enc);
        Position { line, character }
    };
    Range {
        start: pos(start),
        end: pos(end),
    }
}

/// The options of a document: from the config file nearest to its path, the defaults
/// for a document that is not a local file.
fn options(uri: &Uri) -> Result<Options, String> {
    match file_path(uri.as_str()).as_deref().and_then(Path::parent) {
        Some(dir) => config::options_for_dir(dir),
        None => Ok(Options::default()),
    }
}

/// The `[prefixes]` and `[lsp]` tables of a document's config file. A broken file gives
/// none, and its error is published with the formatter's diagnostics.
fn editor(uri: &Uri) -> config::Editor {
    file_path(uri.as_str())
        .as_deref()
        .and_then(Path::parent)
        .and_then(|dir| config::editor_for_dir(dir).ok())
        .unwrap_or_default()
}

/// Format a document with its options.
fn format_doc(
    uri: &Uri,
    language_id: &str,
    text: &str,
    opts: &Result<Options, String>,
) -> Result<Formatted, Refusal> {
    let lang = language(language_id, uri.as_str(), text).map_err(Refusal::Language)?;
    let mut opts = opts.clone().map_err(Refusal::Config)?;
    opts.deadline = Some(Instant::now() + DEADLINE);
    // a formatter bug must not take the editor's server down with it
    match std::panic::catch_unwind(|| sparkles_fmt::format(text, lang, &opts)) {
        Ok(r) => r.map_err(|e| Refusal::Format(lang, e)),
        Err(_) => Err(Refusal::Crashed),
    }
}

/// The language of a document: its `languageId`, else the extension of its URI, else its
/// content. Refuses RDF/XML, compressed files and extensions that need a language.
fn language(language_id: &str, uri: &str, text: &str) -> Result<Language, String> {
    let by_id = match language_id.to_ascii_lowercase().as_str() {
        "ttl" => Some(Language::Turtle),
        "json-ld" => Some(Language::JsonLd),
        id => Language::from_name(id),
    };
    if let Some(l) = by_id {
        return Ok(l);
    }
    // the last path segment of any URI scheme (`untitled:x.rq` too)
    let path = uri.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit(['/', ':']).next().unwrap_or("");
    let name = percent_encoding::percent_decode_str(name).decode_utf8_lossy();
    let detected = match sparkles_fmt::detect_path(Path::new(name.as_ref())) {
        Some(d) => d,
        None => sparkles_fmt::detect(None, text),
    };
    match detected {
        Detection::Lang(l) => Ok(l),
        Detection::RdfXml => Err(sparkles_fmt::RDF_XML_MESSAGE.to_string()),
        Detection::Compressed => Err("compressed input: decompress first".to_string()),
        Detection::SkipInWalk => Err(format!(
            "cannot tell the language of {name}; set the editor's language to one of \
             sparql, turtle, trig, ntriples, nquads or jsonld"
        )),
        Detection::Unknown => Err("cannot tell the language of this document".to_string()),
    }
}

/// The local path of a `file:` URI.
fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))?;
    // an empty or `localhost` authority; other hosts are not local
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(i) if rest[..i].eq_ignore_ascii_case("localhost") => &rest[i..],
        _ => return None,
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()?;
    // `/C:/dir` on Windows
    #[cfg(windows)]
    let decoded = match decoded.strip_prefix('/') {
        Some(p) if p.as_bytes().get(1) == Some(&b':') => std::borrow::Cow::Owned(p.to_string()),
        _ => decoded,
    };
    Some(PathBuf::from(decoded.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        let l = |id: &str, uri: &str, text: &str| language(id, uri, text);
        assert_eq!(l("sparql", "file:///x.txt", ""), Ok(Language::Sparql));
        assert_eq!(l("ttl", "file:///x", ""), Ok(Language::Turtle));
        assert_eq!(l("json-ld", "file:///x", ""), Ok(Language::JsonLd));
        assert_eq!(l("TriG", "file:///x", ""), Ok(Language::TriG));
        // an unknown id: the extension, then the content
        assert_eq!(l("plaintext", "file:///q.ru", ""), Ok(Language::Sparql));
        assert_eq!(l("", "untitled:Untitled-1.nq", ""), Ok(Language::NQuads));
        assert_eq!(
            l("plaintext", "untitled:Untitled-1", "SELECT * {}"),
            Ok(Language::Sparql)
        );
        assert_eq!(
            l("plaintext", "file:///d/a%20b.TTL", ""),
            Ok(Language::Turtle)
        );
        assert!(
            l("xml", "file:///x.owl", "")
                .unwrap_err()
                .contains("RDF/XML")
        );
        assert!(
            l("json", "file:///x.json", "{}")
                .unwrap_err()
                .contains("x.json")
        );
        assert!(
            l("", "file:///x.nt.gz", "")
                .unwrap_err()
                .contains("decompress")
        );
        assert!(l("", "file:///x", "  # only a comment\n").is_err());
    }

    #[test]
    fn file_paths() {
        assert_eq!(
            file_path("file:///home/u/a%20b/q.rq"),
            Some(PathBuf::from("/home/u/a b/q.rq"))
        );
        assert_eq!(
            file_path("file://localhost/srv/q.rq"),
            Some(PathBuf::from("/srv/q.rq"))
        );
        assert_eq!(file_path("file://server/share/q.rq"), None);
        assert_eq!(file_path("untitled:Untitled-1"), None);
        assert_eq!(file_path("file:///q%FF.rq"), None);
    }
}
