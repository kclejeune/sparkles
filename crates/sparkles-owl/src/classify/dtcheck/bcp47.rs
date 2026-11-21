//! Well-formed language tags of BCP 47 (RFC 5646 §2.1), which RDF requires of the tags
//! of language-tagged strings, and the basic language ranges that can match them.
//!
//! Tags and ranges are compared in lower case. The tags matched by a basic range are the
//! range itself and the tags that extend it by whole subtags (RFC 4647 §3.3.1).

/// The irregular grandfathered tags, the only well-formed tags outside the `langtag` and
/// `privateuse` productions. Each is a whole tag that nothing extends.
pub const IRREGULAR: [&str; 17] = [
    "en-gb-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "sgn-be-fr",
    "sgn-be-nl",
    "sgn-ch-de",
];

/// A state of the `langtag` and `privateuse` productions after some whole subtags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St {
    Start,
    /// a language of two or three letters, then `n` extended language subtags
    Lang(u8),
    /// a language of four to eight letters, or the three extended language subtags
    AfterLang,
    AfterScript,
    /// a region or variants
    AfterRegion,
    /// a singleton that still needs a subtag
    ExtStart,
    ExtBody,
    /// `x` that still needs a subtag
    PrivStart,
    PrivBody,
}

fn alpha(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphabetic())
}
fn digit(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_digit())
}
fn alnum(s: &str) -> bool {
    s.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn step(st: St, s: &str) -> Option<St> {
    let n = s.len();
    if !(1..=8).contains(&n) || !alnum(s) {
        return None;
    }
    let script = n == 4 && alpha(s);
    let region = (n == 2 && alpha(s)) || (n == 3 && digit(s));
    let variant = (5..=8).contains(&n) || (n == 4 && s.as_bytes()[0].is_ascii_digit());
    let singleton = n == 1 && s != "x";
    let private = s == "x";
    // what may follow a language, a script or a region, in that order
    let tail = |at: u8| -> Option<St> {
        if at < 1 && script {
            Some(St::AfterScript)
        } else if at < 2 && region || variant {
            Some(St::AfterRegion)
        } else if singleton {
            Some(St::ExtStart)
        } else if private {
            Some(St::PrivStart)
        } else {
            None
        }
    };
    match st {
        St::Start => {
            if private {
                Some(St::PrivStart)
            } else if alpha(s) && (2..=3).contains(&n) {
                Some(St::Lang(0))
            } else if alpha(s) && n >= 4 {
                Some(St::AfterLang)
            } else {
                None
            }
        }
        St::Lang(k) if n == 3 && alpha(s) && k < 3 => Some(if k == 2 {
            St::AfterLang
        } else {
            St::Lang(k + 1)
        }),
        St::Lang(_) | St::AfterLang => tail(0),
        St::AfterScript => tail(1),
        St::AfterRegion => tail(2),
        St::ExtStart => (n >= 2).then_some(St::ExtBody),
        St::ExtBody => {
            if n >= 2 {
                Some(St::ExtBody)
            } else {
                tail(3)
            }
        }
        St::PrivStart | St::PrivBody => Some(St::PrivBody),
    }
}

/// The state after the subtags of `t`, or `None` when no `langtag` or `privateuse` tag
/// starts with them.
fn run(t: &str) -> Option<St> {
    let mut st = St::Start;
    for s in t.split('-') {
        st = step(st, &s.to_ascii_lowercase())?;
    }
    Some(st)
}

/// Whether `tag` is a well-formed language tag.
pub fn well_formed(tag: &str) -> bool {
    let t = tag.to_ascii_lowercase();
    IRREGULAR.contains(&t.as_str())
        || run(&t).is_some_and(|s| !matches!(s, St::ExtStart | St::PrivStart))
}

/// Whether `p` is `t` or a prefix of it by whole subtags.
pub fn prefix(p: &str, t: &str) -> bool {
    t == p || t.strip_prefix(p).is_some_and(|r| r.starts_with('-'))
}

/// A well-formed tag in the `langtag` or `privateuse` productions that the basic range
/// `r` matches, followed by the subtag that opens a private use part when it has none,
/// or `None` when the range matches no such tag. Every tag that extends the result is
/// well-formed and matched by `r`, so those beyond it are infinitely many.
pub fn open_completion(r: &str) -> Option<String> {
    let r = r.to_ascii_lowercase();
    Some(match run(&r)? {
        St::ExtStart => format!("{r}-aa-x"),
        St::PrivStart => r,
        St::PrivBody => r,
        _ => format!("{r}-x"),
    })
}
