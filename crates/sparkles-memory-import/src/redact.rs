//! Redaction (spec C18 §8.10.7 and item 77): every imported or server-ingested text
//! passes a [`Policy`] before it leaves the machine or reaches a model provider. Each
//! match is replaced with `[redacted:<detector>]`.
//!
//! A policy combines:
//! - built-in and dataset regular expression [`Pattern`]s;
//! - optional high-entropy token detection (`entropy`, off by default) for values in
//!   assignments, headers, URL query parameters and quoted strings, excluding 40- and
//!   64-character hex hashes and canonical UUIDs;
//! - optional personal-data detectors (`personal`: `email`, `phone`, `payment-card` with
//!   Luhn check, `iban` with ISO 13616 check digits, and `ip`);
//! - exact known-value matching (`known_values`, 8+ characters) via Aho-Corasick;
//! - an allow-list of regular expressions whose matched spans are never redacted.

use aho_corasick::AhoCorasick;
use regex::Regex;
use std::collections::BTreeMap;
use std::convert::Infallible;
use std::net::Ipv6Addr;
use std::sync::LazyLock;

/// The shortest known value that exact matching looks for, in characters.
pub const MIN_KNOWN_VALUE_CHARS: usize = 8;

/// A named pattern. When the expression has a group named `v`, only that group is
/// replaced, so `Authorization: Bearer x` keeps its header name.
#[derive(Clone, Debug)]
pub struct Pattern {
    pub name: String,
    pub regex: Regex,
}

impl Pattern {
    /// A pattern from a name and an expression in the syntax of Rust's `regex` crate.
    pub fn new(name: &str, expr: &str) -> Result<Pattern, String> {
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        {
            return Err(format!(
                "invalid pattern name {name:?}: use 1 to 64 letters, digits, - or _"
            ));
        }
        let regex = Regex::new(expr).map_err(|e| format!("pattern {name}: {e}"))?;
        Ok(Pattern {
            name: name.to_string(),
            regex,
        })
    }
}

/// The built-in list of §8.10.7, in priority order: an earlier pattern wins over a later
/// one that matches at the same place.
pub fn builtin() -> Vec<Pattern> {
    const LIST: &[(&str, &str)] = &[
        (
            "private-key",
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        ("aws-access-key-id", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
        (
            "aws-secret-key",
            r#"(?i)aws_?secret_?access_?key\s*[:=]\s*['"]?(?P<v>[A-Za-z0-9/+=]{40})"#,
        ),
        (
            "github-token",
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36,}\b|\bgithub_pat_[A-Za-z0-9_]{22,}",
        ),
        ("slack-token", r"\bxox[abposr]-[A-Za-z0-9-]{10,}"),
        ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}"),
        ("openai-key", r"\bsk-[A-Za-z0-9_-]{20,}"),
        ("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}"),
        (
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        ),
        (
            "authorization-header",
            r"(?i)authorization\s*:\s*(?:bearer|basic)\s+(?P<v>[A-Za-z0-9._~+/=-]{8,})",
        ),
        (
            "url-password",
            r"\b[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:(?P<v>[^\s@/]+)@",
        ),
        (
            "secret-assignment",
            r#"(?i)\b[A-Za-z0-9_.-]*(?:key|secret|token|password|passwd)[A-Za-z0-9_.-]*["']?\s*[:=]\s*["']?(?P<v>[^\s"'`,;]{12,})"#,
        ),
    ];
    LIST.iter()
        .map(|(n, e)| Pattern::new(n, e).expect("the built-in patterns compile"))
        .collect()
}

/// High-entropy token detection thresholds (C18 item 77).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntropyConfig {
    pub min_length: usize,
    pub min_bits_per_char: f64,
}

impl Default for EntropyConfig {
    fn default() -> EntropyConfig {
        EntropyConfig {
            min_length: 20,
            min_bits_per_char: 4.0,
        }
    }
}

/// Opt-in personal data categories (C18 item 77).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PersonalCategory {
    Email,
    Phone,
    PaymentCard,
    Iban,
    Ip,
}

impl PersonalCategory {
    pub const ALL: [PersonalCategory; 5] = [
        PersonalCategory::Email,
        PersonalCategory::Phone,
        PersonalCategory::PaymentCard,
        PersonalCategory::Iban,
        PersonalCategory::Ip,
    ];

    pub fn parse(s: &str) -> Option<PersonalCategory> {
        match s {
            "email" => Some(PersonalCategory::Email),
            "phone" => Some(PersonalCategory::Phone),
            "payment-card" => Some(PersonalCategory::PaymentCard),
            "iban" => Some(PersonalCategory::Iban),
            "ip" => Some(PersonalCategory::Ip),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            PersonalCategory::Email => "email",
            PersonalCategory::Phone => "phone",
            PersonalCategory::PaymentCard => "payment-card",
            PersonalCategory::Iban => "iban",
            PersonalCategory::Ip => "ip",
        }
    }
}

