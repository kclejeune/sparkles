//! Fuseki's user files: Shiro's `shiro.ini` (users, roles, URL rules) and the Jetty
//! realm file that `fuseki:passwd` names. Passwords are kept in memory only as long as it
//! takes to hash them, and they never reach a message.

use anyhow::{Context, Result};
use std::path::Path;

/// A password as the file gives it.
#[derive(Clone)]
pub enum Password {
    /// plain text, which can be hashed
    Plain(String),
    /// a form that cannot be turned into an argon2id hash; the string names the form
    Opaque(&'static str),
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Password::Plain(_) => f.write_str("Plain(<redacted>)"),
            Password::Opaque(k) => write!(f, "Opaque({k})"),
        }
    }
}

#[cfg(feature = "auth")]
impl Drop for Password {
    fn drop(&mut self) {
        if let Password::Plain(s) = self {
            // overwrite the bytes before the allocation is released
            zeroize::Zeroize::zeroize(s);
        }
    }
}

#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub password: Password,
    pub roles: Vec<String>,
}

/// One filter of a Shiro URL rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Filter {
    /// `anon`: no authentication
    Anon,
    /// `authcBasic`, `authc`, `user`: any authenticated user
    Authenticated,
    /// `roles[a,b]`: users with every one of the roles
    Roles(Vec<String>),
    /// Fuseki's `LocalhostFilter`: requests from the local machine
    Localhost,
    /// a filter Sparkles cannot evaluate (`perms[…]`, custom classes); allows nobody
    Other(String),
    /// filters that do not decide access (`ssl`, `noSessionCreation`, `rest`, `port`)
    Neutral,
}

#[derive(Clone, Debug)]
pub struct UrlRule {
    pub pattern: String,
    pub filters: Vec<Filter>,
}

/// What a `shiro.ini` or password file says.
#[derive(Debug, Default)]
pub struct UserFile {
    pub users: Vec<User>,
    pub urls: Vec<UrlRule>,
    /// `[main]` settings Sparkles cannot follow (other realms, hashing matchers)
    pub main_unsupported: Vec<String>,
    /// passwords are hashed by a `[main]` credentials matcher
    pub hashed_by_matcher: bool,
}

/// Read a `shiro.ini`.
pub fn read_shiro(path: &Path) -> Result<UserFile> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(parse_shiro(&text))
}

