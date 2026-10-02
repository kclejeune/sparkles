//! A bound on how deeply a query or update nests, checked on its text before it is parsed.
//!
//! The parser is recursive descent, so each bracket costs stack: a few hundred levels of
//! `FILTER NOT EXISTS {` overflow a thread's 2 MiB stack, and a stack overflow aborts the
//! whole process. Chains such as `?a || ?b || …`, `{ … } UNION { … } UNION …`, the
//! OPTIONALs of a group or the steps of a property path are parsed by loops, but each of
//! their operators wraps the ones before it in the algebra, and the code that walks or
//! drops the algebra recurses that deep: tens of thousands of them overflow the stack too.
//!
//! [`check`] scans the tokens once, without recursion, and refuses a text that nests
//! deeper than [`MAX_NESTING`] brackets or whose algebra could nest deeper than
//! [`MAX_DEPTH`] levels. The depth is an upper bound counted from the text, so a query
//! just under the limit may be refused although its algebra is shallower.

/// The deepest nesting of brackets (`(`, `[`, `{`, `<<`, `<<(`, `{|`, the parentheses of
/// function calls and property paths included) and unary `!` the parser accepts. A level
/// costs the parser up to 6 KiB of stack (a `FILTER NOT EXISTS {` group), so 256 levels
/// take about 1.5 MiB.
pub const MAX_NESTING: usize = 256;

/// The deepest algebra the parser builds: the nesting of brackets plus, at each level, one
/// level per operator of an expression chain (`||`, `&&`, `+`, `-`, `*`, `/`), per step or
/// operator of a property path, and per element of a group graph pattern (a nested group,
/// OPTIONAL, MINUS, UNION, FILTER, BIND, VALUES, a triple with a property path).
pub const MAX_DEPTH: usize = 1024;

/// How deeply a text nests, as [`measure`] counts it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Nesting {
    /// the deepest nesting of brackets and `!`
    pub brackets: usize,
    /// the upper bound of the algebra's depth
    pub depth: usize,
}

/// Where and how a text nests too deeply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TooDeep {
    /// the byte offset of the token that went over a limit
    pub offset: usize,
    pub message: String,
}

/// Refuse a query or update that nests deeper than [`MAX_NESTING`] or [`MAX_DEPTH`].
pub fn check(text: &str) -> Result<(), TooDeep> {
    Scanner::new(text, MAX_NESTING, MAX_DEPTH).run().map(|_| ())
}

/// How deeply `text` nests, with no limit.
pub fn measure(text: &str) -> Nesting {
    Scanner::new(text, usize::MAX, usize::MAX)
        .run()
        .unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// outside any bracket (once per update operation)
    Top,
    /// `{`: a group graph pattern, a template or a data block
    Brace,
    /// `(`: an expression, an argument list, a property path or a collection
    Paren,
    /// `[`, `{|`, `<<`, `<<(`: triples about a blank node, a reifier or a triple term
    Triples,
}

struct Level {
    kind: Kind,
    /// the unary `!`s written before the bracket (they nest around it)
    bangs: usize,
    /// the elements of the chains this level holds
    chain: usize,
    /// the depth of the deepest level closed inside this one
    inner: usize,
    /// inside a data block (`INSERT DATA`, `DELETE DATA`, `VALUES`): terms, no chains
    data: bool,
    /// the index of the group (`Top` or `Brace`) whose chain this level's triples join
    group: usize,
}

struct Scanner<'a> {
    text: &'a str,
    b: &'a [u8],
    i: usize,
    max_nesting: usize,
    max_depth: usize,
    levels: Vec<Level>,
    /// the brackets and `!`s open now
    nesting: usize,
    /// the sum over the open levels of their brackets, `!`s and chains: the final depth
    /// is at least this, so the scan stops as soon as it is over the limit
    open_depth: usize,
    /// the `!`s before an operand not reached yet
    bangs: usize,
    /// the next `{` opens a data block (after `DATA`)
    data_next: bool,
    /// a `VALUES` at this level waits for its `{`
    values_at: Option<usize>,
    found: Nesting,
}

impl<'a> Scanner<'a> {
    fn new(text: &'a str, max_nesting: usize, max_depth: usize) -> Self {
        Self {
            text,
            b: text.as_bytes(),
            i: 0,
            max_nesting,
            max_depth,
            levels: vec![Level {
                kind: Kind::Top,
                bangs: 0,
                chain: 0,
                inner: 0,
                data: false,
                group: 0,
            }],
            nesting: 0,
            open_depth: 0,
            bangs: 0,
            data_next: false,
            values_at: None,
            found: Nesting::default(),
        }
    }