/// One unified redaction policy shared by the CLI, `register_source` and server
/// ingestion. Known values are kept in memory only for matching and are never printed
/// by `Debug`.
#[derive(Clone)]
pub struct Policy {
    pub patterns: Vec<Pattern>,
    pub entropy: Option<EntropyConfig>,
    pub personal: Vec<PersonalCategory>,
    pub allow: Vec<Regex>,
    known_matcher: Option<AhoCorasick>,
    known_count: usize,
    pub known_label: String,
}

impl std::fmt::Debug for Policy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Policy")
            .field("patterns", &self.patterns.len())
            .field("entropy", &self.entropy)
            .field("personal", &self.personal)
            .field("allow", &self.allow.len())
            .field("known_values", &self.known_count)
            .field("known_label", &self.known_label)
            .finish()
    }
}

impl Default for Policy {
    fn default() -> Policy {
        Policy::from_patterns(builtin())
    }
}

impl Policy {
    /// A policy with only `patterns` (built-in and any dataset/user patterns).
    pub fn from_patterns(patterns: Vec<Pattern>) -> Policy {
        Policy {
            patterns,
            entropy: None,
            personal: Vec::new(),
            allow: Vec::new(),
            known_matcher: None,
            known_count: 0,
            known_label: "known-value".into(),
        }
    }

    /// Add exact known values to match via Aho-Corasick. Values shorter than
    /// [`MIN_KNOWN_VALUE_CHARS`] characters are skipped.
    pub fn with_known_values<I, S>(mut self, values: I, label: impl Into<String>) -> Policy
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut kept: Vec<String> = values
            .into_iter()
            .map(|s| s.as_ref().trim().to_string())
            .filter(|s| s.chars().count() >= MIN_KNOWN_VALUE_CHARS)
            .collect();
        kept.sort();
        kept.dedup();
        self.known_label = label.into();
        self.known_count = kept.len();
        self.known_matcher = if kept.is_empty() {
            None
        } else {
            AhoCorasick::new(&kept).ok()
        };
        self
    }

    /// Replace every match of this policy in `text`.
    pub fn redact(&self, text: &str) -> Redacted {
        self.redact_with_edits(text).0
    }

    /// Replace every match in `text` and return the byte-offset edits
    /// `(orig_start, orig_end, new_start, new_end)` in order, so callers can remap
    /// character offsets such as PDF page starts.
    pub fn redact_with_edits(&self, text: &str) -> (Redacted, Vec<Edit>) {
        self.redact_checked(text, || Ok::<_, Infallible>(()))
            .unwrap()
    }

    /// Redact text, checking the caller's deadline or cancellation during matching.
    pub fn redact_checked<E>(
        &self,
        text: &str,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<(Redacted, Vec<Edit>), E> {
        let mut found = BTreeMap::new();
        self.visit(text, &mut check, |candidate| {
            merge_match(&mut found, candidate)
        })?;

        let mut out = String::with_capacity(text.len());
        let mut names = Vec::new();
        let mut edits = Vec::new();
        let mut at = 0;
        for (start, (end, _, name)) in found {
            check()?;
            out.push_str(&text[at..start]);
            let new_start = out.len();
            out.push_str("[redacted:");
            out.push_str(name);
            out.push(']');
            names.push(name.to_string());
            edits.push((start, end, new_start, out.len()));
            at = end;
        }
        out.push_str(&text[at..]);
        check()?;
        Ok((Redacted { text: out, names }, edits))
    }

    /// The first match of this policy in `text`, as `(detector_name, byte_offset)`.
    pub fn first_match(&self, text: &str) -> Option<(String, usize)> {
        self.first_match_checked(text, || Ok::<_, Infallible>(()))
            .unwrap()
    }

    /// Find the first match without collecting every candidate.
    pub fn first_match_checked<E>(
        &self,
        text: &str,
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Option<(String, usize)>, E> {
        let mut first = None;
        self.visit(text, &mut check, |(start, _, priority, name)| {
            if first.is_none_or(|(s, p, _)| (start, priority) < (s, p)) {
                first = Some((start, priority, name));
            }
        })?;
        Ok(first.map(|(start, _, name)| (name.to_string(), start)))
    }

    fn visit<'a, E>(
        &'a self,
        text: &str,
        check: &mut impl FnMut() -> Result<(), E>,
        mut visit: impl FnMut((usize, usize, usize, &'a str)),
    ) -> Result<(), E> {
        static MARKER: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"\[redacted:([A-Za-z0-9_-]{1,64})\]").expect("marker regex compiles")
        });
        let mut allowed = BTreeMap::<usize, usize>::new();
        for re in &self.allow {
            check()?;
            for m in re.find_iter(text) {
                check()?;
                if !m.is_empty() {
                    let end = allowed.entry(m.start()).or_default();
                    *end = (*end).max(m.end());
                }
            }
        }
        let mut farthest = 0;
        // Retain the prefix maximum of the ends so one binary search answers containment.
        let allowed: Vec<_> = allowed
            .into_iter()
            .filter(|&(_, end)| {
                if end <= farthest {
                    return false;
                }
                farthest = end;
                true
            })
            .collect();

        let mut markers = Vec::new();
        for c in MARKER.captures_iter(text) {
            check()?;
            let name = &c[1];
            if matches!(name, "known-value" | "server-secret" | "high-entropy")
                || name == self.known_label
                || PersonalCategory::parse(name).is_some()
                || self.patterns.iter().any(|p| p.name == name)
            {
                let m = c.get(0).expect("group 0");
                markers.push((m.start(), m.end()));
            }
        }
        let mut candidate = |s, e, priority, name| {
            let covered = |ranges: &[(usize, usize)]| {
                let i = ranges.partition_point(|&(start, _)| start <= s);
                i > 0 && ranges[i - 1].1 >= e
            };
            if s < e && !covered(&allowed) && (priority == 0 || !covered(&markers)) {
                visit((s, e, priority, name));
            }
        };
        if let Some(ac) = &self.known_matcher {
            check()?;
            for m in ac.find_overlapping_iter(text) {
                check()?;
                candidate(m.start(), m.end(), 0, &self.known_label);
            }
        }
        for (pi, p) in self.patterns.iter().enumerate() {
            check()?;
            for c in p.regex.captures_iter(text) {
                check()?;
                let m = c.name("v").or_else(|| c.get(0)).expect("group 0");
                candidate(m.start(), m.end(), 1 + pi, &p.name);
            }
        }
        let personal_base = 1 + self.patterns.len();
        for (ci, cat) in self.personal.iter().enumerate() {
            check()?;
            for (s, e) in find_personal(*cat, text) {
                check()?;
                candidate(s, e, personal_base + ci, cat.as_str());
            }
        }
        if let Some(cfg) = self.entropy {
            check()?;
            let prio = personal_base + self.personal.len();
            for (s, e) in find_high_entropy(text, cfg) {
                check()?;
                candidate(s, e, prio, "high-entropy");
            }
        }
        check()
    }
}

