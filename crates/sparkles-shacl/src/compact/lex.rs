//! The SHACLC lexer: Turtle's terms (IRIs, prefixed names, strings, numbers, language
//! tags) plus the punctuation and the bare words of the compact syntax.

use super::SyntaxError;

/// A token. Bare words are keywords, parameter names, node kinds and booleans; which
/// one is decided by the parser from the position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tok {
    /// `<…>`, its content with `\u` escapes decoded (not yet resolved)
    IriRef(String),
    /// `prefix:local` (`local` is empty for `prefix:`), the local name unescaped
    PName(String, String),
    /// `@prefix:local`
    AtPName(String, String),
    /// `@` before an IRI reference
    At,
    /// `@en-US` (without the `@`)
    LangTag(String),
    String(String),
    Integer(String),
    Decimal(String),
    Double(String),
    /// a bare word
    Word(String),
    /// punctuation: `{ } [ ] ( ) | / ^ ^^ * + ? ! = . .. ->`
    P(&'static str),
}

impl Tok {
    pub(crate) fn describe(&self) -> String {
        match self {
            Tok::IriRef(i) => format!("<{i}>"),
            Tok::PName(p, l) => format!("{p}:{l}"),
            Tok::AtPName(p, l) => format!("@{p}:{l}"),
            Tok::At => "'@'".into(),
            Tok::LangTag(l) => format!("@{l}"),
            Tok::String(_) => "a string".into(),
            Tok::Integer(n) | Tok::Decimal(n) | Tok::Double(n) => n.clone(),
            Tok::Word(w) => format!("'{w}'"),
            Tok::P(p) => format!("'{p}'"),
        }
    }
}

/// A token and the line and column (1-based, in characters) where it starts.
pub(crate) type Spanned = (Tok, usize, usize);

pub(crate) fn tokenize(text: &str) -> Result<Vec<Spanned>, SyntaxError> {
    let mut l = Lexer {
        chars: text.trim_start_matches('\u{feff}').chars().collect(),
        i: 0,
        line: 1,
        col: 1,
    };
    let mut out = Vec::new();
    loop {
        l.skip_space();
        if l.i >= l.chars.len() {
            return Ok(out);
        }
        let (line, col) = (l.line, l.col);
        let t = l.token()?;
        out.push((t, line, col));
    }
}

struct Lexer {
    chars: Vec<char>,
    i: usize,
    line: usize,
    col: usize,
}

fn is_pn_chars_base(c: char) -> bool {
    matches!(c,
        'A'..='Z' | 'a'..='z'
        | '\u{00C0}'..='\u{00D6}' | '\u{00D8}'..='\u{00F6}' | '\u{00F8}'..='\u{02FF}'
        | '\u{0370}'..='\u{037D}' | '\u{037F}'..='\u{1FFF}' | '\u{200C}'..='\u{200D}'
        | '\u{2070}'..='\u{218F}' | '\u{2C00}'..='\u{2FEF}' | '\u{3001}'..='\u{D7FF}'
        | '\u{F900}'..='\u{FDCF}' | '\u{FDF0}'..='\u{FFFD}' | '\u{10000}'..='\u{EFFFF}')
}

fn is_pn_chars_u(c: char) -> bool {
    is_pn_chars_base(c) || c == '_'
}

