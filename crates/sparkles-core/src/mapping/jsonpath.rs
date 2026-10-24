//! The JSONPath subset of RFC 9535 that RML iterators and references use: the root `$`
//! (and `@` for the current node), member names in dot and bracket notation, array
//! indexes (negative from the end), slices, wildcards, unions of names or indexes,
//! descendants (`..`), and filters that compare a relative path with a literal or test
//! that it exists, joined with `&&`, `||` and `!`. A reference without `$` or `@` is
//! relative to the iteration (`name` is `$.name`). Functions such as `length()` are not
//! read and fail with a message.

use serde_json::Value as J;

#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    segs: Vec<Seg>,
}

#[derive(Clone, Debug, PartialEq)]
enum Seg {
    Child(Vec<Sel>),
    Desc(Vec<Sel>),
}

#[derive(Clone, Debug, PartialEq)]
enum Sel {
    Name(String),
    Wild,
    Index(i64),
    Slice(Option<i64>, Option<i64>, i64),
    Filter(Box<Filter>),
}

#[derive(Clone, Debug, PartialEq)]
enum Filter {
    Or(Box<Filter>, Box<Filter>),
    And(Box<Filter>, Box<Filter>),
    Not(Box<Filter>),
    Exists(Path),
    Cmp(Operand, Op, Operand),
}

#[derive(Clone, Debug, PartialEq)]
enum Operand {
    Path(Path),
    Lit(J),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Path {
    /// Parse an iterator or reference.
    pub fn parse(s: &str) -> Result<Path, String> {
        let mut p = P {
            s: s.as_bytes(),
            i: 0,
            text: s,
        };
        let path = p.path(true)?;
        p.ws();
        if p.i < p.s.len() {
            return Err(p.err("unexpected text"));
        }
        Ok(path)
    }

    /// The nodes the path selects from `root`, in document order.
    pub fn select<'a>(&self, root: &'a J) -> Vec<&'a J> {
        let mut cur = vec![root];
        for seg in &self.segs {
            let mut next = Vec::new();
            match seg {
                Seg::Child(sels) => {
                    for n in &cur {
                        for s in sels {
                            apply(s, n, &mut next);
                        }
                    }
                }
                Seg::Desc(sels) => {
                    for n in &cur {
                        let mut all = Vec::new();
                        descendants(n, &mut all);
                        for d in all {
                            for s in sels {
                                apply(s, d, &mut next);
                            }
                        }
                    }
                }
            }
            cur = next;
        }
        cur
    }

    /// Whether the path is `$` alone.
    pub fn is_root(&self) -> bool {
        self.segs.is_empty()
    }
}

fn descendants<'a>(n: &'a J, out: &mut Vec<&'a J>) {
    out.push(n);
    match n {
        J::Array(a) => a.iter().for_each(|c| descendants(c, out)),
        J::Object(o) => o.values().for_each(|c| descendants(c, out)),
        _ => {}
    }
}

fn apply<'a>(s: &Sel, n: &'a J, out: &mut Vec<&'a J>) {
    match s {
        Sel::Name(k) => {
            if let J::Object(o) = n
                && let Some(v) = o.get(k)
            {
                out.push(v);
            }
        }
        Sel::Wild => match n {
            J::Array(a) => out.extend(a.iter()),
            J::Object(o) => out.extend(o.values()),
            _ => {}
        },
        Sel::Index(i) => {
            if let J::Array(a) = n {
                let len = a.len() as i64;
                let k = if *i < 0 { len + i } else { *i };
                if (0..len).contains(&k) {
                    out.push(&a[k as usize]);
                }
            }
        }
        Sel::Slice(start, end, step) => {
            if let J::Array(a) = n {
                let len = a.len() as i64;
                let norm = |v: i64| if v < 0 { (len + v).max(0) } else { v.min(len) };
                if *step > 0 {
                    let (mut i, e) = (norm(start.unwrap_or(0)), norm(end.unwrap_or(len)));
                    while i < e {
                        out.push(&a[i as usize]);
                        i += step;
                    }
                } else if *step < 0 {
                    let mut i =
                        start.map_or(len - 1, |v| if v < 0 { len + v } else { v.min(len - 1) });
                    let e = end.map_or(-1, |v| if v < 0 { (len + v).max(-1) } else { v });
                    while i > e {
                        if i >= 0 {
                            out.push(&a[i as usize]);
                        }
                        i += step;
                    }
                }
            }
        }
        Sel::Filter(f) => {
            let kids: Vec<&J> = match n {
                J::Array(a) => a.iter().collect(),
                J::Object(o) => o.values().collect(),
                _ => Vec::new(),
            };
            for k in kids {
                if test(f, k) {
                    out.push(k);
                }
            }
        }
    }
}