/// Keep only the disjoint replacement spans while visiting candidates. This also
/// bounds memory when many detectors match the same text.
fn merge_match<'a>(
    matches: &mut BTreeMap<usize, (usize, usize, &'a str)>,
    (mut start, mut end, mut priority, mut name): (usize, usize, usize, &'a str),
) {
    if let Some((&s, &(e, p, n))) = matches.range(..=start).next_back()
        && e > start
    {
        if (s, p) <= (start, priority) {
            if e >= end {
                return;
            }
            (priority, name) = (p, n);
        }
        start = s;
        end = end.max(e);
        matches.remove(&s);
    }
    while let Some((&s, &(e, _, _))) = matches.range(start..end).next() {
        end = end.max(e);
        matches.remove(&s);
    }
    matches.insert(start, (end, priority, name));
}

/// Byte offsets of one replacement: original start and end, then replacement start and end.
pub type Edit = (usize, usize, usize, usize);

/// What redaction did to one text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    /// the name of the detector of each replacement, in order
    pub names: Vec<String>,
}

impl Redacted {
    /// Counts of replacements grouped by detector name.
    pub fn counts(&self) -> BTreeMap<String, usize> {
        let mut out = BTreeMap::new();
        for name in &self.names {
            *out.entry(name.clone()).or_insert(0) += 1;
        }
        out
    }
}

/// Replace every match of `patterns` in `text`. Matches are found in the original text
/// and overlapping matches are combined; the earliest match names the replacement.
pub fn redact(text: &str, patterns: &[Pattern]) -> Redacted {
    Policy::from_patterns(patterns.to_vec()).redact(text)
}

/// The first place where `patterns` match `text`, as the pattern's name and the byte
/// offset of the match, with the rules of [`redact`]: the earliest match wins, and at
/// one place the earlier pattern. A redaction marker never matches.
pub fn first_match(text: &str, patterns: &[Pattern]) -> Option<(String, usize)> {
    Policy::from_patterns(patterns.to_vec()).first_match(text)
}

/// Patterns from a file: one `name<TAB or space>regex` per line; `#` starts a comment
/// line.
pub fn parse_pattern_file(text: &str) -> Result<Vec<Pattern>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let Some((name, expr)) = t.split_once(char::is_whitespace) else {
            return Err(format!(
                "line {}: expected a name and a regular expression",
                n + 1
            ));
        };
        out.push(
            Pattern::new(name.trim(), expr.trim()).map_err(|e| format!("line {}: {e}", n + 1))?,
        );
    }
    Ok(out)
}

