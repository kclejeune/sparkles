//! Group ids: the class and the partitions that lead to a group, with the key the group
//! has in each, as `g1.` and the URL-safe base64 of that JSON. The encoding is
//! deterministic, so a group named by an answer is named by the same id in any later
//! answer that reaches it from another side. An id is decoded only through checks:
//! IRIs and terms are parsed again and printed in their canonical form, numbers must be
//! finite, and the depth and the sizes are bounded, so an id a client makes up can only
//! describe a set its own SPARQL queries could describe.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

/// The prefix and version of the encoding.
const PREFIX: &str = "g1.";
/// Partitions in one path at most.
pub(super) const MAX_DEPTH: usize = 8;
/// Bytes of an id at most.
pub(super) const MAX_ID: usize = 32 << 10;

/// One partition of a set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub(super) enum Level {
    /// by the most frequent values of a property; the rest is one group
    #[serde(rename = "v")]
    Values { p: String, vals: Vec<String> },
    /// by ranges of a numeric property, or of the years of a date property, of width `w`
    /// from `lo`, the last one open above
    #[serde(rename = "r")]
    Range {
        p: String,
        lo: f64,
        w: f64,
        n: u32,
        year: bool,
    },
    /// by the communities F14 wrote to `p`: the coarsest ones, or the children (by
    /// `skos:broader`) of community `parent`
    #[serde(rename = "c")]
    Community { p: String, parent: Option<String> },
}

/// A group's key in its partition.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(super) enum Key {
    /// the value at this index of [`Level::Values::vals`]
    #[serde(rename = "v")]
    Value(usize),
    /// the range of this number
    #[serde(rename = "b")]
    Bucket(u32),
    /// the community with this IRI
    #[serde(rename = "c")]
    Community(String),
    /// none of the partition's values, ranges or communities
    #[serde(rename = "o")]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct Step {
    pub level: Level,
    pub key: Key,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(super) struct GroupId {
    pub class: String,
    pub path: Vec<Step>,
}

impl GroupId {
    pub(super) fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!("{PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// The group an id names, checked.
    pub(super) fn decode(id: &str) -> Result<GroupId, String> {
        if id.len() > MAX_ID {
            return Err("the group id is too long".into());
        }
        let body = id
            .strip_prefix(PREFIX)
            .ok_or("not a group id of this server")?;
        let bytes = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| "the group id is malformed")?;
        let g: GroupId = serde_json::from_slice(&bytes).map_err(|_| "the group id is malformed")?;
        g.checked()
    }

    fn checked(self) -> Result<GroupId, String> {
        let iri = |s: &str| {
            oxrdf::NamedNode::new(s)
                .map(|n| n.into_string())
                .map_err(|_| "the group id holds an invalid IRI".to_string())
        };
        if self.path.is_empty() || self.path.len() > MAX_DEPTH {
            return Err("the group id has no partition or too many".into());
        }
        let mut path = Vec::with_capacity(self.path.len());
        for s in self.path {
            let level = match s.level {
                Level::Values { p, vals } => {
                    if vals.len() > super::groups::MAX_BUCKETS {
                        return Err("the group id holds too many values".into());
                    }
                    let vals = vals
                        .iter()
                        .map(|v| canonical_term(v))
                        .collect::<Result<Vec<_>, _>>()?;
                    if let Key::Value(i) = s.key
                        && i >= vals.len()
                    {
                        return Err("the group id names a value it does not hold".into());
                    }
                    Level::Values { p: iri(&p)?, vals }
                }
                Level::Range { p, lo, w, n, year } => {
                    if !lo.is_finite() || !w.is_finite() || w <= 0.0 || n == 0 {
                        return Err("the group id holds an invalid range".into());
                    }
                    if n as usize > super::groups::MAX_BUCKETS {
                        return Err("the group id holds too many ranges".into());
                    }
                    if let Key::Bucket(b) = s.key
                        && b >= n
                    {
                        return Err("the group id names a range it does not hold".into());
                    }
                    Level::Range {
                        p: iri(&p)?,
                        lo,
                        w,
                        n,
                        year,
                    }
                }
                Level::Community { p, parent } => Level::Community {
                    p: iri(&p)?,
                    parent: parent.as_deref().map(iri).transpose()?,
                },
            };
            let key = match (&level, s.key) {
                (Level::Values { .. }, k @ (Key::Value(_) | Key::Other)) => k,
                (Level::Range { .. }, k @ (Key::Bucket(_) | Key::Other)) => k,
                (Level::Community { .. }, Key::Community(c)) => Key::Community(iri(&c)?),
                (Level::Community { .. }, Key::Other) => Key::Other,
                _ => return Err("the group id pairs a partition with a key of another".into()),
            };
            path.push(Step { level, key });
        }
        Ok(GroupId {
            class: iri(&self.class)?,
            path,
        })
    }
}

/// A term in N-Triples syntax, parsed and printed again (an IRI or a literal).
pub(super) fn canonical_term(s: &str) -> Result<String, String> {
    match s.parse::<oxrdf::Term>() {
        Ok(t @ (oxrdf::Term::NamedNode(_) | oxrdf::Term::Literal(_))) => Ok(t.to_string()),
        _ => Err("the group id holds an invalid value".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> GroupId {
        GroupId {
            class: "http://ex/Person".into(),
            path: vec![
                Step {
                    level: Level::Values {
                        p: "http://ex/city".into(),
                        vals: vec!["\"Oslo\"".into(), "<http://ex/kyoto>".into()],
                    },
                    key: Key::Value(1),
                },
                Step {
                    level: Level::Range {
                        p: "http://ex/age".into(),
                        lo: 18.0,
                        w: 7.5,
                        n: 8,
                        year: false,
                    },
                    key: Key::Other,
                },
            ],
        }
    }

    #[test]
    fn ids_round_trip_and_are_deterministic() {
        let g = sample();
        let id = g.encode();
        assert!(id.starts_with("g1."));
        let back = GroupId::decode(&id).unwrap();
        assert_eq!(back, g);
        assert_eq!(back.encode(), id);
    }

    #[test]
    fn made_up_ids_are_refused() {
        for bad in ["", "g1.", "g1.!!", "x.e30", "g1.e30"] {
            assert!(GroupId::decode(bad).is_err(), "{bad}");
        }
        let enc = |j: serde_json::Value| format!("g1.{}", URL_SAFE_NO_PAD.encode(j.to_string()));
        // an IRI that would break out of the query text
        let mut g = serde_json::to_value(sample()).unwrap();
        g["class"] = "http://ex/a> } ; DROP ALL ; <x".into();
        assert!(GroupId::decode(&enc(g)).is_err());
        // a value that is not a term
        let mut g = serde_json::to_value(sample()).unwrap();
        g["path"][0]["level"]["vals"][0] = "\"x\" } DELETE".into();
        assert!(GroupId::decode(&enc(g)).is_err());
        // a key out of range, a bucket key on a value partition
        let mut g = serde_json::to_value(sample()).unwrap();
        g["path"][0]["key"] = serde_json::json!({ "v": 9 });
        assert!(GroupId::decode(&enc(g)).is_err());
        let mut g = serde_json::to_value(sample()).unwrap();
        g["path"][0]["key"] = serde_json::json!({ "b": 0 });
        assert!(GroupId::decode(&enc(g)).is_err());
        // a range that is not finite
        let mut g = serde_json::to_value(sample()).unwrap();
        g["path"][1]["level"]["w"] = 0.into();
        assert!(GroupId::decode(&enc(g)).is_err());
    }
}