fn test(f: &Filter, n: &J) -> bool {
    match f {
        Filter::Or(a, b) => test(a, n) || test(b, n),
        Filter::And(a, b) => test(a, n) && test(b, n),
        Filter::Not(a) => !test(a, n),
        Filter::Exists(p) => !p.select(n).is_empty(),
        Filter::Cmp(a, op, b) => {
            let one = |o: &Operand| -> Option<J> {
                match o {
                    Operand::Lit(v) => Some(v.clone()),
                    Operand::Path(p) => {
                        let v = p.select(n);
                        (v.len() == 1).then(|| v[0].clone())
                    }
                }
            };
            let (x, y) = (one(a), one(b));
            match op {
                Op::Eq => eq(x.as_ref(), y.as_ref()),
                Op::Ne => !eq(x.as_ref(), y.as_ref()),
                Op::Lt => lt(x.as_ref(), y.as_ref()),
                Op::Le => lt(x.as_ref(), y.as_ref()) || eq(x.as_ref(), y.as_ref()),
                Op::Gt => lt(y.as_ref(), x.as_ref()),
                Op::Ge => lt(y.as_ref(), x.as_ref()) || eq(x.as_ref(), y.as_ref()),
            }
        }
    }
}

fn eq(a: Option<&J>, b: Option<&J>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(J::Number(x)), Some(J::Number(y))) => x.as_f64() == y.as_f64(),
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