/// Known secret values from a file: one value per line, trimming whitespace, ignoring
/// empty lines and `#` comment lines, and skipping values shorter than
/// [`MIN_KNOWN_VALUE_CHARS`] characters.
pub fn parse_known_values_file(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter(|l| l.chars().count() >= MIN_KNOWN_VALUE_CHARS)
        .map(str::to_string)
        .collect()
}

/// Values of the current process's environment variables whose names contain `KEY`,
/// `SECRET`, `TOKEN` or `PASSWORD` (case-insensitive), skipping values shorter than
/// [`MIN_KNOWN_VALUE_CHARS`] characters.
pub fn env_known_values() -> Vec<String> {
    env_known_values_from(std::env::vars())
}

/// Helper for testing [`env_known_values`] without mutating process environment.
pub fn env_known_values_from<I, K, V>(vars: I) -> Vec<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    vars.into_iter()
        .filter(|(k, _)| {
            let up = k.as_ref().to_ascii_uppercase();
            up.contains("KEY")
                || up.contains("SECRET")
                || up.contains("TOKEN")
                || up.contains("PASSWORD")
        })
        .map(|(_, v)| v.as_ref().trim().to_string())
        .filter(|v| v.chars().count() >= MIN_KNOWN_VALUE_CHARS)
        .collect()
}

/// Remap a sorted list of code-point offsets in `orig` after byte edits from
/// [`Policy::redact_with_edits`] produced `redacted`.
pub fn remap_char_offsets(
    orig: &str,
    redacted: &str,
    edits: &[Edit],
    offsets: &[usize],
) -> Vec<usize> {
    if edits.is_empty() || offsets.is_empty() {
        return offsets.to_vec();
    }
    // Convert orig char offsets -> orig byte offsets -> redacted byte offsets -> redacted char offsets.
    let mut orig_bytes: Vec<usize> = Vec::with_capacity(offsets.len());
    let mut chars = orig.char_indices();
    let mut at = 0usize;
    for &c_off in offsets {
        while at < c_off && chars.next().is_some() {
            at += 1;
        }
        orig_bytes.push(chars.clone().next().map_or(orig.len(), |(i, _)| i));
    }
    let mut new_bytes: Vec<usize> = Vec::with_capacity(orig_bytes.len());
    let mut ei = 0usize;
    let mut delta: isize = 0;
    for b_off in orig_bytes {
        while ei < edits.len() && edits[ei].1 <= b_off {
            let (os, oe, ns, ne) = edits[ei];
            delta += (ne - ns) as isize - (oe - os) as isize;
            ei += 1;
        }
        let mapped = if ei < edits.len() && edits[ei].0 <= b_off {
            // Offset fell inside a redacted span: snap to the start of the marker.
            edits[ei].2
        } else {
            (b_off as isize + delta).max(0) as usize
        };
        new_bytes.push(mapped.min(redacted.len()));
    }
    // Convert redacted byte offsets -> redacted char offsets in one pass.
    let mut out = Vec::with_capacity(new_bytes.len());
    let mut rchars = redacted.char_indices();
    let mut rc = 0usize;
    let mut cur_byte = 0usize;
    for target in new_bytes {
        while cur_byte < target {
            if let Some((i, ch)) = rchars.next() {
                cur_byte = i + ch.len_utf8();
                rc += 1;
            } else {
                break;
            }
        }
        out.push(rc);
    }
    out
}

// --- high-entropy detector --------------------------------------------------------------

/// Contexts in which high-entropy tokens are inspected: assignments, headers, URL query
/// parameters and quoted strings.
static ENTROPY_CONTEXTS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // Assignment: `foo = value`, `foo: "value"`
        r#"(?m)[A-Za-z0-9_.-]+\s*[:=]\s*['"]?(?P<v>[A-Za-z0-9+/=_-]+)"#,
        // Header with optional scheme (`X-Auth: Token <v>` or `X-Api-Key: <v>`)
        r#"(?m)^[A-Za-z0-9-]+\s*:\s*(?:[A-Za-z0-9._-]+\s+)?(?P<v>[A-Za-z0-9+/=_-]+)"#,
        // URL query parameter: `?sig=...` or `&token=...`
        r#"[?&][A-Za-z0-9_.-]+=(?P<v>[A-Za-z0-9+/=_-]+)"#,
        // Quoted string: `"..."` or `'...'`
        r#"["'](?P<v>[A-Za-z0-9+/=_-]+)["']"#,
    ]
    .iter()
    .map(|e| Regex::new(e).expect("entropy context regex compiles"))
    .collect()
});

fn is_canonical_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, &c)| matches!(i, 8 | 13 | 18 | 23) || c.is_ascii_hexdigit())
}

