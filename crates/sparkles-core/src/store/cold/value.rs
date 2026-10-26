//! Controlled consumption of already admitted JSON values. Metadata retains the
//! ordinary last-duplicate-key-wins behavior without an uncontrolled second parser.
//! Callers hold the original JSON admission and its parser-enforced nesting bound.
//! This private reader serves closed metadata schemas with string map keys; it does
//! not expose a general replacement for every custom Serde deserializer. Standard
//! deallocation during refusal is distinct from cooperative producer traversal.
use super::ColdBudget;
use crate::{Error, Result};
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess,
    Visitor,
};
use std::cell::{Cell, RefCell};

/// Diagnostics deliberately omit input values. Serde's default invalid-type error
/// can otherwise copy and format an arbitrarily large malformed string.
#[derive(Debug)]
struct DecodeError;
impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid historical metadata field")
    }
}
impl std::error::Error for DecodeError {}
impl de::Error for DecodeError {
    fn custom<T: std::fmt::Display>(_: T) -> Self {
        Self
    }
    fn invalid_type(_: de::Unexpected<'_>, _: &dyn de::Expected) -> Self {
        Self
    }
    fn invalid_value(_: de::Unexpected<'_>, _: &dyn de::Expected) -> Self {
        Self
    }
    fn invalid_length(_: usize, _: &dyn de::Expected) -> Self {
        Self
    }
    fn unknown_variant(_: &str, _: &'static [&'static str]) -> Self {
        Self
    }
    fn unknown_field(_: &str, _: &'static [&'static str]) -> Self {
        Self
    }
    fn missing_field(_: &'static str) -> Self {
        Self
    }
    fn duplicate_field(_: &'static str) -> Self {
        Self
    }
}

struct State<'a, 'b> {
    budget: &'a ColdBudget<'b>,
    count: Cell<usize>,
    failure: RefCell<Option<Error>>,
}
impl State<'_, '_> {
    fn check(&self) -> std::result::Result<(), DecodeError> {
        let count = self.count.get();
        self.count.set(count.saturating_add(1));
        if let Err(error) = self.budget.key_step("metadata value decode", count) {
            *self.failure.borrow_mut() = Some(error);
            return Err(DecodeError);
        }
        Ok(())
    }
    fn discard(&self, value: serde_json::Value) -> std::result::Result<(), DecodeError> {
        self.check()?;
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    self.discard(value)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (_, value) in values {
                    self.discard(value)?;
                }
            }
            _ => {}
        }
        self.check()
    }
}

pub(super) fn from_value<T: DeserializeOwned>(
    budget: &ColdBudget<'_>,
    value: serde_json::Value,
) -> Result<T> {
    let state = State {
        budget,
        count: Cell::new(0),
        failure: RefCell::new(None),
    };
    let result = T::deserialize(ValueReader {
        value,
        state: &state,
    });
    if let Some(error) = state.failure.into_inner() {
        return Err(error);
    }
    result.map_err(|_| Error::Corrupt("invalid historical metadata field".into()))
}

/// Only declared fields reach the typed schema's flatten pass. Unknown extension
/// values remain valid input but are discarded cooperatively for this scalar read.
/// The same declaration must generate the schema and this member list.
pub(super) fn from_fields<T: DeserializeOwned>(
    budget: &ColdBudget<'_>,
    value: serde_json::Value,
    fields: &[&str],
) -> Result<T> {
    let state = State {
        budget,
        count: Cell::new(0),
        failure: RefCell::new(None),
    };
    let projected = (|| {
        state.check()?;
        let serde_json::Value::Object(values) = value else {
            return Err(DecodeError);
        };
        let mut selected = serde_json::Map::new();
        for (key, value) in values {
            state.check()?;
            if fields.contains(&key.as_str()) {
                selected.insert(key, value);
            } else {
                state.discard(value)?;
            }
        }
        state.check()?;
        Ok(serde_json::Value::Object(selected))
    })();
    if let Some(error) = state.failure.into_inner() {
        return Err(error);
    }
    from_value(
        budget,
        projected.map_err(|_| Error::Corrupt("invalid historical metadata object".into()))?,
    )
}

