//! Who may use what (spec G08 §6): Shiro's URL rules and `fuseki:allowedUsers`,
//! evaluated per dataset, operation and principal, and written as Sparkles grants.

use super::convert::ServerPlan;
use super::endpoints;
use super::report::Report;
use super::users::{Filter, Password, UserFile};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// An endpoint of a service and who Fuseki lets use it.
#[derive(Clone, Debug)]
pub struct EndpointAccess {
    pub op: String,
    pub name: String,
    pub allowed: Option<Vec<String>>,
}

/// The access settings of one service.
#[derive(Clone, Debug)]
pub struct DatasetAccess {
    pub name: String,
    pub service_allowed: Option<Vec<String>>,
    pub endpoints: Vec<EndpointAccess>,
    /// graph access control: user → graphs
    pub graphs: Option<Vec<(String, Vec<String>)>>,
}

pub struct AccessInput<'a> {
    pub users: Option<(&'a Path, &'a UserFile)>,
    pub shiro: bool,
    pub server_allowed: Option<Vec<String>>,
    pub datasets: Vec<DatasetAccess>,
}

/// One limited grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub dataset: String,
    pub level: &'static str,
    pub endpoints: Option<Vec<String>>,
    pub graphs: Option<Vec<String>>,
}

/// The grants of a principal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub datasets: BTreeMap<String, &'static str>,
    pub grants: Vec<Grant>,
    pub server: Vec<&'static str>,
}