fn is_token_alphabet(s: &str) -> bool {
    // Allow hex, standard base64 (`+`, `/`, trailing `=`), or base64url (`-`, `_`, trailing `=`).
    let core = s.trim_end_matches('=');
    let pad = s.len() - core.len();
    if pad > 2 || core.is_empty() {
        return false;
    }
    if core.contains('=') {
        return false;
    }
    let has_std = core.bytes().any(|b| b == b'+' || b == b'/');
    let has_url = core.bytes().any(|b| b == b'-' || b == b'_');
    // Do not accept mixed standard base64 and base64url punctuation in one token.
    if has_std && has_url {
        return false;
    }
    core.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_'))
}

/// Shannon entropy per byte/character (in bits) of an ASCII token.
pub fn shannon_entropy(s: &str) -> f64 {
    let len = s.len();
    if len == 0 {
        return 0.0;
    }
    let mut counts = [0u32; 256];
    for &b in s.as_bytes() {
        counts[usize::from(b)] += 1;
    }
    let total = len as f64;
    let mut h = 0.0;
    for &c in &counts {
        if c > 0 {
            let p = f64::from(c) / total;
            h -= p * p.log2();
        }
    }
    h
}

fn find_high_entropy(text: &str, cfg: EntropyConfig) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for re in ENTROPY_CONTEXTS.iter() {
        for cap in re.captures_iter(text) {
            let Some(m) = cap.name("v") else {
                continue;
            };
            let s = m.as_str();
            if s.len() < cfg.min_length || !is_token_alphabet(s) {
                continue;
            }
            // Exclude 40- and 64-char hex commit/content hashes and canonical UUIDs.
            if (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            if is_canonical_uuid(s) {
                continue;
            }
            if shannon_entropy(s) >= cfg.min_bits_per_char {
                out.push((m.start(), m.end()));
            }
        }
    }
    out
}

// --- personal data detectors ------------------------------------------------------------

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}\b")
        .expect("email regex compiles")
});

static PHONE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\+[1-9](?:[\s().-]*\d){6,14}").expect("phone regex compiles"));

static CARD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:\d[ -]?){12,18}\d").expect("payment card regex compiles"));

static IBAN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b[A-Z]{2}\d{2}(?:[ \t-]?[A-Z0-9]){11,30}\b").expect("iban regex compiles")
});

static IPV4_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(?:\.(?:25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}\b",
    )
    .expect("ipv4 regex compiles")
});

static IPV6_CANDIDATE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[0-9A-Fa-f:]{2,39}(?:\.[0-9]{1,3}){0,3}").expect("ipv6 regex compiles")
});

fn find_personal(cat: PersonalCategory, text: &str) -> Vec<(usize, usize)> {
    match cat {
        PersonalCategory::Email => find_emails(text),
        PersonalCategory::Phone => find_phones(text),
        PersonalCategory::PaymentCard => find_cards(text),
        PersonalCategory::Iban => find_ibans(text),
        PersonalCategory::Ip => find_ips(text),
    }
}

fn find_emails(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut userinfo = false;
    for m in EMAIL_RE.find_iter(text) {
        // Skip URL userinfo like `https://user@host.example` or `postgres://user:pass@host.example`.
        let before = &text[at..m.start()];
        if let Some(i) = before.rfind(|c: char| c == '/' || c.is_whitespace()) {
            userinfo = text[..at + i + 1].ends_with("://");
        }
        if !userinfo {
            out.push((m.start(), m.end()));
        }
        at = m.end();
    }
    out
}

fn find_phones(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for m in PHONE_RE.find_iter(text) {
        let s = m.start();
        let e = m.end();
        if s > 0 {
            let prev = bytes[s - 1];
            if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'+' {
                continue;
            }
        }
        if e < bytes.len() {
            let next = bytes[e];
            if next.is_ascii_alphanumeric() || next == b'_' {
                continue;
            }
        }
        let digits = m.as_str().bytes().filter(u8::is_ascii_digit).count();
        if (7..=15).contains(&digits) {
            out.push((s, e));
        }
    }
    out
}