/// Logical lines of an INI file: comments dropped, `\` continuations joined.
fn ini_lines(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for raw in text.lines() {
        let line = raw.trim();
        if cur.is_empty() && (line.starts_with('#') || line.starts_with(';')) {
            continue;
        }
        if let Some(l) = line.strip_suffix('\\') {
            cur.push_str(l);
            continue;
        }
        cur.push_str(line);
        let l = std::mem::take(&mut cur);
        if !l.trim().is_empty() {
            out.push(l.trim().to_string());
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// `key = value` or `key: value`, split at the first separator.
fn key_value(line: &str) -> Option<(String, String)> {
    let i = line.find(['=', ':'])?;
    Some((
        line[..i].trim().to_string(),
        line[i + 1..].trim().to_string(),
    ))
}

pub fn parse_shiro(text: &str) -> UserFile {
    let mut f = UserFile::default();
    let mut section = String::new();
    // [main] objects: name → class
    let mut objects: Vec<(String, String)> = Vec::new();
    for line in ini_lines(text) {
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim().to_ascii_lowercase();
            continue;
        }
        let Some((k, v)) = key_value(&line) else {
            continue;
        };
        match section.as_str() {
            "main" | "" => {
                if let Some((obj, prop)) = k.split_once('.') {
                    if prop == "credentialsMatcher" {
                        let class = v
                            .strip_prefix('$')
                            .and_then(|o| objects.iter().find(|(n, _)| n == o))
                            .map(|(_, c)| c.clone())
                            .unwrap_or_else(|| v.clone());
                        if !class.ends_with("SimpleCredentialsMatcher") {
                            f.hashed_by_matcher = true;
                        }
                    } else if obj == "securityManager" && prop == "realms" {
                        f.main_unsupported
                            .push(format!("securityManager.realms = {v}"));
                    } else if objects.iter().any(|(n, c)| n == obj && is_foreign_realm(c)) {
                        // the properties of a realm already reported
                    }
                } else {
                    if is_foreign_realm(&v) {
                        f.main_unsupported.push(format!("{k} = {v}"));
                    }
                    objects.push((k, v));
                }
            }
            "users" => {
                let mut parts = v.split(',').map(str::trim);
                let pw = parts.next().unwrap_or("").to_string();
                let roles = parts.filter(|r| !r.is_empty()).map(String::from).collect();
                let password = if pw.starts_with("$shiro1$") {
                    Password::Opaque("a Shiro hash")
                } else {
                    Password::Plain(pw)
                };
                // a name given twice: the later line wins, as in Shiro
                f.users.retain(|u| u.name != k);
                f.users.push(User {
                    name: k,
                    password,
                    roles,
                });
            }
            "urls" => {
                let filters = split_filters(&v)
                    .into_iter()
                    .map(|name| filter_of(&name, &objects))
                    .collect();
                f.urls.push(UrlRule {
                    pattern: k,
                    filters,
                });
            }
            _ => {}
        }
    }
    if f.hashed_by_matcher {
        for u in &mut f.users {
            if matches!(u.password, Password::Plain(_)) {
                u.password = Password::Opaque("a hash for the configured credentials matcher");
            }
        }
    }
    f
}

fn is_foreign_realm(class: &str) -> bool {
    let c = class.to_ascii_lowercase();
    c.contains("realm") && !c.ends_with("inirealm")
}

/// Split `authcBasic, roles[a, b]` at the commas outside brackets.
fn split_filters(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in v.chars() {
        match c {
            '[' => {
                depth += 1;
                cur.push(c);
            }
            ']' => {
                depth -= 1;
                cur.push(c);
            }
            ',' if depth == 0 => out.push(std::mem::take(&mut cur).trim().to_string()),
            _ => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out.retain(|s| !s.is_empty());
    out
}

fn filter_of(spec: &str, objects: &[(String, String)]) -> Filter {
    let (name, arg) = match spec.split_once('[') {
        Some((n, rest)) => (n.trim(), Some(rest.trim_end_matches(']').trim())),
        None => (spec.trim(), None),
    };
    match name {
        "anon" => Filter::Anon,
        "authcBasic" | "authc" | "user" => Filter::Authenticated,
        "roles" => Filter::Roles(
            arg.unwrap_or("")
                .split(',')
                .map(|r| r.trim().trim_matches('"').to_string())
                .filter(|r| !r.is_empty())
                .collect(),
        ),
        "ssl" | "noSessionCreation" | "port" | "rest" | "invalidRequest" => Filter::Neutral,
        other => match objects.iter().find(|(n, _)| n == other) {
            Some((_, class)) if class.ends_with("LocalhostFilter") => Filter::Localhost,
            _ => Filter::Other(spec.to_string()),
        },
    }
}

/// Read the Jetty realm file of `fuseki:passwd`: `name: password[, role…]`.
pub fn read_passwd(path: &Path) -> Result<UserFile> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Ok(parse_passwd(&text))
}

pub fn parse_passwd(text: &str) -> UserFile {
    let mut f = UserFile::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((k, v)) = key_value(line) else {
            continue;
        };
        let mut parts = v.split(',').map(str::trim);
        let pw = parts.next().unwrap_or("").to_string();
        let roles = parts.filter(|r| !r.is_empty()).map(String::from).collect();
        let password = if pw.starts_with("OBF:") {
            Password::Opaque("an obfuscated (OBF:) password")
        } else if pw.starts_with("MD5:") {
            Password::Opaque("an MD5 hash")
        } else if pw.starts_with("CRYPT:") {
            Password::Opaque("a crypt(3) hash")
        } else {
            Password::Plain(pw)
        };
        f.users.push(User {
            name: k,
            password,
            roles,
        });
    }
    f
}

/// Shiro's Ant-style path match: `**` any number of segments, `*` within a segment, `?`
/// one character.
pub fn ant_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    let s: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&p, &s)
}