fn lt(a: Option<&J>, b: Option<&J>) -> bool {
    match (a, b) {
        (Some(J::Number(x)), Some(J::Number(y))) => x.as_f64() < y.as_f64(),
        (Some(J::String(x)), Some(J::String(y))) => x < y,
        _ => false,
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
    text: &'a str,
}

impl P<'_> {
    fn err(&self, m: &str) -> String {
        format!("JSONPath {:?}: {m} at position {}", self.text, self.i + 1)
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn eat(&mut self, b: u8) -> bool {
        if self.peek() == Some(b) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    /// A path. At the top, a path without `$` or `@` starts with a member name.
    fn path(&mut self, top: bool) -> Result<Path, String> {
        let mut segs = Vec::new();
        self.ws();
        if !(self.eat(b'$') || self.eat(b'@')) {
            if !top {
                return Err(self.err("a path in a filter starts with @ or $"));
            }
            // a bare reference: the first member name, up to `.` or `[`
            let name = self.bare_name();
            if name.is_empty() && self.peek() != Some(b'[') {
                return Err(self.err("an empty reference"));
            }
            if !name.is_empty() {
                segs.push(if name == "*" {
                    Seg::Child(vec![Sel::Wild])
                } else {
                    Seg::Child(vec![Sel::Name(name)])
                });
            }
        }
        loop {
            match self.peek() {
                Some(b'.') => {
                    self.i += 1;
                    if self.eat(b'.') {
                        if self.peek() == Some(b'[') {
                            segs.push(Seg::Desc(self.bracket()?));
                        } else {
                            let n = self.dot_name()?;
                            segs.push(Seg::Desc(vec![n]));
                        }
                    } else {
                        let n = self.dot_name()?;
                        segs.push(Seg::Child(vec![n]));
                    }
                }
                Some(b'[') => segs.push(Seg::Child(self.bracket()?)),
                _ => break,
            }
        }
        Ok(Path { segs })
    }

    fn bare_name(&mut self) -> String {
        let start = self.i;
        while let Some(b) = self.peek() {
            if b == b'.' || b == b'[' {
                break;
            }
            self.i += 1;
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    fn dot_name(&mut self) -> Result<Sel, String> {
        if self.eat(b'*') {
            return Ok(Sel::Wild);
        }
        let start = self.i;
        while let Some(b) = self.peek() {
            if b == b'.'
                || b == b'['
                || b == b' '
                || b == b')'
                || b == b'='
                || b == b'!'
                || b == b'<'
                || b == b'>'
                || b == b'&'
                || b == b'|'
                || b == b']'
                || b == b','
            {
                break;
            }
            self.i += 1;
        }
        if start == self.i {
            return Err(self.err("a member name is missing"));
        }
        let name = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        if self.peek() == Some(b'(') {
            return Err(self.err("functions are not supported"));
        }
        Ok(Sel::Name(name))
    }

    fn bracket(&mut self) -> Result<Vec<Sel>, String> {
        self.i += 1; // [
        let mut out = Vec::new();
        loop {
            self.ws();
            let sel = match self.peek() {
                Some(b'*') => {
                    self.i += 1;
                    Sel::Wild
                }
                Some(b'\'' | b'"') => Sel::Name(self.string()?),
                Some(b'?') => {
                    self.i += 1;
                    self.ws();
                    let paren = self.eat(b'(');
                    let f = self.or()?;
                    self.ws();
                    if paren && !self.eat(b')') {
                        return Err(self.err("a ) is missing"));
                    }
                    Sel::Filter(Box::new(f))
                }
                Some(b'-' | b'0'..=b'9' | b':') => self.index_or_slice()?,
                _ => return Err(self.err("expected a name, an index, * or a filter")),
            };
            out.push(sel);
            self.ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b']') {
                return Ok(out);
            }
            return Err(self.err("a ] is missing"));
        }
    }

    fn int(&mut self) -> Option<i64> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while self.peek().is_some_and(|b| b.is_ascii_digit()) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    fn index_or_slice(&mut self) -> Result<Sel, String> {
        let a = if self.peek() == Some(b':') {
            None
        } else {
            Some(self.int().ok_or_else(|| self.err("not an index"))?)
        };
        self.ws();
        if !self.eat(b':') {
            return Ok(Sel::Index(a.expect("an index")));
        }
        self.ws();
        let b = if matches!(self.peek(), Some(b'-' | b'0'..=b'9')) {
            self.int()
        } else {
            None
        };
        self.ws();
        let step = if self.eat(b':') {
            self.ws();
            if matches!(self.peek(), Some(b'-' | b'0'..=b'9')) {
                self.int().unwrap_or(1)
            } else {
                1
            }
        } else {
            1
        };
        Ok(Sel::Slice(a, b, step))
    }

    fn string(&mut self) -> Result<String, String> {
        let q = self.s[self.i];
        self.i += 1;
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            self.i += 1;
            if b == q {
                return String::from_utf8(out).map_err(|_| self.err("not UTF-8"));
            }
            if b == b'\\' {
                let Some(e) = self.peek() else { break };
                self.i += 1;
                match e {
                    b'n' => out.push(b'\n'),
                    b't' => out.push(b'\t'),
                    b'r' => out.push(b'\r'),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'/' | b'\\' | b'\'' | b'"' => out.push(e),
                    b'u' => {
                        let hex = self
                            .s
                            .get(self.i..self.i + 4)
                            .and_then(|h| std::str::from_utf8(h).ok())
                            .and_then(|h| u32::from_str_radix(h, 16).ok())
                            .and_then(char::from_u32)
                            .ok_or_else(|| self.err("a bad \\u escape"))?;
                        self.i += 4;
                        let mut b = [0u8; 4];
                        out.extend_from_slice(hex.encode_utf8(&mut b).as_bytes());
                    }
                    _ => return Err(self.err("a bad escape")),
                }
            } else {
                out.push(b);
            }
        }
        Err(self.err("an unclosed string"))
    }

    fn or(&mut self) -> Result<Filter, String> {
        let mut a = self.and()?;
        loop {
            self.ws();
            if self.s[self.i..].starts_with(b"||") {
                self.i += 2;
                let b = self.and()?;
                a = Filter::Or(Box::new(a), Box::new(b));
            } else {
                return Ok(a);
            }
        }
    }

    fn and(&mut self) -> Result<Filter, String> {
        let mut a = self.unary()?;
        loop {
            self.ws();
            if self.s[self.i..].starts_with(b"&&") {
                self.i += 2;
                let b = self.unary()?;
                a = Filter::And(Box::new(a), Box::new(b));
            } else {
                return Ok(a);
            }
        }
    }

    fn unary(&mut self) -> Result<Filter, String> {
        self.ws();
        if self.peek() == Some(b'!') && self.s.get(self.i + 1) != Some(&b'=') {
            self.i += 1;
            return Ok(Filter::Not(Box::new(self.unary()?)));
        }
        if self.eat(b'(') {
            let f = self.or()?;
            self.ws();
            if !self.eat(b')') {
                return Err(self.err("a ) is missing"));
            }
            return Ok(f);
        }
        let a = self.operand()?;
        self.ws();
        let op = if self.s[self.i..].starts_with(b"==") {
            Some(Op::Eq)
        } else if self.s[self.i..].starts_with(b"!=") {
            Some(Op::Ne)
        } else if self.s[self.i..].starts_with(b"<=") {
            Some(Op::Le)
        } else if self.s[self.i..].starts_with(b">=") {
            Some(Op::Ge)
        } else if self.peek() == Some(b'<') {
            Some(Op::Lt)
        } else if self.peek() == Some(b'>') {
            Some(Op::Gt)
        } else {
            None
        };
        let Some(op) = op else {
            return match a {
                Operand::Path(p) => Ok(Filter::Exists(p)),
                Operand::Lit(_) => Err(self.err("a literal is not a test")),
            };
        };
        self.i += if matches!(op, Op::Lt | Op::Gt) { 1 } else { 2 };
        let b = self.operand()?;
        Ok(Filter::Cmp(a, op, b))
    }

    fn operand(&mut self) -> Result<Operand, String> {
        self.ws();
        match self.peek() {
            Some(b'@' | b'$') => Ok(Operand::Path(self.path(false)?)),
            Some(b'\'' | b'"') => Ok(Operand::Lit(J::String(self.string()?))),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                self.i += 1;
                while self.peek().is_some_and(|b| {
                    b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-')
                }) {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("");
                serde_json::from_str::<J>(t)
                    .map(Operand::Lit)
                    .map_err(|_| self.err("not a number"))
            }
            _ => {
                for (w, v) in [
                    ("true", J::Bool(true)),
                    ("false", J::Bool(false)),
                    ("null", J::Null),
                ] {
                    if self.s[self.i..].starts_with(w.as_bytes()) {
                        self.i += w.len();
                        return Ok(Operand::Lit(v));
                    }
                }
                Err(self.err("expected a path or a literal"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sel(p: &str, v: &J) -> Vec<J> {
        Path::parse(p)
            .unwrap()
            .select(v)
            .into_iter()
            .cloned()
            .collect()
    }

    #[test]
    fn selects() {
        let v = json!({"people": [{"id": 7, "tags": ["a", "b"], "n": {"x": 1}},
                                  {"id": 8, "tags": [], "n": {"x": 2}}],
                       "odd key": 3});
        assert_eq!(sel("$.people[*].id", &v), vec![json!(7), json!(8)]);
        assert_eq!(sel("$.people[0].tags[*]", &v), vec![json!("a"), json!("b")]);
        assert_eq!(
            sel("tags[*]", &v["people"][0]),
            vec![json!("a"), json!("b")]
        );
        assert_eq!(sel("id", &v["people"][0]), vec![json!(7)]);
        assert_eq!(sel("n.x", &v["people"][1]), vec![json!(2)]);
        assert_eq!(sel("$['odd key']", &v), vec![json!(3)]);
        assert_eq!(sel("odd key", &v), vec![json!(3)]);
        assert_eq!(sel("$..x", &v), vec![json!(1), json!(2)]);
        assert_eq!(sel("$.people[-1].id", &v), vec![json!(8)]);
        assert_eq!(sel("$.people[0:1].id", &v), vec![json!(7)]);
        assert_eq!(sel("$.people[?(@.id == 8)].n.x", &v), vec![json!(2)]);
        assert_eq!(
            sel("$.people[?@.id > 7 && @.n.x < 3].id", &v),
            vec![json!(8)]
        );
        assert_eq!(sel("$.people[?(@.missing)].id", &v), Vec::<J>::new());
        assert_eq!(sel("$", &v).len(), 1);
        assert!(Path::parse("$.a.length()").is_err());
        assert!(Path::parse("$.a[").is_err());
    }
}