fn luhn_valid(digits: &[u8]) -> bool {
    let mut sum = 0u32;
    let mut double = false;
    for &d in digits.iter().rev() {
        let mut v = u32::from(d - b'0');
        if double {
            v *= 2;
            if v > 9 {
                v -= 9;
            }
        }
        sum += v;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn find_cards(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for m in CARD_RE.find_iter(text) {
        let s = m.start();
        let e = m.end();
        if s > 0 {
            let prev = bytes[s - 1];
            if prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'.' | b'/' | b'-' | b'+') {
                continue;
            }
        }
        if e < bytes.len() {
            let next = bytes[e];
            if next.is_ascii_alphanumeric() || matches!(next, b'_' | b'.' | b'/' | b'-') {
                continue;
            }
        }
        let digits: Vec<u8> = m.as_str().bytes().filter(u8::is_ascii_digit).collect();
        if (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
            out.push((s, e));
        }
    }
    out
}

fn iban_valid(raw: &str) -> bool {
    let compact: Vec<u8> = raw
        .bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'-'))
        .collect();
    if !(15..=34).contains(&compact.len()) {
        return false;
    }
    if !compact[0].is_ascii_uppercase()
        || !compact[1].is_ascii_uppercase()
        || !compact[2].is_ascii_digit()
        || !compact[3].is_ascii_digit()
    {
        return false;
    }
    if !compact[4..]
        .iter()
        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
    {
        return false;
    }
    // ISO 13616: move first 4 chars to the end, expand A..=Z -> 10..=35, mod 97 == 1.
    let mut rem: u32 = 0;
    for &b in compact[4..].iter().chain(&compact[..4]) {
        if b.is_ascii_digit() {
            rem = (rem * 10 + u32::from(b - b'0')) % 97;
        } else {
            let v = u32::from(b - b'A') + 10;
            rem = (rem * 100 + v) % 97;
        }
    }
    rem == 1
}

fn find_ibans(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for m in IBAN_RE.find_iter(text) {
        if iban_valid(m.as_str()) {
            out.push((m.start(), m.end()));
        }
    }
    out
}

fn find_ips(text: &str) -> Vec<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    for m in IPV4_RE.find_iter(text) {
        let s = m.start();
        let e = m.end();
        if s > 0 && (bytes[s - 1] == b'.' || bytes[s - 1].is_ascii_alphanumeric()) {
            continue;
        }
        if e < bytes.len()
            && (continues_dotted_address(bytes, e) || bytes[e].is_ascii_alphanumeric())
        {
            continue;
        }
        out.push((s, e));
    }
    for m in IPV6_CANDIDATE_RE.find_iter(text) {
        let s = m.start();
        let e = m.end();
        let cand = m.as_str();
        // Need at least two colons so times like 12:30 or hex pairs are not considered.
        if cand == "::" || cand.bytes().filter(|&b| b == b':').count() < 2 {
            continue;
        }
        // Keep matches bounded by punctuation or whitespace.
        if s > 0 {
            let prev = bytes[s - 1];
            if prev.is_ascii_alphanumeric() || prev == b':' || prev == b'_' {
                continue;
            }
        }
        if e < bytes.len() {
            let next = bytes[e];
            if next.is_ascii_alphanumeric()
                || next == b':'
                || next == b'_'
                || continues_dotted_address(bytes, e)
            {
                continue;
            }
        }
        if cand.parse::<Ipv6Addr>().is_ok() {
            out.push((s, e));
        }
    }
    out
}