    fn run(mut self) -> Result<Nesting, TooDeep> {
        while self.i < self.b.len() {
            self.token()?;
        }
        while self.levels.len() > 1 {
            self.close(0)?;
        }
        self.release_bangs();
        self.end_operation()?;
        Ok(self.found)
    }

    fn at(&self, j: usize) -> u8 {
        self.b.get(j).copied().unwrap_or(0)
    }

    fn starts_with(&self, s: &[u8]) -> bool {
        self.b[self.i..].starts_with(s)
    }

    fn top(&self) -> &Level {
        self.levels.last().expect("the top level is never closed")
    }

    fn token(&mut self) -> Result<(), TooDeep> {
        let c = self.b[self.i];
        match c {
            b' ' | b'\t' | b'\r' | b'\n' => self.i += 1,
            b'#' => {
                while self.i < self.b.len() && !matches!(self.b[self.i], b'\n' | b'\r') {
                    self.i += 1;
                }
            }
            b'"' | b'\'' => {
                self.string(c);
                self.operand();
            }
            b'<' if self.starts_with(b"<<(") => self.open(Kind::Triples, 3)?,
            b'<' if self.starts_with(b"<<") => self.open(Kind::Triples, 2)?,
            b'<' => match self.iri_end() {
                Some(end) => {
                    self.i = end;
                    self.callee();
                }
                None => {
                    // `<` or `<=`
                    self.i += if self.at(self.i + 1) == b'=' { 2 } else { 1 };
                    self.operand();
                }
            },
            b'>' if self.starts_with(b">>") => self.close(2)?,
            b'>' => {
                self.i += if self.at(self.i + 1) == b'=' { 2 } else { 1 };
                self.operand();
            }
            b'(' => self.open(Kind::Paren, 1)?,
            b')' if self.starts_with(b")>>") => self.close(3)?,
            b')' => self.close(1)?,
            b'[' => self.open(Kind::Triples, 1)?,
            b']' => self.close(1)?,
            b'{' if self.starts_with(b"{|") => self.open(Kind::Triples, 2)?,
            b'{' => self.open(Kind::Brace, 1)?,
            b'}' => self.close(1)?,
            b'|' if self.starts_with(b"|}") => self.close(2)?,
            b'|' if self.starts_with(b"||") => self.operator(2, true, false)?,
            // an alternative in a property path
            b'|' => self.operator(1, true, true)?,
            b'&' if self.starts_with(b"&&") => self.operator(2, true, false)?,
            b'!' if self.starts_with(b"!=") => {
                self.i += 2;
                self.operand();
            }
            b'!' if self.top().kind == Kind::Paren => {
                // `!e`: the parser recurses once per `!`, around the operand
                self.i += 1;
                self.bangs += 1;
                self.check_nesting(self.nesting + self.bangs)?;
                self.check_depth(self.open_depth + self.bangs)?;
            }
            // a negated property set, in a path
            b'!' => self.operator(1, false, true)?,
            b'^' if self.starts_with(b"^^") => {
                self.i += 2;
                self.operand();
            }
            // an inverse path
            b'^' => self.operator(1, false, true)?,
            // in an expression an operator (a sign too: `?a -1` is a subtraction); in a
            // triple a step or modifier of a property path
            b'+' | b'*' | b'/' => self.operator(1, true, true)?,
            b'-' => self.operator(1, true, false)?,
            b'?' | b'$' if self.is_name_char(self.at(self.i + 1)) => {
                self.i += 1;
                while self.is_name_char(self.at(self.i)) {
                    self.i += 1;
                }
                // a variable between VALUES and its `{`
                let values_at = self.values_at;
                self.operand();
                self.values_at = values_at;
            }
            // the `?` modifier of a property path
            b'?' => self.operator(1, false, true)?,
            b'@' => {
                self.i += 1;
                while self.at(self.i).is_ascii_alphanumeric() || self.at(self.i) == b'-' {
                    self.i += 1;
                }
                self.operand();
            }
            b'0'..=b'9' => {
                self.number();
                self.operand();
            }
            b'.' if self.at(self.i + 1).is_ascii_digit() => {
                self.number();
                self.operand();
            }
            b'_' if self.at(self.i + 1) == b':' => {
                self.i += 2;
                while self.is_name_char(self.at(self.i)) || matches!(self.at(self.i), b'-' | b'.')
                {
                    self.i += 1;
                }
                self.trailing_dots();
                self.operand();
            }
            b':' | b'_' => self.name(),
            c if c.is_ascii_alphabetic() || c >= 0x80 => self.name(),
            b';' if self.levels.len() == 1 => {
                // the end of an update operation
                self.i += 1;
                self.operand();
                self.end_operation()?;
            }
            _ => {
                // `,`, `.`, `;`, `=`, `~`, `$`, `\` and characters no token starts with
                self.i += 1;
                self.operand();
            }
        }
        Ok(())
    }

