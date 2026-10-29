//! The fixed comparison frame. This does not derive or authenticate graph decisions.
use super::model::*;
use oxrdf::NamedNode;
use sparkles::task::Control;
use std::collections::{BTreeMap, BTreeSet};

type Result<T> = std::result::Result<T, Failure>;

fn check(control: &Control) -> Result<()> {
    control.check().map_err(|_| Failure::ReplayCancelled)
}
fn iri(value: &str) -> Result<()> {
    NamedNode::new(value)
        .map(|_| ())
        .map_err(|_| Failure::InvalidDomainRecord)
}
fn string_set(values: &mut Vec<String>) {
    values.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    values.dedup();
}

fn normalize(comparison: &Comparison, control: &Control) -> Result<Comparison> {
    check(control)?;
    if comparison.profile_version != 1 {
        return Err(Failure::UnsupportedProfile);
    }
    iri(&comparison.scope)?;
    let mut input_ids = comparison.input_ids.clone();
    string_set(&mut input_ids);
    let inputs: BTreeSet<_> = input_ids.iter().map(String::as_str).collect();
    for id in &input_ids {
        check(control)?;
        iri(id)?;
    }
    let reference = |id: &str| -> Result<()> {
        iri(id)?;
        if inputs.contains(id) {
            Ok(())
        } else {
            Err(Failure::UnresolvedReference)
        }
    };
    let mut assignments = BTreeMap::new();
    for original in &comparison.assignments {
        check(control)?;
        reference(&original.entity)?;
        let mut row = original.clone();
        string_set(&mut row.decisions);
        for decision in &row.decisions {
            reference(decision)?;
        }
        match (row.status, &row.representative) {
            (Status::Resolved, Some(id)) => reference(id)?,
            (Status::Unresolved, None) => {}
            _ => return Err(Failure::InvalidDomainRecord),
        }
        if let Some(previous) = assignments.get(&row.entity) {
            if previous != &row {
                return Err(Failure::InvalidDomainRecord);
            }
        } else {
            assignments.insert(row.entity.clone(), row);
        }
    }
    for row in assignments.values() {
        if let Some(id) = &row.representative
            && !assignments.contains_key(id)
        {
            return Err(Failure::UnresolvedReference);
        }
    }
    let mut conflicts: BTreeMap<(Vec<String>, ConflictCode), BTreeSet<String>> = BTreeMap::new();
    for original in &comparison.conflicts {
        check(control)?;
        let mut entities = original.entities.clone();
        string_set(&mut entities);
        if entities.is_empty() {
            return Err(Failure::InvalidDomainRecord);
        }
        for entity in &entities {
            if assignments
                .get(entity)
                .is_none_or(|row| row.status != Status::Unresolved)
            {
                return Err(Failure::InvalidDomainRecord);
            }
        }
        let decisions = conflicts.entry((entities, original.code)).or_default();
        for decision in &original.decisions {
            reference(decision)?;
            decisions.insert(decision.clone());
        }
    }
    let mut entity_component: BTreeMap<&str, &[String]> = BTreeMap::new();
    let mut conflict_decisions: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for ((entities, _), decisions) in &conflicts {
        check(control)?;
        for entity in entities {
            if let Some(previous) = entity_component.insert(entity, entities)
                && previous != entities
            {
                return Err(Failure::InvalidDomainRecord);
            }
            conflict_decisions
                .entry(entity)
                .or_default()
                .extend(decisions.iter().map(String::as_str));
        }
    }
    for row in assignments.values() {
        check(control)?;
        if row.status == Status::Unresolved {
            let expected = conflict_decisions
                .get(row.entity.as_str())
                .ok_or(Failure::InvalidDomainRecord)?;
            if row
                .decisions
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != *expected
            {
                return Err(Failure::InvalidDomainRecord);
            }
        }
    }
    let mut unknowns = BTreeMap::new();
    for row in &comparison.unknowns {
        check(control)?;
        reference(&row.record)?;
        unknowns.insert((row.record.clone(), row.code), row.clone());
    }
    Ok(Comparison {
        profile_version: 1,
        scope: comparison.scope.clone(),
        input_ids,
        assignments: assignments.into_values().collect(),
        conflicts: conflicts
            .into_iter()
            .map(|((entities, code), decisions)| Conflict {
                code,
                entities,
                decisions: decisions.into_iter().collect(),
            })
            .collect(),
        unknowns: unknowns.into_values().collect(),
    })
}