fn continues_dotted_address(bytes: &[u8], end: usize) -> bool {
    bytes.get(end) == Some(&b'.') && bytes.get(end + 1).is_some_and(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_patterns() {
        let p = builtin();
        let gh = format!("token ghp_{}", "a".repeat(36));
        let r = redact(&gh, &p);
        assert_eq!(r.text, "token [redacted:github-token]");
        assert_eq!(r.names, vec!["github-token"]);
        let r = redact("key: sk-ant-api03-abcdefghijklmnopqrstuvwxyz", &p);
        assert!(r.text.contains("[redacted:anthropic-key]"), "{}", r.text);
        assert_eq!(r.names.len(), 1);
        let r = redact("Authorization: Bearer abcdefghijkl.mnop", &p);
        assert_eq!(
            r.text,
            "Authorization: Bearer [redacted:authorization-header]"
        );
        let r = redact("postgres://app:hunter2@db:5432/x", &p);
        assert_eq!(r.text, "postgres://app:[redacted:url-password]@db:5432/x");
        let r = redact("DB_PASSWORD=correct-horse-battery", &p);
        assert_eq!(r.text, "DB_PASSWORD=[redacted:secret-assignment]");
        let r = redact(
            "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\nafter",
            &p,
        );
        assert_eq!(r.text, "[redacted:private-key]\nafter");
        let r = redact("AKIAABCDEFGHIJKLMNOP and AIza", &p);
        assert_eq!(r.names, vec!["aws-access-key-id"]);
        // short or ordinary values stay
        let r = redact("port 5433, password: short, the token budget", &p);
        assert!(r.names.is_empty(), "{:?}", r.names);
    }

    #[test]
    fn extra_patterns() {
        let mut p = builtin();
        p.extend(
            parse_pattern_file("# a comment\nacme-deploy-key acme_dk_[A-Za-z0-9]{8}\n").unwrap(),
        );
        let r = redact("use acme_dk_ABCDEFGH now", &p);
        assert_eq!(r.text, "use [redacted:acme-deploy-key] now");
        assert!(parse_pattern_file("bad! x").is_err());
        // the server's check finds the first match and never a marker
        let gh = format!("ok\nuse ghp_{} now", "b".repeat(36));
        assert_eq!(first_match(&gh, &p), Some(("github-token".into(), 7)));
        let r = redact(&gh, &p);
        assert_eq!(first_match(&r.text, &p), None);
        assert_eq!(first_match("nothing here", &p), None);
        assert!(parse_pattern_file("x (").is_err());
    }

    #[test]
    fn high_entropy_detector_contexts_and_exclusions() {
        let mut policy = Policy::from_patterns(builtin());
        policy.entropy = Some(EntropyConfig::default());

        // High-entropy value in an assignment, header, URL query param and quoted string.
        let sample = concat!(
            "custom_cred = 9fK2mP7qX4vL8nR1wZ6cB3tY0hJ5\n",
            "X-Service-Sig: aB3dE6gH9jK2mN5pQ8rS1tU4vW7yZ0\n",
            "https://example.org/cb?sig=Qz7xL2pV9mK4nR8wT1bY6cF3hJ0dS5\n",
            "value \"kL9pQ2rT5vW8yB1nM4cX7zF0hJ3dG6\"\n",
        );
        let r = policy.redact(sample);
        assert_eq!(
            r.names,
            vec![
                "high-entropy",
                "high-entropy",
                "high-entropy",
                "high-entropy"
            ],
            "text: {}",
            r.text
        );
        assert!(!r.text.contains("9fK2mP7qX4vL8nR1wZ6cB3tY0hJ5"));
        assert!(!r.text.contains("aB3dE6gH9jK2mN5pQ8rS1tU4vW7yZ0"));
        assert!(!r.text.contains("Qz7xL2pV9mK4nR8wT1bY6cF3hJ0dS5"));
        assert!(!r.text.contains("kL9pQ2rT5vW8yB1nM4cX7zF0hJ3dG6"));

        // 40-char SHA-1, 64-char SHA-256, canonical UUIDs, low-entropy and bare prose stay.
        let safe = concat!(
            "commit = a1b2c3d4e5f60718293a4b5c6d7e8f9012345678\n",
            "digest = 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
            "id = \"550e8400-e29b-41d4-a716-446655440000\"\n",
            "repeat = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n",
            "plain text 9fK2mP7qX4vL8nR1wZ6cB3tY0hJ5 outside any context\n",
        );
        let r = policy.redact(safe);
        assert!(r.names.is_empty(), "{:?} -> {}", r.names, r.text);
        assert_eq!(r.text, safe);
    }

    #[test]
    fn personal_data_detectors_positive_and_negative() {
        let mut policy = Policy::from_patterns(builtin());
        policy.personal = PersonalCategory::ALL.to_vec();

        // Positive examples for every category.
        let text = concat!(
            "Email: alice.smith+ops@example.co.uk\n",
            "Phone: +1 (415) 555-2671\n",
            "Card: 4532-0151-1283-0366\n",
            "IBAN: GB82 WEST 1234 5698 7654 32\n",
            "IPv4: 192.168.1.42 and IPv6: 2001:db8::1\n",
        );
        let r = policy.redact(text);
        assert_eq!(
            r.text,
            concat!(
                "Email: [redacted:email]\n",
                "Phone: [redacted:phone]\n",
                "Card: [redacted:payment-card]\n",
                "IBAN: [redacted:iban]\n",
                "IPv4: [redacted:ip] and IPv6: [redacted:ip]\n",
            )
        );
        let counts = r.counts();
        assert_eq!(counts.get("email"), Some(&1));
        assert_eq!(counts.get("phone"), Some(&1));
        assert_eq!(counts.get("payment-card"), Some(&1));
        assert_eq!(counts.get("iban"), Some(&1));
        assert_eq!(counts.get("ip"), Some(&2));

        // Negative examples: bad Luhn, bad IBAN check digits, phone without `+`, invalid IPv4, time.
        let safe = concat!(
            "Bad card: 4532-0151-1283-0367\n",
            "Bad IBAN: GB00 WEST 1234 5698 7654 32\n",
            "Local phone: 415-555-2671\n",
            "Version: 1.2.3.4.5 and 999.1.2.3\n",
            "Time: 12:30 and short +12345, separator a :: b\n",
        );
        let r = policy.redact(safe);
        assert!(r.names.is_empty(), "{:?} -> {}", r.names, r.text);
        assert_eq!(r.text, safe);
    }

    #[test]
    fn embedded_ipv4_addresses_redact_whole_with_surrounding_punctuation() {
        let mut p = Policy::from_patterns(Vec::new());
        p.personal = vec![PersonalCategory::Ip];
        let r = p
            .redact("From (::ffff:192.0.2.1), https://[::ffff:192.0.2.2]:80 and ::ffff:192.0.2.3.");
        assert_eq!(
            r.text,
            "From ([redacted:ip]), https://[[redacted:ip]]:80 and [redacted:ip]."
        );
        assert_eq!(r.names, ["ip", "ip", "ip"]);
    }

    #[test]
    fn known_values_and_allow_list() {
        let mut policy = Policy::from_patterns(builtin())
            .with_known_values(["my-internal-secret-value-99", "short7"], "server-secret");
        // Debug never exposes the known secret value.
        let dbg = format!("{policy:?}");
        assert!(!dbg.contains("my-internal-secret-value-99"), "{dbg}");

        let text = "Leaked my-internal-secret-value-99 and short7 here.";
        let r = policy.redact(text);
        assert_eq!(r.text, "Leaked [redacted:server-secret] and short7 here.");
        assert_eq!(r.names, vec!["server-secret"]);

        // Allow list preserves an explicit public test key while redacting others.
        let test_key = format!("ghp_{}", "T".repeat(36));
        let real_key = format!("ghp_{}", "R".repeat(36));
        policy
            .allow
            .push(Regex::new(&regex::escape(&test_key)).unwrap());
        let both = format!("test={test_key} real={real_key}");
        let r = policy.redact(&both);
        assert_eq!(
            r.text,
            format!("test={test_key} real=[redacted:github-token]")
        );
        assert_eq!(r.names, vec!["github-token"]);
    }

    #[test]
    fn overlapping_matches_redact_the_union_after_allow_filtering() {
        let p = Policy::default().with_known_values(["secret12"], "known");
        let r = p.redact("API_KEY=secret12_live_9876543210");
        assert_eq!(r.text, "API_KEY=[redacted:known]");
        assert_eq!(p.first_match(&r.text), None);
        assert!(
            p.first_match("API_KEY=[redacted:known]_live_9876543210")
                .is_some()
        );
        let fake = format!("[redacted:ghp_{}]", "T".repeat(36));
        assert_eq!(p.first_match(&fake), Some(("github-token".into(), 10)));

        let regex = Pattern::new("prefix", "abcde").unwrap();
        let mut p =
            Policy::from_patterns(vec![regex]).with_known_values(["cdefghij", "ghijklmn"], "known");
        assert_eq!(p.redact("abcdefghijklmn!").text, "[redacted:prefix]!");
        p.patterns.clear();
        assert_eq!(p.redact("abcdefghijklmn!").text, "ab[redacted:known]!");
        p.allow.push(Regex::new("cdefghij").unwrap());
        assert_eq!(p.redact("abcdefghijklmn!").text, "abcdef[redacted:known]!");
    }

    #[test]
    fn dense_allow_list_and_cancellation() {
        let mut p = Policy::from_patterns(vec![Pattern::new("token", "token").unwrap()]);
        p.allow.push(Regex::new("allowed-token").unwrap());
        let text = "allowed-token ".repeat(100_000) + "token";
        assert_eq!(p.first_match(&text), Some(("token".into(), 1_400_000)));
        assert_eq!(p.redact(&text).names, ["token"]);
        let mut calls = 0;
        let check = || {
            calls += 1;
            if calls > 100 {
                Err("cancelled")
            } else {
                Ok(())
            }
        };
        assert_eq!(p.redact_checked(&text, check), Err("cancelled"));
    }

    #[test]
    fn env_and_file_known_values() {
        let vals = env_known_values_from([
            ("HOME", "/home/alice"),
            ("MY_API_KEY", "super-secret-api-key"),
            ("APP_SECRET", "short"),
            ("db_password", "correct-horse-battery"),
            ("SESSION_TOKEN", "tok-1234567890"),
        ]);
        assert_eq!(
            vals,
            vec![
                "super-secret-api-key",
                "correct-horse-battery",
                "tok-1234567890"
            ]
        );
        let file = parse_known_values_file("# comment\n\nshort7\nlong-secret-value\n");
        assert_eq!(file, vec!["long-secret-value"]);
    }

    #[test]
    fn remap_char_offsets_adjusts_page_starts() {
        let orig =
            "<!-- page 1 -->\nkey sk-ant-api03-abcdefghijklmnopqrstuvwxyz\n<!-- page 2 -->\nend";
        let p2_orig = orig[..orig.find("<!-- page 2 -->").unwrap()]
            .chars()
            .count();
        let policy = Policy::default();
        let (r, edits) = policy.redact_with_edits(orig);
        let mapped = remap_char_offsets(orig, &r.text, &edits, &[0, p2_orig]);
        let p2_expected = r.text[..r.text.find("<!-- page 2 -->").unwrap()]
            .chars()
            .count();
        assert_eq!(mapped, vec![0, p2_expected]);
    }
}