    fn is_name_char(&self, c: u8) -> bool {
        c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
    }

    /// A keyword, a function name, `a`, `true`, or a prefixed name. `-` and `.` belong to
    /// a name only up to its `:` or after it: `true-1` is `true` and `-1`.
    fn name(&mut self) {
        let start = self.i;
        let mut j = self.i;
        while self.is_name_char(self.at(j)) {
            j += 1;
        }
        let mut k = j;
        while self.is_name_char(self.at(k)) || matches!(self.at(k), b'-' | b'.') {
            k += 1;
        }
        if self.at(k) != b':' {
            self.i = j;
            self.callee();
            let text = self.text;
            let word = &text[start..j];
            if word.eq_ignore_ascii_case("DATA") {
                self.data_next = true;
            } else if word.eq_ignore_ascii_case("VALUES") {
                self.values_at = Some(self.levels.len());
            }
            return;
        }
        // a prefixed name; its local part may hold `:`, `%XX` and `\` escapes
        self.i = k + 1;
        loop {
            let c = self.at(self.i);
            if self.is_name_char(c) || matches!(c, b'-' | b'.' | b':' | b'%') {
                self.i += 1;
            } else if c == b'\\' && self.i + 1 < self.b.len() {
                self.i += 2;
            } else {
                break;
            }
        }
        self.trailing_dots();
        self.callee();
    }

    /// A name does not end with an unescaped `.`: `ex:a.` is `ex:a` and `.`.
    fn trailing_dots(&mut self) {
        while self.b[self.i - 1] == b'.' && self.b[self.i - 2] != b'\\' {
            self.i -= 1;
        }
    }

    fn number(&mut self) {
        while self.at(self.i).is_ascii_digit() {
            self.i += 1;
        }
        if self.at(self.i) == b'.' && self.at(self.i + 1).is_ascii_digit() {
            self.i += 1;
            while self.at(self.i).is_ascii_digit() {
                self.i += 1;
            }
        }
        if matches!(self.at(self.i), b'e' | b'E') {
            let mut j = self.i + 1;
            if matches!(self.at(j), b'+' | b'-') {
                j += 1;
            }
            if self.at(j).is_ascii_digit() {
                self.i = j;
                while self.at(self.i).is_ascii_digit() {
                    self.i += 1;
                }
            }
        }
    }

    /// Skip a string, or only its quote when no string token starts there. A long string
    /// that does not end is the empty string and what follows, as the longest match reads
    /// it.
    fn string(&mut self, q: u8) {
        if self.at(self.i + 1) == q && self.at(self.i + 2) == q {
            let mut j = self.i + 3;
            while j < self.b.len() {
                if self.b[j] == q && self.at(j + 1) == q && self.at(j + 2) == q {
                    self.i = j + 3;
                    return;
                }
                if self.b[j] == b'\\' {
                    match self.escape(j) {
                        Some(len) => j += len,
                        None => break,
                    }
                } else {
                    j += 1;
                }
            }
        }
        let mut j = self.i + 1;
        while j < self.b.len() {
            match self.b[j] {
                c if c == q => {
                    self.i = j + 1;
                    return;
                }
                b'\n' | b'\r' => break,
                b'\\' => match self.escape(j) {
                    Some(len) => j += len,
                    None => break,
                },
                _ => j += 1,
            }
        }
        self.i += 1;
    }

    /// The length of the escape sequence at `j` (`\n`, `\"`, `é`, …), if valid.
    fn escape(&self, j: usize) -> Option<usize> {
        match self.at(j + 1) {
            b't' | b'b' | b'n' | b'r' | b'f' | b'"' | b'\'' | b'\\' => Some(2),
            b'u' => self.hex(j + 2, 4).then_some(6),
            b'U' => self.hex(j + 2, 8).then_some(10),
            _ => None,
        }
    }

    fn hex(&self, from: usize, n: usize) -> bool {
        (from..from + n).all(|j| self.at(j).is_ascii_hexdigit())
    }

    /// The end of the IRI at `<`, by the grammar's `IRIREF` terminal (so in `?a < ?b` and
    /// `?a <?b` the `<` is an operator).
    fn iri_end(&self) -> Option<usize> {
        let mut j = self.i + 1;
        while j < self.b.len() {
            match self.b[j] {
                b'>' => return Some(j + 1),
                b'<' | b'"' | b'{' | b'}' | b'|' | b'^' | b'`' | 0..=b' ' => return None,
                b'\\' => match self.at(j + 1) {
                    b'u' if self.hex(j + 2, 4) => j += 6,
                    b'U' if self.hex(j + 2, 8) => j += 10,
                    _ => return None,
                },
                _ => j += 1,
            }
        }
        None
    }