struct Writer<'a> {
    bytes: Vec<u8>,
    limit: usize,
    control: &'a Control,
}
impl<'a> Writer<'a> {
    fn new(limit: OutputLimit, control: &'a Control) -> Result<Self> {
        check(control)?;
        if limit.0 > MAX_OUTPUT_BYTES {
            return Err(Failure::ReplayLimit);
        }
        Ok(Self {
            bytes: Vec::new(),
            limit: limit.0,
            control,
        })
    }
    fn raw(&mut self, bytes: &[u8]) -> Result<()> {
        check(self.control)?;
        let next = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .ok_or(Failure::ReplayLimit)?;
        if next > self.limit {
            return Err(Failure::ReplayLimit);
        }
        if self.bytes.capacity() < next {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(4096)
                .min(self.limit)
                .max(next);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| Failure::ReplayLimit)?;
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    fn scalar(&mut self, value: &str) -> Result<()> {
        self.raw(b"\"")?;
        for c in value.chars() {
            match c {
                '"' => self.raw(b"\\\"")?,
                '\\' => self.raw(b"\\\\")?,
                '\0'..='\u{1f}' => {
                    let hex = b"0123456789abcdef";
                    self.raw(&[
                        b'\\',
                        b'u',
                        b'0',
                        b'0',
                        hex[(c as usize) >> 4],
                        hex[(c as usize) & 15],
                    ])?;
                }
                other => {
                    let mut buffer = [0; 4];
                    self.raw(other.encode_utf8(&mut buffer).as_bytes())?;
                }
            }
        }
        self.raw(b"\"")
    }
    fn strings(&mut self, values: &[String]) -> Result<()> {
        self.raw(b"[")?;
        for (index, value) in values.iter().enumerate() {
            if index != 0 {
                self.raw(b",")?;
            }
            self.scalar(value)?;
        }
        self.raw(b"]")
    }
}

pub(crate) fn encode_scalar(value: &str, limit: OutputLimit, control: &Control) -> Result<Vec<u8>> {
    let mut writer = Writer::new(limit, control)?;
    writer.scalar(value)?;
    Ok(writer.bytes)
}

/// Encode a caller-verified comparison without writing a file or current pointer.
/// Local consistency checks cannot establish component completeness or authorization.
pub(crate) fn encode_comparison(
    comparison: &Comparison,
    limit: OutputLimit,
    control: &Control,
) -> Result<Vec<u8>> {
    check(control)?;
    if limit.0 > MAX_OUTPUT_BYTES {
        return Err(Failure::ReplayLimit);
    }
    let comparison = normalize(comparison, control)?;
    let mut writer = Writer::new(limit, control)?;
    writer.raw(b"{\"profileVersion\":1,\"scope\":")?;
    writer.scalar(&comparison.scope)?;
    writer.raw(b",\"inputIds\":")?;
    writer.strings(&comparison.input_ids)?;
    writer.raw(b",\"assignments\":[")?;
    for (index, row) in comparison.assignments.iter().enumerate() {
        if index != 0 {
            writer.raw(b",")?;
        }
        writer.raw(b"{\"entity\":")?;
        writer.scalar(&row.entity)?;
        writer.raw(b",\"status\":")?;
        writer.scalar(row.status.as_str())?;
        writer.raw(b",\"representative\":")?;
        if let Some(id) = &row.representative {
            writer.scalar(id)?;
        } else {
            writer.raw(b"null")?;
        }
        writer.raw(b",\"decisions\":")?;
        writer.strings(&row.decisions)?;
        writer.raw(b"}")?;
    }
    writer.raw(b"],\"conflicts\":[")?;
    for (index, row) in comparison.conflicts.iter().enumerate() {
        if index != 0 {
            writer.raw(b",")?;
        }
        writer.raw(b"{\"code\":")?;
        writer.scalar(row.code.as_str())?;
        writer.raw(b",\"entities\":")?;
        writer.strings(&row.entities)?;
        writer.raw(b",\"decisions\":")?;
        writer.strings(&row.decisions)?;
        writer.raw(b"}")?;
    }
    writer.raw(b"],\"unknowns\":[")?;
    for (index, row) in comparison.unknowns.iter().enumerate() {
        if index != 0 {
            writer.raw(b",")?;
        }
        writer.raw(b"{\"record\":")?;
        writer.scalar(&row.record)?;
        writer.raw(b",\"code\":")?;
        writer.scalar(row.code.as_str())?;
        writer.raw(b"}")?;
    }
    writer.raw(b"]}")?;
    Ok(writer.bytes)
}