struct ValueReader<'a, 'b, 'c> {
    value: serde_json::Value,
    state: &'a State<'b, 'c>,
}

macro_rules! typed_value {
    ($($method:ident $(($($arg:ident: $ty:ty),*))? => $kind:pat),* $(,)?) => {
        $(fn $method<V: Visitor<'de>>(self, $($($arg: $ty,)*)? visitor: V) -> std::result::Result<V::Value, DecodeError> {
            self.state.check()?;
            if !matches!(&self.value, $kind) { return Err(DecodeError); }
            self.deserialize_any(visitor)
        })*
    };
}

impl<'de> de::Deserializer<'de> for ValueReader<'_, '_, '_> {
    type Error = DecodeError;
    fn deserialize_any<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        use serde_json::Value;
        match self.value {
            Value::Null => visitor.visit_unit(),
            Value::Bool(value) => visitor.visit_bool(value),
            Value::String(value) => visitor.visit_string(value),
            Value::Number(value) => {
                if let Some(value) = value.as_u64() {
                    visitor.visit_u64(value)
                } else if let Some(value) = value.as_i64() {
                    visitor.visit_i64(value)
                } else {
                    visitor.visit_f64(value.as_f64().ok_or(DecodeError)?)
                }
            }
            Value::Array(values) => {
                let mut access = ArrayReader {
                    values: values.into_iter(),
                    state: self.state,
                };
                let result = visitor.visit_seq(&mut access)?;
                self.state.check()?;
                if access.values.len() != 0 {
                    return Err(DecodeError);
                }
                Ok(result)
            }
            Value::Object(values) => {
                let mut access = ObjectReader {
                    values: values.into_iter(),
                    pending: None,
                    state: self.state,
                };
                let result = visitor.visit_map(&mut access)?;
                self.state.check()?;
                if access.values.len() != 0 || access.pending.is_some() {
                    return Err(DecodeError);
                }
                Ok(result)
            }
        }
    }
    fn deserialize_option<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        if self.value.is_null() {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        visitor.visit_newtype_struct(self)
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        let (variant, value) = match self.value {
            serde_json::Value::String(variant) => (variant, None),
            serde_json::Value::Object(values) if values.len() == 1 => {
                let (variant, value) = values.into_iter().next().ok_or(DecodeError)?;
                (variant, Some(value))
            }
            _ => return Err(DecodeError),
        };
        visitor.visit_enum(VariantReader {
            variant,
            value,
            state: self.state,
        })
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(
        self,
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.discard(self.value)?;
        visitor.visit_unit()
    }
    typed_value! {
        deserialize_bool => serde_json::Value::Bool(_),
        deserialize_i8 => serde_json::Value::Number(_),
        deserialize_i16 => serde_json::Value::Number(_),
        deserialize_i32 => serde_json::Value::Number(_),
        deserialize_i64 => serde_json::Value::Number(_),
        deserialize_i128 => serde_json::Value::Number(_),
        deserialize_u8 => serde_json::Value::Number(_),
        deserialize_u16 => serde_json::Value::Number(_),
        deserialize_u32 => serde_json::Value::Number(_),
        deserialize_u64 => serde_json::Value::Number(_),
        deserialize_u128 => serde_json::Value::Number(_),
        deserialize_f32 => serde_json::Value::Number(_),
        deserialize_f64 => serde_json::Value::Number(_),
        deserialize_char => serde_json::Value::String(_),
        deserialize_str => serde_json::Value::String(_),
        deserialize_string => serde_json::Value::String(_),
        deserialize_identifier => serde_json::Value::String(_),
        deserialize_bytes => serde_json::Value::String(_) | serde_json::Value::Array(_),
        deserialize_byte_buf => serde_json::Value::String(_) | serde_json::Value::Array(_),
        deserialize_unit => serde_json::Value::Null,
        deserialize_unit_struct(_name: &'static str) => serde_json::Value::Null,
        deserialize_seq => serde_json::Value::Array(_),
        deserialize_tuple(_len: usize) => serde_json::Value::Array(_),
        deserialize_tuple_struct(_name: &'static str, _len: usize) => serde_json::Value::Array(_),
        deserialize_map => serde_json::Value::Object(_),
        deserialize_struct(_name: &'static str, _fields: &'static [&'static str]) => serde_json::Value::Object(_) | serde_json::Value::Array(_),
    }
}

struct ArrayReader<'a, 'b, 'c> {
    values: std::vec::IntoIter<serde_json::Value>,
    state: &'a State<'b, 'c>,
}
impl<'de> SeqAccess<'de> for ArrayReader<'_, '_, '_> {
    type Error = DecodeError;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> std::result::Result<Option<T::Value>, DecodeError> {
        self.state.check()?;
        self.values
            .next()
            .map(|value| {
                seed.deserialize(ValueReader {
                    value,
                    state: self.state,
                })
            })
            .transpose()
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.values.len())
    }
}

struct ObjectReader<'a, 'b, 'c> {
    values: serde_json::map::IntoIter,
    pending: Option<serde_json::Value>,
    state: &'a State<'b, 'c>,
}
impl<'de> MapAccess<'de> for ObjectReader<'_, '_, '_> {
    type Error = DecodeError;
    fn next_key_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> std::result::Result<Option<T::Value>, DecodeError> {
        self.state.check()?;
        if self.pending.is_some() {
            return Err(DecodeError);
        }
        let Some((key, value)) = self.values.next() else {
            return Ok(None);
        };
        self.pending = Some(value);
        seed.deserialize(de::value::StringDeserializer::<DecodeError>::new(key))
            .map(Some)
    }
    fn next_value_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> std::result::Result<T::Value, DecodeError> {
        self.state.check()?;
        seed.deserialize(ValueReader {
            value: self.pending.take().ok_or(DecodeError)?,
            state: self.state,
        })
    }
    fn size_hint(&self) -> Option<usize> {
        Some(self.values.len())
    }
}