fn match_segments(p: &[&str], s: &[&str]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((&"**", rest)) => (0..=s.len()).any(|i| match_segments(rest, &s[i..])),
        Some((seg, rest)) => match s.split_first() {
            Some((first, srest)) => {
                glob(seg.as_bytes(), first.as_bytes()) && match_segments(rest, srest)
            }
            None => false,
        },
    }
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| glob(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && glob(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && glob(rest, &s[1..]),
    }
}

impl UserFile {
    /// The filters of the first URL rule that matches `path`, as Shiro picks them.
    pub fn rule_for(&self, path: &str) -> Option<&UrlRule> {
        self.urls.iter().find(|r| ant_match(&r.pattern, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shiro_sections() {
        let f = parse_shiro(
            "[main]\nlocalhostFilter = org.apache.jena.fuseki.authz.LocalhostFilter\n\
             [users]\nadmin = pw, administrator\nuser1=passwd1\n\
             [roles]\nadministrator = *\n\
             [urls]\n/$/ping = anon\n/$/** = authcBasic, roles[administrator]\n\
             /local/** = localhostFilter\n/**=anon\n",
        );
        assert_eq!(f.users.len(), 2);
        assert_eq!(f.users[0].roles, vec!["administrator"]);
        assert!(matches!(&f.users[1].password, Password::Plain(p) if p == "passwd1"));
        assert_eq!(f.rule_for("/$/ping").unwrap().filters, vec![Filter::Anon]);
        assert_eq!(
            f.rule_for("/$/datasets").unwrap().filters,
            vec![
                Filter::Authenticated,
                Filter::Roles(vec!["administrator".into()])
            ]
        );
        assert_eq!(
            f.rule_for("/local/x").unwrap().filters,
            vec![Filter::Localhost]
        );
        assert_eq!(
            f.rule_for("/ds/sparql").unwrap().filters,
            vec![Filter::Anon]
        );
        assert!(format!("{:?}", f.users[0]).contains("<redacted>"));
    }

    #[test]
    fn hashed_passwords_are_opaque() {
        let f = parse_shiro(
            "[main]\nsha256Matcher = org.apache.shiro.authc.credential.Sha256CredentialsMatcher\n\
             iniRealm.credentialsMatcher = $sha256Matcher\n[users]\nadmin = 8c6976e5b5410415\n",
        );
        assert!(matches!(f.users[0].password, Password::Opaque(_)));
        let f = parse_shiro("[users]\nadmin = $shiro1$SHA-256$500000$abc$def\n");
        assert!(matches!(f.users[0].password, Password::Opaque(_)));
        let f = parse_shiro(
            "[main]\nldapRealm = org.apache.shiro.realm.ldap.DefaultLdapRealm\nldapRealm.url = ldap://x\n",
        );
        assert_eq!(f.main_unsupported.len(), 1);
        let p =
            parse_passwd("# c\nuser1: pw1\nuser2: OBF:1v2j1uum1xtv1zej1zer1xtn1uvk1v1v, admin\n");
        assert!(matches!(p.users[0].password, Password::Plain(_)));
        assert!(matches!(p.users[1].password, Password::Opaque(_)));
        assert_eq!(p.users[1].roles, vec!["admin"]);
    }

    #[test]
    fn ant_patterns() {
        assert!(ant_match("/**", "/ds/sparql"));
        assert!(ant_match("/ds/**", "/ds"));
        assert!(ant_match("/ds/**", "/ds/q"));
        assert!(!ant_match("/ds", "/ds/q"));
        assert!(ant_match("/d?/*", "/ds/q"));
        assert!(!ant_match("/$/**", "/ds"));
    }
}