fn is_pn_chars(c: char) -> bool {
    is_pn_chars_u(c)
        || c == '-'
        || c.is_ascii_digit()
        || c == '\u{00B7}'
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

const LOCAL_ESCAPES: &str = "_~.-!$&'()*+,;=/?#@%";

impl Lexer {
    fn peek(&self, k: usize) -> Option<char> {
        self.chars.get(self.i + k).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.i).copied()?;
        self.i += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn err(&self, msg: impl Into<String>) -> SyntaxError {
        SyntaxError {
            line: self.line,
            column: self.col,
            message: msg.into(),
        }
    }

    fn skip_space(&mut self) {
        while let Some(c) = self.peek(0) {
            if c.is_whitespace() {
                self.bump();
            } else if c == '#' {
                while let Some(c) = self.peek(0) {
                    if c == '\n' || c == '\r' {
                        break;
                    }
                    self.bump();
                }
            } else {
                break;
            }
        }
    }

    fn token(&mut self) -> Result<Tok, SyntaxError> {
        let c = self.peek(0).expect("not at the end");
        match c {
            '<' => self.iri_ref().map(Tok::IriRef),
            '"' | '\'' => self.string().map(Tok::String),
            '@' => self.at(),
            '0'..='9' => self.number(),
            '+' | '-' if self.number_follows(1) => self.number(),
            '.' if self.peek(1).is_some_and(|d| d.is_ascii_digit()) => self.number(),
            '.' => {
                self.bump();
                if self.peek(0) == Some('.') {
                    self.bump();
                    Ok(Tok::P(".."))
                } else {
                    Ok(Tok::P("."))
                }
            }
            '-' if self.peek(1) == Some('>') => {
                self.bump();
                self.bump();
                Ok(Tok::P("->"))
            }
            '^' => {
                self.bump();
                if self.peek(0) == Some('^') {
                    self.bump();
                    Ok(Tok::P("^^"))
                } else {
                    Ok(Tok::P("^"))
                }
            }
            '{' | '}' | '[' | ']' | '(' | ')' | '|' | '/' | '*' | '+' | '?' | '!' | '=' => {
                self.bump();
                Ok(Tok::P(match c {
                    '{' => "{",
                    '}' => "}",
                    '[' => "[",
                    ']' => "]",
                    '(' => "(",
                    ')' => ")",
                    '|' => "|",
                    '/' => "/",
                    '*' => "*",
                    '+' => "+",
                    '?' => "?",
                    '!' => "!",
                    _ => "=",
                }))
            }
            ':' => {
                self.bump();
                let local = self.local()?;
                Ok(Tok::PName(String::new(), local))
            }
            c if is_pn_chars_base(c) => {
                let word = self.prefix_chars();
                if self.peek(0) == Some(':') {
                    self.bump();
                    let local = self.local()?;
                    Ok(Tok::PName(word, local))
                } else {
                    Ok(Tok::Word(word))
                }
            }
            c => Err(self.err(format!("unexpected character {c:?}"))),
        }
    }

    /// Whether a number starts `k` characters ahead (after a sign).
    fn number_follows(&self, k: usize) -> bool {
        match self.peek(k) {
            Some(d) if d.is_ascii_digit() => true,
            Some('.') => self.peek(k + 1).is_some_and(|d| d.is_ascii_digit()),
            _ => false,
        }
    }

    /// `PN_PREFIX`: a word that may contain dots, but not end with one.
    fn prefix_chars(&mut self) -> String {
        let start = self.i;
        let mut end = self.i;
        let mut k = self.i;
        while let Some(&c) = self.chars.get(k) {
            if is_pn_chars(c) {
                k += 1;
                end = k;
            } else if c == '.' {
                k += 1;
            } else {
                break;
            }
        }
        while self.i < end {
            self.bump();
        }
        self.chars[start..end].iter().collect()
    }

    /// `PN_LOCAL` after the colon, with backslash escapes removed (`%HH` is kept).
    fn local(&mut self) -> Result<String, SyntaxError> {
        let mut out = String::new();
        // characters consumed but not yet known to be inside the name (trailing dots)
        let mut dots = 0usize;
        let mut first = true;
        while let Some(c) = self.peek(0) {
            let ok_first = is_pn_chars_u(c) || c == ':' || c.is_ascii_digit();
            let ok_rest = is_pn_chars(c) || c == ':';
            if (first && ok_first) || (!first && ok_rest) {
                // a dot before this character is part of the name
                out.extend(std::iter::repeat_n('.', dots));
                dots = 0;
                out.push(c);
                self.bump();
            } else if c == '%' {
                let (a, b) = (self.peek(1), self.peek(2));
                if !(a.is_some_and(|a| a.is_ascii_hexdigit())
                    && b.is_some_and(|b| b.is_ascii_hexdigit()))
                {
                    return Err(self.err("invalid % escape in a prefixed name"));
                }
                out.extend(std::iter::repeat_n('.', dots));
                dots = 0;
                for _ in 0..3 {
                    out.push(self.bump().expect("checked"));
                }
            } else if c == '\\' {
                let e = self.peek(1);
                match e {
                    Some(e) if LOCAL_ESCAPES.contains(e) => {
                        out.extend(std::iter::repeat_n('.', dots));
                        dots = 0;
                        self.bump();
                        self.bump();
                        out.push(e);
                    }
                    _ => return Err(self.err("invalid escape in a prefixed name")),
                }
            } else if c == '.' && !first {
                // count it; it belongs to the name only if a name character follows
                let mut k = 0;
                while self.peek(k) == Some('.') {
                    k += 1;
                }
                let next = self.peek(k);
                let continues =
                    next.is_some_and(|n| is_pn_chars(n) || n == ':' || n == '%' || n == '\\');
                if !continues {
                    break;
                }
                for _ in 0..k {
                    self.bump();
                }
                dots += k;
            } else {
                break;
            }
            first = false;
        }
        Ok(out)
    }

    fn at(&mut self) -> Result<Tok, SyntaxError> {
        self.bump();
        match self.peek(0) {
            Some('<') => Ok(Tok::At),
            Some(':') => {
                self.bump();
                let local = self.local()?;
                Ok(Tok::AtPName(String::new(), local))
            }
            Some(c) if is_pn_chars_base(c) => {
                // a prefixed name if a colon follows its prefix, else a language tag
                let save = (self.i, self.line, self.col);
                let word = self.prefix_chars();
                if self.peek(0) == Some(':') {
                    self.bump();
                    let local = self.local()?;
                    return Ok(Tok::AtPName(word, local));
                }
                (self.i, self.line, self.col) = save;
                let mut tag = String::new();
                while let Some(c) = self.peek(0) {
                    if c.is_ascii_alphabetic() {
                        tag.push(c);
                        self.bump();
                    } else {
                        break;
                    }
                }
                while self.peek(0) == Some('-')
                    && self.peek(1).is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    tag.push('-');
                    self.bump();
                    while let Some(c) = self.peek(0) {
                        if c.is_ascii_alphanumeric() {
                            tag.push(c);
                            self.bump();
                        } else {
                            break;
                        }
                    }
                }
                if tag.is_empty() {
                    return Err(self.err("expected a language tag or a shape reference after '@'"));
                }
                Ok(Tok::LangTag(tag))
            }
            _ => Err(self.err("expected a language tag or a shape reference after '@'")),
        }
    }

    fn hex(&mut self, n: usize) -> Result<char, SyntaxError> {
        let mut v = 0u32;
        for _ in 0..n {
            let d = self
                .peek(0)
                .and_then(|c| c.to_digit(16))
                .ok_or_else(|| self.err("invalid \\u escape"))?;
            self.bump();
            v = v * 16 + d;
        }
        char::from_u32(v).ok_or_else(|| self.err("invalid code point in a \\u escape"))
    }

    fn iri_ref(&mut self) -> Result<String, SyntaxError> {
        self.bump();
        let mut out = String::new();
        loop {
            let c = self
                .bump()
                .ok_or_else(|| self.err("unterminated IRI reference"))?;
            match c {
                '>' => return Ok(out),
                '\\' => match self.bump() {
                    Some('u') => out.push(self.hex(4)?),
                    Some('U') => out.push(self.hex(8)?),
                    _ => return Err(self.err("invalid escape in an IRI reference")),
                },
                c if c <= ' ' || "<\"{}|^`".contains(c) => {
                    return Err(self.err(format!("invalid character {c:?} in an IRI reference")));
                }
                c => out.push(c),
            }
        }
    }

    fn string(&mut self) -> Result<String, SyntaxError> {
        let (line, column) = (self.line, self.col);
        let at_start = |message: &str| SyntaxError {
            line,
            column,
            message: message.into(),
        };
        let q = self.bump().expect("a quote");
        let long = self.peek(0) == Some(q) && self.peek(1) == Some(q);
        if long {
            self.bump();
            self.bump();
        } else if self.peek(0) == Some(q) {
            // the empty string
            self.bump();
            return Ok(String::new());
        }
        let mut out = String::new();
        loop {
            let c = self.bump().ok_or_else(|| at_start("unterminated string"))?;
            if c == q {
                if !long {
                    return Ok(out);
                }
                if self.peek(0) == Some(q) && self.peek(1) == Some(q) {
                    // the closing quotes; any quotes before them belong to the string
                    while self.peek(2) == Some(q) {
                        out.push(q);
                        self.bump();
                    }
                    self.bump();
                    self.bump();
                    return Ok(out);
                }
                out.push(c);
                continue;
            }
            match c {
                '\\' => {
                    let e = self.bump().ok_or_else(|| at_start("unterminated string"))?;
                    out.push(match e {
                        't' => '\t',
                        'b' => '\u{8}',
                        'n' => '\n',
                        'r' => '\r',
                        'f' => '\u{c}',
                        '"' => '"',
                        '\'' => '\'',
                        '\\' => '\\',
                        'u' => self.hex(4)?,
                        'U' => self.hex(8)?,
                        e => return Err(self.err(format!("invalid escape \\{e} in a string"))),
                    });
                }
                '\n' | '\r' if !long => return Err(at_start("line break in a short string")),
                c => out.push(c),
            }
        }
    }

    fn digits(&mut self, out: &mut String) -> usize {
        let mut n = 0;
        while let Some(c) = self.peek(0) {
            if c.is_ascii_digit() {
                out.push(c);
                self.bump();
                n += 1;
            } else {
                break;
            }
        }
        n
    }

    /// Whether an exponent starts here.
    fn exponent_follows(&self) -> bool {
        matches!(self.peek(0), Some('e' | 'E'))
            && match self.peek(1) {
                Some('+' | '-') => self.peek(2).is_some_and(|c| c.is_ascii_digit()),
                Some(c) => c.is_ascii_digit(),
                None => false,
            }
    }

    fn exponent(&mut self, out: &mut String) {
        out.push(self.bump().expect("e"));
        if let Some(s @ ('+' | '-')) = self.peek(0) {
            out.push(s);
            self.bump();
        }
        self.digits(out);
    }

    fn number(&mut self) -> Result<Tok, SyntaxError> {
        let mut out = String::new();
        if let Some(s @ ('+' | '-')) = self.peek(0) {
            out.push(s);
            self.bump();
        }
        let int = self.digits(&mut out);
        let dot_fraction = self.peek(0) == Some('.')
            && (self.peek(1).is_some_and(|c| c.is_ascii_digit())
                || (int > 0
                    && matches!(self.peek(1), Some('e' | 'E'))
                    && match self.peek(2) {
                        Some('+' | '-') => self.peek(3).is_some_and(|c| c.is_ascii_digit()),
                        Some(c) => c.is_ascii_digit(),
                        None => false,
                    }));
        if dot_fraction {
            out.push('.');
            self.bump();
            let frac = self.digits(&mut out);
            if self.exponent_follows() {
                self.exponent(&mut out);
                return Ok(Tok::Double(out));
            }
            if frac == 0 {
                return Err(self.err("expected digits after the decimal point"));
            }
            return Ok(Tok::Decimal(out));
        }
        if int > 0 && self.exponent_follows() {
            self.exponent(&mut out);
            return Ok(Tok::Double(out));
        }
        if int == 0 {
            return Err(self.err("expected a number"));
        }
        Ok(Tok::Integer(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        tokenize(s).unwrap().into_iter().map(|t| t.0).collect()
    }

    #[test]
    fn terms_and_punctuation() {
        assert_eq!(
            toks("ex:a.b. [0..*] ->"),
            [
                Tok::PName("ex".into(), "a.b".into()),
                Tok::P("."),
                Tok::P("["),
                Tok::Integer("0".into()),
                Tok::P(".."),
                Tok::P("*"),
                Tok::P("]"),
                Tok::P("->"),
            ]
        );
        assert_eq!(
            toks("@ex:S @<http://x/> \"a\"@en-GB 'b'^^xsd:string"),
            [
                Tok::AtPName("ex".into(), "S".into()),
                Tok::At,
                Tok::IriRef("http://x/".into()),
                Tok::String("a".into()),
                Tok::LangTag("en-GB".into()),
                Tok::String("b".into()),
                Tok::P("^^"),
                Tok::PName("xsd".into(), "string".into()),
            ]
        );
        assert_eq!(
            toks("1 -2 +3.5 .5 1e3 1.E-2 4."),
            [
                Tok::Integer("1".into()),
                Tok::Integer("-2".into()),
                Tok::Decimal("+3.5".into()),
                Tok::Decimal(".5".into()),
                Tok::Double("1e3".into()),
                Tok::Double("1.E-2".into()),
                Tok::Integer("4".into()),
                Tok::P("."),
            ]
        );
        assert_eq!(
            toks("\"\"\"a\"b\"\"\"\" '\\u0041\\n' : ex:a\\.b"),
            [
                Tok::String("a\"b\"".into()),
                Tok::String("A\n".into()),
                Tok::PName("".into(), "".into()),
                Tok::PName("ex".into(), "a.b".into()),
            ]
        );
        assert_eq!(
            toks("ex:p+ ^ex:q* # comment\n minCount=1"),
            [
                Tok::PName("ex".into(), "p".into()),
                Tok::P("+"),
                Tok::P("^"),
                Tok::PName("ex".into(), "q".into()),
                Tok::P("*"),
                Tok::Word("minCount".into()),
                Tok::P("="),
                Tok::Integer("1".into()),
            ]
        );
    }

    #[test]
    fn errors_have_positions() {
        let e = tokenize("shape ex:S {\n  ex:p \"open\n").unwrap_err();
        assert_eq!((e.line, e.column), (2, 8), "{e}");
        assert!(tokenize("<a b>").is_err());
        assert!(tokenize("ex:a%zz").is_err());
    }
}
