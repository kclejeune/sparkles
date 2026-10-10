//! JSON Merge Patch (RFC 7396) and the field paths of spec C19 §4.
//!
//! A field is a path of object members. Arrays and scalars are leaves, and so is an
//! empty object. Paths are written with dots, and a member name that holds a dot or a
//! backslash escapes it with a backslash, so `sendByProvider.local\.llm` names the
//! member `local.llm` of `sendByProvider`.

use serde_json::{Map, Value};

/// Apply `patch` to `target` as RFC 7396 says: objects merge member by member, `null`
/// removes a member, and anything else replaces the earlier value.
pub fn merge(target: &mut Value, patch: &Value) {
    let Value::Object(p) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(Map::new());
    }
    let t = target.as_object_mut().expect("an object");
    for (k, v) in p {
        if v.is_null() {
            t.remove(k);
        } else {
            merge(t.entry(k.clone()).or_insert(Value::Null), v);
        }
    }
}

/// `base` with `patch` merged in.
pub fn merged(base: &Value, patch: &Value) -> Value {
    let mut v = base.clone();
    merge(&mut v, patch);
    v
}

/// Two merge patches combined into one that applies both, `top` after `under`. Unlike
/// [`merged`] it keeps a `null` of `top`, which still removes that member.
pub fn overlaid(under: &Value, top: &Value) -> Value {
    match (under, top) {
        (Value::Object(u), Value::Object(t)) => {
            let mut out = u.clone();
            for (k, tv) in t {
                let v = match out.get(k) {
                    Some(uv) => overlaid(uv, tv),
                    None => tv.clone(),
                };
                out.insert(k.clone(), v);
            }
            Value::Object(out)
        }
        _ => top.clone(),
    }
}

/// The merge patch that turns `base` into `target`, or `None` when they are equal. A
/// member of `base` that `target` lacks becomes `null`.
pub fn diff(base: &Value, target: &Value) -> Option<Value> {
    match (base, target) {
        (Value::Object(b), Value::Object(t)) => {
            let mut out = Map::new();
            for (k, tv) in t {
                match b.get(k) {
                    Some(bv) => {
                        if let Some(d) = diff(bv, tv) {
                            out.insert(k.clone(), d);
                        }
                    }
                    None => {
                        out.insert(k.clone(), tv.clone());
                    }
                }
            }
            for k in b.keys() {
                if !t.contains_key(k) {
                    out.insert(k.clone(), Value::Null);
                }
            }
            (!out.is_empty()).then_some(Value::Object(out))
        }
        _ if base == target => None,
        _ => Some(target.clone()),
    }
}

/// Remove the empty objects of a layer, which merge as nothing.
pub fn prune(v: &mut Value) {
    if let Value::Object(m) = v {
        for x in m.values_mut() {
            prune(x);
        }
        m.retain(|_, x| !matches!(x, Value::Object(o) if o.is_empty()));
    }
}

/// The value at `path`, if every member on the way exists.
pub fn at<'a>(v: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(v, |v, k| v.as_object()?.get(k))
}

/// Set the value at `path`, making the objects on the way.
pub fn set_at(v: &mut Value, path: &[String], x: Value) {
    let Some((last, init)) = path.split_last() else {
        *v = x;
        return;
    };
    let mut cur = v;
    for k in init {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        cur = cur
            .as_object_mut()
            .expect("an object")
            .entry(k.clone())
            .or_insert(Value::Null);
    }
    if !cur.is_object() {
        *cur = Value::Object(Map::new());
    }
    cur.as_object_mut()
        .expect("an object")
        .insert(last.clone(), x);
}

/// Remove the value at `path`, and the objects on the way that it leaves empty. `true`
/// when there was one.
pub fn remove_at(v: &mut Value, path: &[String]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    let Some(m) = v.as_object_mut() else {
        return false;
    };
    if rest.is_empty() {
        return m.remove(first).is_some();
    }
    let Some(child) = m.get_mut(first) else {
        return false;
    };
    let removed = remove_at(child, rest);
    if removed && matches!(child, Value::Object(o) if o.is_empty()) {
        m.remove(first);
    }
    removed
}