    fn check_nesting(&self, nesting: usize) -> Result<(), TooDeep> {
        if nesting > self.max_nesting {
            return Err(TooDeep {
                offset: self.i,
                message: format!("nested deeper than {} levels", self.max_nesting),
            });
        }
        Ok(())
    }

    fn check_depth(&self, depth: usize) -> Result<(), TooDeep> {
        if depth > self.max_depth {
            return Err(TooDeep {
                offset: self.i,
                message: format!(
                    "nested deeper than {} levels, counting each operator of an expression, \
                     step of a property path and element of a group as a level",
                    self.max_depth
                ),
            });
        }
        Ok(())
    }

    /// The pending `!`s apply to the operand that ends here.
    fn release_bangs(&mut self) {
        if self.bangs > 0 {
            let bangs = std::mem::take(&mut self.bangs);
            self.found.brackets = self.found.brackets.max(self.nesting + bangs);
            let top = self.levels.last_mut().expect("the top level is never closed");
            top.inner = top.inner.max(bangs);
        }
    }

    /// A name or IRI: it may be a function, or `EXISTS`, so pending `!`s wait for a
    /// bracket that follows.
    fn callee(&mut self) {
        self.data_next = false;
        if self.values_at == Some(self.levels.len()) {
            self.values_at = None;
        }
    }

    /// A token that ends an operand.
    fn operand(&mut self) {
        self.release_bangs();
        self.callee();
    }

    /// An operator of `len` bytes: one more element of the chain of the expression
    /// (`expr`) or of the group whose triples hold the path (`path`) at this level.
    fn operator(&mut self, len: usize, expr: bool, path: bool) -> Result<(), TooDeep> {
        self.i += len;
        self.operand();
        let top = self.top();
        let owner = match top.kind {
            _ if top.data => return Ok(()),
            Kind::Paren if expr => self.levels.len() - 1,
            Kind::Paren => return Ok(()),
            _ if path => top.group,
            _ => return Ok(()),
        };
        self.levels[owner].chain += 1;
        self.open_depth += 1;
        self.check_depth(self.open_depth)
    }

    fn open(&mut self, kind: Kind, len: usize) -> Result<(), TooDeep> {
        let parent = self.top();
        // a nested group, FILTER or BIND, a collection or a parenthesized path in a group is
        // one more element of the group's chain
        let element = !parent.data
            && match parent.kind {
                Kind::Top | Kind::Brace => matches!(kind, Kind::Brace | Kind::Paren),
                Kind::Triples => kind == Kind::Paren,
                Kind::Paren => false,
            };
        let at = self.levels.len();
        let data = parent.data
            || (kind == Kind::Brace && (self.data_next || self.values_at == Some(at)));
        let group = if kind == Kind::Brace {
            at
        } else {
            parent.group
        };
        if element {
            let owner = parent.group;
            self.levels[owner].chain += 1;
            self.open_depth += 1;
        }
        if kind == Kind::Brace && self.values_at == Some(at) {
            self.values_at = None;
        }
        self.data_next = false;
        let bangs = std::mem::take(&mut self.bangs);
        self.nesting += 1 + bangs;
        self.open_depth += 1 + bangs;
        self.check_nesting(self.nesting)?;
        self.check_depth(self.open_depth)?;
        self.found.brackets = self.found.brackets.max(self.nesting);
        self.levels.push(Level {
            kind,
            bangs,
            chain: 0,
            inner: 0,
            data,
            group,
        });
        self.i += len;
        Ok(())
    }

    fn close(&mut self, len: usize) -> Result<(), TooDeep> {
        self.release_bangs();
        self.data_next = false;
        self.i += len;
        if self.levels.len() == 1 {
            // a closer without an opener: the parser stops here
            return Ok(());
        }
        let level = self.levels.pop().expect("more than the top level");
        if self.values_at.is_some_and(|at| at > self.levels.len()) {
            self.values_at = None;
        }
        self.nesting -= 1 + level.bangs;
        let own = 1 + level.bangs + level.chain;
        self.open_depth -= own;
        let depth = own + level.inner;
        self.check_depth(self.open_depth + depth)?;
        let parent = self.levels.last_mut().expect("the top level is never closed");
        parent.inner = parent.inner.max(depth);
        Ok(())
    }

    /// The end of an update operation: the next one starts its own chains.
    fn end_operation(&mut self) -> Result<(), TooDeep> {
        let top = &mut self.levels[0];
        let depth = top.chain + top.inner;
        self.open_depth -= top.chain;
        top.chain = 0;
        top.inner = 0;
        self.found.depth = self.found.depth.max(depth);
        self.check_depth(depth)
    }
}