impl Grants {
    pub fn is_empty(&self) -> bool {
        self.datasets.is_empty() && self.grants.is_empty() && self.server.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct UserPlan {
    pub name: String,
    pub password: Password,
    /// where the password came from, for messages
    pub source: String,
    pub roles: Vec<String>,
    pub grants: Grants,
}

/// The auth configuration to write.
#[derive(Clone, Debug, Default)]
pub struct AuthPlan {
    pub anonymous: Grants,
    pub roles: BTreeMap<String, Grants>,
    pub users: Vec<UserPlan>,
}

/// A caller: the anonymous one or a named user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who<'a> {
    Anonymous,
    User(&'a str, &'a [String]),
}

/// `fuseki:allowedUsers` as Fuseki's `Auth.policyAllowSpecific` reads it.
fn policy_allows(list: Option<&Vec<String>>, who: Who) -> bool {
    let Some(list) = list else {
        return true;
    };
    if list.is_empty() || list.iter().any(|u| u == "!") {
        return false;
    }
    if list.iter().any(|u| u == "*") {
        return matches!(who, Who::User(..));
    }
    if list.iter().any(|u| u == "_") {
        return true;
    }
    match who {
        Who::Anonymous => false,
        Who::User(n, _) => list.iter().any(|u| u == n),
    }
}

/// The Shiro filters of a rule, for one caller.
fn filters_allow(filters: &[Filter], who: Who) -> bool {
    filters.iter().all(|f| match f {
        Filter::Anon | Filter::Neutral => true,
        Filter::Authenticated => matches!(who, Who::User(..)),
        Filter::Roles(rs) => match who {
            Who::User(_, roles) => rs.iter().all(|r| roles.contains(r)),
            Who::Anonymous => false,
        },
        Filter::Localhost | Filter::Other(_) => false,
    })
}

pub fn plan(input: &AccessInput, report: &mut Report, server: &mut ServerPlan) -> Option<AuthPlan> {
    let users: &[super::users::User] = input.users.map(|(_, u)| u.users.as_slice()).unwrap_or(&[]);
    let user_file = input.users.map(|(p, _)| super::graph::shown(p));
    let shiro = if input.shiro {
        input.users.map(|(_, u)| u)
    } else {
        None
    };
    let any_allowed = input.server_allowed.is_some()
        || input.datasets.iter().any(|d| {
            d.service_allowed.is_some() || d.endpoints.iter().any(|e| e.allowed.is_some())
        });
    let any_graphs = input.datasets.iter().any(|d| d.graphs.is_some());
    let any_rules = shiro.is_some_and(|s| !s.urls.is_empty());
    let read_only: Vec<bool> = input
        .datasets
        .iter()
        .map(|d| !d.endpoints.iter().any(|e| endpoints::is_write(&e.op)))
        .collect();
    let all_read_only = !read_only.is_empty() && read_only.iter().all(|r| *r);
    let some_read_only = read_only.iter().any(|r| *r);

    if let Some((p, f)) = input.users {
        for m in &f.main_unsupported {
            report.unsupported(
                super::graph::shown(p),
                format!(
                    "[main] {m}: Sparkles reads users from its own configuration; an external \
                     directory connects through OIDC or a forward-auth proxy"
                ),
            );
        }
    }

    if users.is_empty() && !any_allowed && !any_graphs && !any_rules {
        if all_read_only {
            server.read_only = true;
            report.converted("server", "every service is read-only: --read-only");
            return None;
        }
        if !some_read_only {
            return None;
        }
        // an open server with some read-only datasets: the anonymous caller's grants
        // follow each service's endpoints
    }
    if users.is_empty() && (any_allowed || any_graphs) {
        report.unsupported(
            "server",
            "the configuration limits access to named users, and no user file was found; pass \
             --shiro or --passwd, or users have to be added to auth.toml by hand",
        );
    }

    // who may use each operation of each dataset
    let mut who_all: Vec<Who> = vec![Who::Anonymous];
    who_all.extend(users.iter().map(|u| Who::User(&u.name, &u.roles)));
    let mut plan = AuthPlan::default();
    let mut per_user: Vec<Grants> = vec![Grants::default(); users.len()];
    let mut localhost_noted = BTreeSet::new();
    for d in &input.datasets {
        for (wi, who) in who_all.iter().enumerate() {
            let mut ops: BTreeSet<String> = BTreeSet::new();
            for e in &d.endpoints {
                if e.op == "no-op" {
                    continue;
                }
                let path = if e.name.is_empty() {
                    format!("/{}", d.name)
                } else {
                    format!("/{}/{}", d.name, e.name)
                };
                let shiro_ok = match shiro.and_then(|s| s.rule_for(&path)) {
                    Some(rule) => {
                        if rule
                            .filters
                            .iter()
                            .any(|f| matches!(f, Filter::Localhost | Filter::Other(_)))
                            && localhost_noted.insert(rule.pattern.clone())
                        {
                            let what: Vec<String> = rule
                                .filters
                                .iter()
                                .map(|f| match f {
                                    Filter::Localhost => "localhostFilter".to_string(),
                                    Filter::Other(o) => o.clone(),
                                    other => format!("{other:?}"),
                                })
                                .collect();
                            report.approximated(
                                user_file.clone().unwrap_or_default(),
                                format!(
                                    "[urls] {} = {}: Sparkles cannot test this filter, so it \
                                     grants nobody access through it",
                                    rule.pattern,
                                    what.join(", ")
                                ),
                            );
                        }
                        filters_allow(&rule.filters, *who)
                    }
                    None => true,
                };
                if shiro_ok
                    && policy_allows(input.server_allowed.as_ref(), *who)
                    && policy_allows(d.service_allowed.as_ref(), *who)
                    && policy_allows(e.allowed.as_ref(), *who)
                {
                    ops.insert(endpoints::canonical(&e.op).to_string());
                }
            }
            // graph access control limits what each user reads
            let graphs = match (&d.graphs, who) {
                (None, _) => None,
                (Some(reg), Who::User(n, _)) => match reg.iter().find(|(u, _)| u == n) {
                    Some((_, gs)) => Some(gs.clone()),
                    None => continue,
                },
                (Some(_), Who::Anonymous) => continue,
            };
            if ops.is_empty() {
                continue;
            }
            let grants = if wi == 0 {
                &mut plan.anonymous
            } else {
                &mut per_user[wi - 1]
            };
            add_grant(grants, &d.name, &ops, graphs);
        }
    }

    // administration: Shiro's rule for /$/
    let mut admin_note = None;
    if let Some(s) = shiro {
        match s.rule_for("/$/datasets") {
            Some(rule) => {
                let roles: Vec<&String> = rule
                    .filters
                    .iter()
                    .filter_map(|f| match f {
                        Filter::Roles(rs) => Some(rs),
                        _ => None,
                    })
                    .flatten()
                    .collect();
                if rule.filters.iter().any(|f| matches!(f, Filter::Localhost)) {
                    admin_note = Some(format!(
                        "[urls] {} = localhostFilter: Sparkles cannot tell local callers apart \
                         once authentication is on, so no one administers the server; give a \
                         user server = [\"server-admin\"] in auth.toml",
                        rule.pattern
                    ));
                } else if !roles.is_empty() {
                    for r in &roles {
                        plan.roles.entry((*r).clone()).or_default().server = vec!["server-admin"];
                    }
                    report.converted(
                        user_file.clone().unwrap_or_default(),
                        format!(
                            "[urls] {}: the role{} {} administer{} the server (server-admin)",
                            rule.pattern,
                            if roles.len() == 1 { "" } else { "s" },
                            roles
                                .iter()
                                .map(|r| r.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            if roles.len() == 1 { "s" } else { "" }
                        ),
                    );
                } else if rule
                    .filters
                    .iter()
                    .any(|f| matches!(f, Filter::Authenticated))
                {
                    for g in per_user.iter_mut() {
                        g.server = vec!["server-admin"];
                    }
                    report.converted(
                        user_file.clone().unwrap_or_default(),
                        format!(
                            "[urls] {}: every user administers the server, as in Fuseki",
                            rule.pattern
                        ),
                    );
                } else if rule
                    .filters
                    .iter()
                    .all(|f| matches!(f, Filter::Anon | Filter::Neutral))
                {
                    admin_note = Some(format!(
                        "[urls] {} = anon lets everyone administer Fuseki; Sparkles grants that \
                         to no one, so give a user server = [\"server-admin\"] in auth.toml",
                        rule.pattern
                    ));
                }
            }
            None => {
                admin_note = Some(
                    "no [urls] rule covers /$/; no one administers the server, so give a user \
                     server = [\"server-admin\"] in auth.toml"
                        .into(),
                );
            }
        }
    } else {
        admin_note = Some(
            "no user administers the server; give one server = [\"server-admin\"] in auth.toml"
                .into(),
        );
    }
    if let Some(n) = admin_note {
        report.approximated(user_file.clone().unwrap_or_else(|| "server".into()), n);
    }

    // users: names, passwords and roles
    for (u, grants) in users.iter().zip(per_user) {
        let place = user_file.clone().unwrap_or_default();
        if !valid_user_name(&u.name) {
            report.unsupported(
                &place,
                format!(
                    "user \"{}\": Sparkles user names have letters, digits, '_', '.', '@' and \
                     '-' only",
                    u.name
                ),
            );
            continue;
        }
        let roles: Vec<String> = u
            .roles
            .iter()
            .filter(|r| plan.roles.contains_key(*r))
            .cloned()
            .collect();
        match &u.password {
            Password::Plain(_) => report.converted(
                &place,
                format!("user {}: the password, hashed with argon2id", u.name),
            ),
            Password::Opaque(kind) => report.unsupported(
                &place,
                format!(
                    "user {}: the password is {kind}, which Sparkles cannot read; auth.toml has \
                     the user commented out until a hash from `sparkles auth hash` is added",
                    u.name
                ),
            ),
        }
        plan.users.push(UserPlan {
            name: u.name.clone(),
            password: u.password.clone(),
            source: place,
            roles,
            grants,
        });
    }
    // users that access rules name but no file defines
    let known: BTreeSet<&str> = users.iter().map(|u| u.name.as_str()).collect();
    let mut named: BTreeSet<String> = BTreeSet::new();
    let lists = input
        .server_allowed
        .iter()
        .chain(
            input
                .datasets
                .iter()
                .filter_map(|d| d.service_allowed.as_ref()),
        )
        .chain(
            input
                .datasets
                .iter()
                .flat_map(|d| d.endpoints.iter().filter_map(|e| e.allowed.as_ref())),
        );
    for l in lists {
        named.extend(
            l.iter()
                .filter(|u| !matches!(u.as_str(), "*" | "_" | "!" | ""))
                .cloned(),
        );
    }
    for d in &input.datasets {
        if let Some(reg) = &d.graphs {
            named.extend(reg.iter().map(|(u, _)| u.clone()));
        }
    }
    for n in named.iter().filter(|n| !known.contains(n.as_str())) {
        if !users.is_empty() {
            report.ignored(
                "server",
                format!(
                    "user \"{n}\" is named in access rules but has no password in the user file"
                ),
            );
        }
    }
    let total_grants = !plan.anonymous.is_empty()
        || plan.users.iter().any(|u| !u.grants.is_empty())
        || !plan.roles.is_empty();
    if total_grants || !plan.users.is_empty() {
        let summary = summary(&plan);
        report.converted("server", format!("auth.toml: {summary}"));
        Some(plan)
    } else {
        None
    }
}

/// A Sparkles principal name: `[A-Za-z0-9_.@-]{1,64}`.
fn valid_user_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// Add the grant for `ops` on `ds` (spec G08 §6.2).
fn add_grant(g: &mut Grants, ds: &str, ops: &BTreeSet<String>, graphs: Option<Vec<String>>) {
    let write = ops.iter().any(|o| endpoints::is_write(o));
    let level = if write { "write" } else { "read" };
    let full = ops.contains("query") && (!write || ops.contains("update"));
    let endpoints = (!full).then(|| {
        let mut v: Vec<String> = ops
            .iter()
            .filter_map(|o| endpoints::grant_endpoint(o))
            .map(String::from)
            .collect();
        v.sort();
        v.dedup();
        v
    });
    // graph grants are read grants (Fuseki applies them to read-only services)
    let level = if graphs.is_some() { "read" } else { level };
    if endpoints.is_none() && graphs.is_none() {
        g.datasets.insert(ds.to_string(), level);
    } else {
        g.grants.push(Grant {
            dataset: ds.to_string(),
            level,
            endpoints,
            graphs,
        });
    }
}

fn summary(plan: &AuthPlan) -> String {
    let mut parts = Vec::new();
    let n = plan.users.len();
    parts.push(format!("{n} user{}", if n == 1 { "" } else { "s" }));
    if !plan.roles.is_empty() {
        parts.push(format!(
            "role{} {}",
            if plan.roles.len() == 1 { "" } else { "s" },
            plan.roles.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    if !plan.anonymous.is_empty() {
        let ds: Vec<String> = plan
            .anonymous
            .datasets
            .iter()
            .map(|(d, l)| format!("{d} ({l})"))
            .chain(plan.anonymous.grants.iter().map(|g| {
                format!(
                    "{} ({}{})",
                    g.dataset,
                    g.level,
                    g.endpoints
                        .as_ref()
                        .map(|e| format!(": {}", e.join(" ")))
                        .unwrap_or_default()
                )
            }))
            .collect();
        parts.push(format!("anonymous access to {}", ds.join(", ")));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowed_users_semantics() {
        let roles: Vec<String> = Vec::new();
        let u1 = Who::User("user1", &roles);
        assert!(policy_allows(None, Who::Anonymous));
        assert!(!policy_allows(Some(&vec![]), u1));
        assert!(!policy_allows(Some(&vec!["!".into()]), u1));
        assert!(policy_allows(Some(&vec!["*".into()]), u1));
        assert!(!policy_allows(Some(&vec!["*".into()]), Who::Anonymous));
        assert!(policy_allows(Some(&vec!["_".into()]), Who::Anonymous));
        assert!(policy_allows(
            Some(&vec!["user1".into(), "user2".into()]),
            u1
        ));
        assert!(!policy_allows(Some(&vec!["user2".into()]), u1));
    }

    #[test]
    fn grants_follow_the_operations() {
        let mut g = Grants::default();
        let ops = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        add_grant(&mut g, "a", &ops(&["query", "gsp-r"]), None);
        add_grant(&mut g, "b", &ops(&["query", "update", "gsp-rw"]), None);
        add_grant(&mut g, "c", &ops(&["gsp-r"]), None);
        add_grant(&mut g, "d", &ops(&["query", "gsp-rw"]), None);
        add_grant(&mut g, "e", &ops(&["query"]), Some(vec!["http://g".into()]));
        assert_eq!(g.datasets.get("a"), Some(&"read"));
        assert_eq!(g.datasets.get("b"), Some(&"write"));
        assert_eq!(
            g.grants[0],
            Grant {
                dataset: "c".into(),
                level: "read",
                endpoints: Some(vec!["gsp-r".into()]),
                graphs: None
            }
        );
        // writes through the Graph Store only
        assert_eq!(
            g.grants[1].endpoints.as_deref(),
            Some(&["gsp-rw".to_string(), "query".to_string()][..])
        );
        assert_eq!(g.grants[1].level, "write");
        assert_eq!(
            g.grants[2].graphs.as_deref(),
            Some(&["http://g".to_string()][..])
        );
        assert_eq!(g.grants[2].endpoints, None);
    }
}