/// The leaves of `v`: the paths of its scalars, arrays and empty objects.
pub fn leaves(v: &Value) -> Vec<Vec<String>> {
    fn walk(v: &Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
        match v {
            Value::Object(m) if !m.is_empty() => {
                for (k, x) in m {
                    path.push(k.clone());
                    walk(x, path, out);
                    path.pop();
                }
            }
            _ if !path.is_empty() => out.push(path.clone()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, &mut Vec::new(), &mut out);
    out
}

/// Whether `prefix` is `path` or a path above it.
pub fn starts_with(path: &[String], prefix: &[String]) -> bool {
    path.len() >= prefix.len() && path[..prefix.len()] == *prefix
}

/// A dotted path as its members (`None` for an empty member or a dangling backslash).
pub fn parse_path(s: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => cur.push(chars.next()?),
            '.' => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    (!out.iter().any(String::is_empty)).then_some(out)
}

/// The dotted form of a path.
pub fn path_string(path: &[String]) -> String {
    path.iter()
        .map(|k| k.replace('\\', "\\\\").replace('.', "\\."))
        .collect::<Vec<_>>()
        .join(".")
}

/// The first member named `endpoint` or `apiKey`, anywhere in `v`.
pub fn forbidden_member(v: &Value) -> Option<&'static str> {
    const FORBIDDEN: &[&str] = &["endpoint", "apiKey"];
    match v {
        Value::Object(m) => m.iter().find_map(|(k, v)| {
            FORBIDDEN
                .iter()
                .find(|f| **f == k)
                .copied()
                .or_else(|| forbidden_member(v))
        }),
        Value::Array(a) => a.iter().find_map(forbidden_member),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(s: &str) -> Vec<String> {
        parse_path(s).unwrap()
    }

    #[test]
    fn merge_follows_rfc_7396() {
        // the examples of RFC 7396 appendix A
        let cases = [
            (json!({"a": "b"}), json!({"a": "c"}), json!({"a": "c"})),
            (
                json!({"a": "b"}),
                json!({"b": "c"}),
                json!({"a": "b", "b": "c"}),
            ),
            (json!({"a": "b"}), json!({"a": null}), json!({})),
            (
                json!({"a": "b", "b": "c"}),
                json!({"a": null}),
                json!({"b": "c"}),
            ),
            (json!({"a": ["b"]}), json!({"a": "c"}), json!({"a": "c"})),
            (json!({"a": "c"}), json!({"a": ["b"]}), json!({"a": ["b"]})),
            (
                json!({"a": {"b": "c"}}),
                json!({"a": {"b": "d", "c": null}}),
                json!({"a": {"b": "d"}}),
            ),
            (
                json!({"a": [{"b": "c"}]}),
                json!({"a": [1]}),
                json!({"a": [1]}),
            ),
            (json!(["a", "b"]), json!(["c", "d"]), json!(["c", "d"])),
            (json!({"a": "b"}), json!(["c"]), json!(["c"])),
            (json!({"a": "foo"}), json!(null), json!(null)),
            (json!({"a": "foo"}), json!("bar"), json!("bar")),
            (
                json!({"e": null}),
                json!({"a": 1}),
                json!({"e": null, "a": 1}),
            ),
            (
                json!([1, 2]),
                json!({"a": "b", "c": null}),
                json!({"a": "b"}),
            ),
            (
                json!({}),
                json!({"a": {"bb": {"ccc": null}}}),
                json!({"a": {"bb": {}}}),
            ),
        ];
        for (target, patch, want) in cases {
            assert_eq!(merged(&target, &patch), want, "{target} + {patch}");
        }
    }

    #[test]
    fn diff_is_the_inverse_of_merge() {
        let cases = [
            (
                json!({"a": 1, "b": {"c": 2}}),
                json!({"a": 1, "b": {"c": 3}}),
            ),
            (json!({"a": 1, "b": {"c": 2}}), json!({"b": {}})),
            (json!({"a": [1, 2]}), json!({"a": [1]})),
            (json!({"a": {"x": 1}}), json!({"a": 5})),
            (json!({"a": 5}), json!({"a": {"x": 1}})),
            (json!({}), json!({"n": {"m": true}})),
        ];
        for (base, target) in cases {
            let d = diff(&base, &target).unwrap();
            assert_eq!(merged(&base, &d), target, "{base} → {target} by {d}");
        }
        assert_eq!(diff(&json!({"a": [1]}), &json!({"a": [1]})), None);
        assert_eq!(
            diff(&json!({"a": 1, "b": 2}), &json!({"a": 1})),
            Some(json!({"b": null}))
        );
    }

    #[test]
    fn paths() {
        assert_eq!(p("budget.perRequest"), ["budget", "perRequest"]);
        assert_eq!(
            p(r"sendByProvider.local\.llm"),
            ["sendByProvider", "local.llm"]
        );
        assert_eq!(path_string(&p(r"a\\b.c\.d")), r"a\\b.c\.d");
        assert_eq!(parse_path(""), None);
        assert_eq!(parse_path("a..b"), None);
        assert_eq!(parse_path("a\\"), None);
        assert!(starts_with(&p("a.b.c"), &p("a.b")));
        assert!(starts_with(&p("a.b"), &p("a.b")));
        assert!(!starts_with(&p("a.bc"), &p("a.b")));
        assert!(!starts_with(&p("a"), &p("a.b")));
    }

    #[test]
    fn overlaid_keeps_nulls() {
        let u = json!({"a": 1, "b": {"c": 2, "d": 3}});
        let t = json!({"a": null, "b": {"c": null, "e": 4}, "f": {"g": null}});
        assert_eq!(
            overlaid(&u, &t),
            json!({"a": null, "b": {"c": null, "d": 3, "e": 4}, "f": {"g": null}})
        );
        assert_eq!(overlaid(&Value::Null, &t), t);
    }

    #[test]
    fn leaves_set_and_remove() {
        let v = json!({"a": 1, "b": {"c": [1], "d": {}}, "e": {"f": null}});
        assert_eq!(leaves(&v), vec![p("a"), p("b.c"), p("b.d"), p("e.f")]);
        let mut v = json!({"a": {"b": 1}});
        set_at(&mut v, &p("a.c.d"), json!(2));
        set_at(&mut v, &p("x"), json!(3));
        assert_eq!(v, json!({"a": {"b": 1, "c": {"d": 2}}, "x": 3}));
        assert!(remove_at(&mut v, &p("a.c.d")));
        assert_eq!(v, json!({"a": {"b": 1}, "x": 3}));
        assert!(!remove_at(&mut v, &p("a.zz")));
        assert!(remove_at(&mut v, &p("a.b")));
        assert_eq!(v, json!({"x": 3}));
        assert_eq!(at(&v, &p("x")), Some(&json!(3)));
        assert_eq!(at(&v, &p("x.y")), None);
        let mut v = json!({"a": {}, "b": {"c": {}}, "d": null});
        prune(&mut v);
        assert_eq!(v, json!({"d": null}));
    }

    #[test]
    fn forbidden_members_anywhere() {
        assert_eq!(
            forbidden_member(&json!({"a": [{"apiKey": 1}]})),
            Some("apiKey")
        );
        assert_eq!(
            forbidden_member(&json!({"x": {"endpoint": "u"}})),
            Some("endpoint")
        );
        assert_eq!(forbidden_member(&json!({"roles": {}})), None);
    }
}