struct VariantReader<'a, 'b, 'c> {
    variant: String,
    value: Option<serde_json::Value>,
    state: &'a State<'b, 'c>,
}
impl<'de, 'a, 'b, 'c> EnumAccess<'de> for VariantReader<'a, 'b, 'c> {
    type Error = DecodeError;
    type Variant = ValueVariant<'a, 'b, 'c>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> std::result::Result<(V::Value, Self::Variant), DecodeError> {
        self.state.check()?;
        let value = seed.deserialize(de::value::StringDeserializer::<DecodeError>::new(
            self.variant,
        ))?;
        Ok((
            value,
            ValueVariant {
                value: self.value,
                state: self.state,
            },
        ))
    }
}
struct ValueVariant<'a, 'b, 'c> {
    value: Option<serde_json::Value>,
    state: &'a State<'b, 'c>,
}
impl<'de> VariantAccess<'de> for ValueVariant<'_, '_, '_> {
    type Error = DecodeError;
    fn unit_variant(self) -> std::result::Result<(), DecodeError> {
        self.state.check()?;
        match self.value {
            None | Some(serde_json::Value::Null) => Ok(()),
            _ => Err(DecodeError),
        }
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> std::result::Result<T::Value, DecodeError> {
        self.state.check()?;
        seed.deserialize(ValueReader {
            value: self.value.ok_or(DecodeError)?,
            state: self.state,
        })
    }
    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        let value = self.value.ok_or(DecodeError)?;
        if let serde_json::Value::Array(values) = &value {
            if values.is_empty() {
                return visitor.visit_unit();
            }
        } else {
            return Err(DecodeError);
        }
        de::Deserializer::deserialize_tuple(
            ValueReader {
                value,
                state: self.state,
            },
            len,
            visitor,
        )
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> std::result::Result<V::Value, DecodeError> {
        self.state.check()?;
        let value = self.value.ok_or(DecodeError)?;
        if !value.is_object() {
            return Err(DecodeError);
        }
        de::Deserializer::deserialize_struct(
            ValueReader {
                value,
                state: self.state,
            },
            "metadata variant",
            fields,
            visitor,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryOptions, ReadLimit};
    use serde::Deserialize;
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    #[derive(Debug, Deserialize, PartialEq)]
    struct Closed {
        count: u64,
        name: String,
        optional: Option<String>,
        items: Vec<i64>,
        nested: BTreeMap<String, Option<Vec<i64>>>,
    }
    #[derive(Debug, Deserialize, PartialEq)]
    enum Choice {
        Unit,
        New(Vec<u64>),
        Tuple(u64, Option<String>),
        Struct { count: u64, items: Vec<String> },
    }

    fn parity<T: DeserializeOwned + PartialEq + std::fmt::Debug>(
        budget: &ColdBudget<'_>,
        value: Value,
    ) {
        let expected = serde_json::from_value::<T>(value.clone());
        let actual = budget.value::<T>(value);
        match expected {
            Ok(expected) => assert_eq!(actual.unwrap(), expected),
            Err(_) => assert!(
                matches!(actual, Err(Error::Corrupt(_))),
                "closed reader broadened the canonical JSON entry point"
            ),
        }
    }

    #[derive(Debug, PartialEq)]
    struct Bytes(Vec<u8>);
    impl<'de> serde::Deserialize<'de> for Bytes {
        fn deserialize<D: de::Deserializer<'de>>(
            deserializer: D,
        ) -> std::result::Result<Self, D::Error> {
            struct ByteVisitor;
            impl<'de> Visitor<'de> for ByteVisitor {
                type Value = Bytes;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("bytes")
                }
                fn visit_str<E: de::Error>(self, value: &str) -> std::result::Result<Bytes, E> {
                    Ok(Bytes(value.as_bytes().to_vec()))
                }
                fn visit_string<E: de::Error>(
                    self,
                    value: String,
                ) -> std::result::Result<Bytes, E> {
                    Ok(Bytes(value.into_bytes()))
                }
                fn visit_u64<E: de::Error>(self, value: u64) -> std::result::Result<Bytes, E> {
                    Ok(Bytes(vec![value as u8]))
                }
                fn visit_seq<A: SeqAccess<'de>>(
                    self,
                    mut values: A,
                ) -> std::result::Result<Bytes, A::Error> {
                    let mut out = Vec::new();
                    while let Some(value) = values.next_element()? {
                        out.push(value);
                    }
                    Ok(Bytes(out))
                }
            }
            deserializer.deserialize_bytes(ByteVisitor)
        }
    }

    #[test]
    fn value_entry_points_keep_uuid_strings_and_byte_kinds() {
        let options = HistoryOptions::default();
        let budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: None,
            held: 0,
        })
        .unwrap();
        let id = uuid::Uuid::new_v4();
        parity::<uuid::Uuid>(&budget, json!(id.to_string()));
        parity::<uuid::Uuid>(
            &budget,
            json!([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        );
        for value in [
            json!("hello"),
            json!([1, 2, 3]),
            json!(7),
            json!(null),
            json!({}),
        ] {
            parity::<Bytes>(&budget, value);
        }
        for value in [
            json!(null),
            json!(true),
            json!(0),
            json!(-1),
            json!(1.25),
            json!("1"),
            json!([]),
            json!({}),
        ] {
            parity::<String>(&budget, value.clone());
            parity::<bool>(&budget, value.clone());
            parity::<u64>(&budget, value.clone());
            parity::<i64>(&budget, value.clone());
            parity::<()>(&budget, value.clone());
            parity::<Vec<u64>>(&budget, value.clone());
            parity::<BTreeMap<String, u64>>(&budget, value);
        }
    }

    #[test]
    fn closed_schemas_preserve_values_options_and_all_enum_shapes() {
        let options = HistoryOptions::default();
        let budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: None,
            held: 0,
        })
        .unwrap();
        for value in [
            json!({"count": 7, "name": "Unicode 🦀", "optional": null, "items": [-2, 3], "nested": {"a": null, "b": [1,-1]}}),
            json!({"count": 0, "name": "", "optional": "x", "items": [], "nested": {}, "unknown": [1, {}, [null]]}),
            json!({"count": -1, "name": "x", "items": [], "nested": {}}),
            json!({"count": "invalid", "name": "x", "items": [], "nested": {}}),
            json!({"count": 1, "name": null, "items": [], "nested": {}}),
            json!([]),
        ] {
            parity::<Closed>(&budget, value);
        }
        for value in [
            json!("Unit"),
            json!({"Unit": null}),
            json!({"New": [0, u64::MAX]}),
            json!({"Tuple": [1, null]}),
            json!({"Struct": {"count": 8, "items": ["x"]}}),
            json!({"New": []}),
            json!({"Tuple": []}),
            json!({"Unit": 1}),
            json!({"New": [1], "Unit": null}),
            json!("Unknown"),
        ] {
            parity::<Choice>(&budget, value);
        }
        for value in [
            json!(null),
            json!(true),
            json!(i64::MIN),
            json!(u64::MAX),
            json!(1.25),
            json!("x"),
            json!([1, {"a": [null, false]}]),
        ] {
            parity::<Value>(&budget, value);
        }
    }

    #[derive(Debug, Deserialize, PartialEq)]
    struct One {
        count: u64,
    }

    #[test]
    fn ignored_subtrees_and_enum_seeds_poll_controls_instead_of_bulk_drop() {
        let cancel = Arc::new(AtomicBool::new(false));
        let options = HistoryOptions {
            cancel: Some(cancel.clone()),
            deadline: None,
        };
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(32 << 20),
            held: 0,
        })
        .unwrap();
        let value = json!({"count": 1, "unknown": (0..5000).map(|n| json!({"nested": [n, n]})).collect::<Vec<_>>()});
        let bytes = serde_json::to_vec(&value).unwrap();
        let parsed: super::super::Loaded<Value> = budget.json(&bytes).unwrap();
        let held = budget.used();
        let observer = |stage: &str, at: usize| {
            if stage == "metadata value decode" && at == 50 {
                cancel.store(true, Ordering::Relaxed);
            }
        };
        budget.observe_keys(&observer);
        assert!(matches!(
            budget.value::<One>(parsed.value),
            Err(Error::Cancelled)
        ));
        assert_eq!(budget.used(), held);
        budget.release(parsed.charge);
        cancel.store(false, Ordering::Relaxed);
        let parsed: super::super::Loaded<Value> = budget.json(br#"{"New":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30]}"#).unwrap();
        assert!(matches!(
            budget.value::<Choice>(parsed.value),
            Err(Error::Cancelled)
        ));
        budget.release(parsed.charge);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn malformed_large_strings_have_constant_sanitized_errors() {
        let options = HistoryOptions::default();
        let mut budget = ColdBudget::new(ReadLimit {
            options: &options,
            memory: Some(512 << 20),
            held: 0,
        })
        .unwrap();
        let bytes = format!("{{\"count\":\"{}\"}}", "🦀".repeat(256 << 10)).into_bytes();
        let parsed: super::super::Loaded<Value> = budget.json(&bytes).unwrap();
        let error = budget.value::<One>(parsed.value).unwrap_err();
        assert!(matches!(error, Error::Corrupt(_)));
        assert!(error.to_string().len() < 128);
        budget.release(parsed.charge);
        assert_eq!(budget.used(), 0);
    }
}
