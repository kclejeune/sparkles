//! One encoded document, with physical bytes before the original final credit.
use super::{Error, NativeBudget, NativeReservation, OutputBudget, Result};
use serde::Serialize;
use serde_json::value::RawValue;
use std::{io::Write, sync::Arc};

struct BytesBacking {
    bytes: Vec<u8>,
    _credit: NativeReservation,
}

/// One encoded native frame. Clones and borrowed slices retain the same actual
/// allocation; this type has no unrestricted owning byte extractor.
#[derive(Clone)]
pub struct OwnedBytes {
    inner: Arc<BytesBacking>,
}
impl AsRef<[u8]> for OwnedBytes {
    fn as_ref(&self) -> &[u8] {
        &self.inner.bytes
    }
}
impl OwnedBytes {
    pub fn len(&self) -> usize {
        self.inner.bytes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.bytes.is_empty()
    }
}
impl std::fmt::Debug for OwnedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedBytes")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

struct JsonBacking {
    value: Box<RawValue>,
    _credit: NativeReservation,
}

/// Immutable encoded JSON retaining its actual original accounting and source.
/// Clones share the same bytes. A borrowed view confers no independent ownership.
#[derive(Clone)]
pub struct OwnedJson {
    inner: Arc<JsonBacking>,
}
impl OwnedJson {
    pub fn as_raw(&self) -> &RawValue {
        &self.inner.value
    }
    pub fn as_str(&self) -> &str {
        self.as_raw().get()
    }
    pub fn len(&self) -> usize {
        self.as_str().len()
    }
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }
    pub fn try_copy_in(&self, budget: &OutputBudget) -> Result<Self> {
        budget.json(self)
    }
    pub fn try_copy_in_native(&self, budget: &NativeBudget) -> Result<Self> {
        budget.json(self)
    }
}
impl Serialize for OwnedJson {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        self.as_raw().serialize(serializer)
    }
}
impl std::fmt::Debug for OwnedJson {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.as_str().fmt(f)
    }
}

impl OutputBudget {
    pub fn json<T: Serialize + ?Sized>(&self, value: &T) -> Result<OwnedJson> {
        self.native_budget()?.json(value)
    }
}

impl NativeBudget {
    /// Encode into one real credited frame. The callback borrows the writer and
    /// must account for any separate workspace it allocates. No partial frame is
    /// published after callback, control, allocation or write failure.
    pub fn encode_bytes(
        &self,
        encode: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
    ) -> Result<OwnedBytes> {
        self.check()?;
        let header = self.arc_bytes::<BytesBacking>()?;
        let mut writer = JsonWriter {
            bytes: Vec::new(),
            first: None,
            budget: self,
            credit: self.reserve(header)?,
        };
        let encoded = encode(&mut writer);
        if let Some(error) = writer.first.take() {
            return Err(error);
        }
        self.check()?;
        encoded.map_err(Error::Io)?;
        let JsonWriter { bytes, credit, .. } = writer;
        // Retain capacity, not only length: the Vec is moved without a shrink.
        let output = OwnedBytes {
            inner: Arc::new(BytesBacking {
                bytes,
                _credit: credit,
            }),
        };
        self.check()?;
        Ok(output)
    }

    /// Serialize borrowed input into this genuine receiving owner. Every growth
    /// is admitted before allocating, including old/new capacity overlap.
    pub fn json<T: Serialize + ?Sized>(&self, value: &T) -> Result<OwnedJson> {
        self.check()?;
        let header = self.arc_bytes::<JsonBacking>()?;
        let mut writer = JsonWriter {
            bytes: Vec::new(),
            first: None,
            budget: self,
            credit: self.reserve(header)?,
        };
        let encoded = serde_json::to_writer(&mut writer, value);
        if let Some(error) = writer.first.take() {
            return Err(error);
        }
        self.check()?;
        encoded.map_err(|error| Error::Io(error.into()))?;
        let depth = json_depth(self, &writer.bytes)?;
        // serde_json1.0.154 RawValue validation walks nesting iteratively using
        // a Vec<u8> of frames. Cover its growing old/new scratch, separately
        // from the String -> Box shrink's old/new encoded-byte overlap.
        let scratch = depth
            .checked_mul(4)
            .and_then(|n| n.checked_add(64))
            .ok_or_else(|| self.overflow())?;
        let temporary = writer
            .bytes
            .len()
            .checked_add(scratch)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or_else(|| self.overflow())?;
        writer.credit.grow_by(temporary)?;
        self.check()?;
        let JsonWriter { bytes, credit, .. } = writer;
        let encoded = String::from_utf8(bytes)
            .map_err(|_| Error::invalid("native JSON encoder produced invalid UTF-8"))?;
        // Safe validation also rejects malformed user Serialize implementations;
        // no unchecked RawValue constructor or private serde protocol is used.
        let value = RawValue::from_string(encoded)
            .map_err(|_| Error::invalid("native JSON encoder produced an invalid document"))?;
        self.check()?;
        let mut credit = credit;
        credit.release_to(
            header
                .checked_add(u64::try_from(value.get().len()).map_err(|_| self.overflow())?)
                .ok_or_else(|| self.overflow())?,
        )?;
        let output = OwnedJson {
            inner: Arc::new(JsonBacking {
                value,
                _credit: credit,
            }),
        };
        self.check()?;
        Ok(output)
    }
}

struct JsonWriter<'a> {
    bytes: Vec<u8>,
    first: Option<Error>,
    budget: &'a NativeBudget,
    credit: NativeReservation,
}
impl JsonWriter<'_> {
    fn append(&mut self, input: &[u8]) -> Result<()> {
        self.budget.check()?;
        let needed = self
            .bytes
            .len()
            .checked_add(input.len())
            .ok_or_else(|| self.budget.overflow())?;
        if needed > self.bytes.capacity() {
            let old = self.bytes.capacity();
            let capacity = needed.max(old.saturating_mul(2)).max(64);
            if capacity > isize::MAX as usize {
                return Err(self.budget.overflow());
            }
            let quoted = u64::try_from(capacity).map_err(|_| self.budget.overflow())?;
            // Existing credit still includes the old physical allocation.
            self.credit.grow_by(quoted)?;
            #[cfg(test)]
            {
                use crate::algo::memory::probe;
                probe::at(probe::Point::Allocation("native-json-buffer", capacity));
                if probe::fails("native-json-buffer") {
                    return Err(Error::invalid("injected native JSON allocation failure"));
                }
            }
            self.budget.check()?;
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(|_| Error::invalid("native JSON allocation failed"))?;
            if self.bytes.capacity() != capacity {
                return Err(Error::invalid("native JSON capacity exceeds admission"));
            }
            // Reallocation has retired the old capacity before it is returned.
            self.credit.release_to(self.credit.bytes() - old as u64)?;
        }
        self.budget.check()?;
        self.bytes.extend_from_slice(input);
        self.budget.check()
    }
}
impl Write for JsonWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if self.first.is_none() {
            match self.append(input) {
                Ok(()) => return Ok(input.len()),
                Err(error) => self.first = Some(error),
            }
        }
        Err(std::io::Error::other("native JSON output refused"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn json_depth(budget: &NativeBudget, bytes: &[u8]) -> Result<usize> {
    let (mut depth, mut maximum, mut string, mut escaped) = (0usize, 0usize, false, false);
    for (i, &byte) in bytes.iter().enumerate() {
        if i.is_multiple_of(1024) {
            budget.check()?;
        }
        if string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                string = false;
            }
        } else {
            match byte {
                b'"' => string = true,
                b'{' | b'[' => {
                    depth = depth.checked_add(1).ok_or_else(|| budget.overflow())?;
                    maximum = maximum.max(depth);
                }
                b'}' | b']' => {
                    depth = depth
                        .checked_sub(1)
                        .ok_or_else(|| Error::invalid("unbalanced native JSON document"))?;
                }
                _ => {}
            }
        }
    }
    if depth != 0 || string || bytes.is_empty() {
        return Err(Error::invalid("incomplete native JSON document"));
    }
    budget.check()?;
    Ok(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        algo::memory::probe,
        error::BudgetKind,
        sparql::{QueryOptions, QueryResult, pool::QueryPool, query},
        store::{Store, StoreOptions},
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture() -> (QueryResult, Arc<QueryPool>) {
        let store = Store::in_memory(StoreOptions::default());
        let pool = QueryPool::new(16 << 20);
        let result = query(
            store.snapshot(),
            "SELECT ?v WHERE { VALUES ?v { \"seed\" } }",
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(4 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        (result, pool)
    }

    #[test]
    fn serializer_secondary_error_preserves_original_control_after_a_real_prefix() {
        struct CancelThenError(Arc<std::sync::atomic::AtomicBool>);
        impl Serialize for CancelThenError {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                use serde::ser::Error as _;
                let _encoded = serializer.serialize_str("actual encoded prefix")?;
                self.0.store(true, Ordering::SeqCst);
                Err(S::Error::custom("secondary formatter error"))
            }
        }
        let (result, pool) = fixture();
        let before = result.inner.ctx.live_bytes();
        let error = result
            .output_budget()
            .json(&CancelThenError(result.inner.ctx.cancel.clone()))
            .err()
            .unwrap();
        let after = result.inner.ctx.live_bytes();
        drop(result);
        assert_eq!(after, before, "the actual partial buffer retired");
        assert_eq!(pool.granted(), 0);
        assert!(matches!(error, Error::Cancelled), "{error:?}");
    }

    #[test]
    fn encoded_json_refuses_before_allocation_and_after_a_real_nonempty_prefix() {
        for available in [0, 63, 512] {
            let (result, pool) = fixture();
            let ctx = &result.inner.ctx;
            let header = crate::sparql::cursor::arc_backing_bytes::<JsonBacking>(ctx).unwrap();
            let remaining = if available == 0 {
                header - 1
            } else {
                header + available
            };
            let pressure = ctx.charge(ctx.memory_remaining() - remaining).unwrap();
            let before = ctx.live_bytes();
            let callbacks = Arc::new(AtomicUsize::new(0));
            let observed = callbacks.clone();
            let hook = probe::install(
                move |point| {
                    if matches!(point, probe::Point::Allocation("native-json-buffer", _)) {
                        observed.fetch_add(1, Ordering::SeqCst);
                    }
                },
                None,
            );
            let original = "x".repeat(64 << 10);
            let error = result
                .output_budget()
                .json(&("prefix", &original))
                .err()
                .unwrap();
            assert!(
                matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory && b.limit == 4 << 20 && b.requested > b.limit)
            );
            assert_eq!(
                callbacks.load(Ordering::SeqCst),
                usize::from(available == 512)
            );
            assert_eq!(ctx.live_bytes(), before);
            assert_eq!(original.len(), 64 << 10);
            drop(hook);
            drop(pressure);
            let document = result.output_budget().json(&("prefix", &original)).unwrap();
            let actual: serde_json::Value = serde_json::from_str(document.as_str()).unwrap();
            assert_eq!(actual[0], "prefix");
            assert_eq!(actual[1], original);
            drop(actual);
            drop(document);
            drop(result);
            assert_eq!(pool.granted(), 0);
        }
    }

    #[test]
    fn encoded_json_preserves_server_refusal_from_the_original_pool() {
        let store = Store::in_memory(StoreOptions::default());
        let pool = QueryPool::new(8 << 20);
        let result = query(
            store.snapshot(),
            "SELECT ?v WHERE { VALUES ?v { \"seed\" } }",
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(32 << 20),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        let ctx = &result.inner.ctx;
        let pressure = ctx.charge((8 << 20) - ctx.live_bytes() - 1).unwrap();
        let before = ctx.live_bytes();
        let error = result.output_budget().json(&"real receiver").err().unwrap();
        assert!(
            matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::ServerMemory && b.limit == 8 << 20 && b.requested > b.limit)
        );
        assert_eq!(ctx.live_bytes(), before);
        drop(pressure);
        let output = result.output_budget().json(&"real receiver").unwrap();
        assert_eq!(output.as_str(), "\"real receiver\"");
        drop(output);
        drop(result);
        drop(store);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn encoded_json_allocation_fault_and_before_after_copy_cancel_refund_partials() {
        for late in [false, true] {
            let (result, pool) = fixture();
            let before = result.inner.ctx.live_bytes();
            if late {
                struct CancelAfter(Arc<std::sync::atomic::AtomicBool>);
                impl Serialize for CancelAfter {
                    fn serialize<S: serde::Serializer>(
                        &self,
                        s: S,
                    ) -> std::result::Result<S::Ok, S::Error> {
                        let value = s.serialize_str("the whole actual payload")?;
                        self.0.store(true, Ordering::SeqCst);
                        Ok(value)
                    }
                }
                assert!(matches!(
                    result
                        .output_budget()
                        .json(&CancelAfter(result.inner.ctx.cancel.clone())),
                    Err(Error::Cancelled)
                ));
            } else {
                let calls = Arc::new(AtomicUsize::new(0));
                let observed = calls.clone();
                let flag = result.inner.ctx.cancel.clone();
                let hook = probe::install(
                    move |point| {
                        if matches!(point, probe::Point::Allocation("native-json-buffer", _)) {
                            observed.fetch_add(1, Ordering::SeqCst);
                            flag.store(true, Ordering::SeqCst);
                        }
                    },
                    None,
                );
                assert!(matches!(
                    result.output_budget().json(&"payload"),
                    Err(Error::Cancelled)
                ));
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                drop(hook);
            }
            assert_eq!(result.inner.ctx.live_bytes(), before);
            drop(result);
            assert_eq!(pool.granted(), 0);
        }
        let (result, pool) = fixture();
        let before = result.inner.ctx.live_bytes();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let hook = probe::install(
            move |point| {
                if matches!(point, probe::Point::Allocation("native-json-buffer", _)) {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            },
            Some("native-json-buffer"),
        );
        assert!(matches!(
            result.output_budget().json(&"payload"),
            Err(Error::Invalid(_))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(result.inner.ctx.live_bytes(), before);
        drop(hook);
        drop(result);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn encoded_json_keeps_the_first_typed_refusal_over_a_secondary_serde_error() {
        use serde::ser::{Error as _, SerializeSeq};
        struct Secondary<'a>(&'a str);
        impl Serialize for Secondary<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                let mut sequence = serializer.serialize_seq(Some(2))?;
                sequence.serialize_element("accepted prefix")?;
                if sequence.serialize_element(self.0).is_err() {
                    return Err(S::Error::custom("secondary serialization error"));
                }
                sequence.end()
            }
        }
        let (result, pool) = fixture();
        let ctx = &result.inner.ctx;
        let header = crate::sparql::cursor::arc_backing_bytes::<JsonBacking>(ctx).unwrap();
        let pressure = ctx.charge(ctx.memory_remaining() - header - 512).unwrap();
        let before = ctx.live_bytes();
        let payload = "x".repeat(64 << 10);
        assert!(
            matches!(result.output_budget().json(&Secondary(&payload)), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory && b.limit == 4 << 20)
        );
        assert_eq!(ctx.live_bytes(), before);
        drop(pressure);
        drop(result);
        assert_eq!(pool.granted(), 0);
    }

    #[test]
    fn encoded_json_receiving_copy_has_its_own_actual_context_and_last_owner() {
        let (source, source_pool) = fixture();
        let (target, target_pool) = fixture();
        let source_ctx = Arc::downgrade(&source.inner.ctx);
        let target_ctx = Arc::downgrade(&target.inner.ctx);
        let payload = "x".repeat(64 << 10);
        let original = source.output_budget().json(&payload).unwrap();
        let copied = original.try_copy_in(&target.output_budget()).unwrap();
        assert_eq!(original.as_str(), copied.as_str());
        let shared = copied.clone();
        assert!(Arc::ptr_eq(&copied.inner, &shared.inner));
        assert!(!Arc::ptr_eq(&copied.inner, &original.inner));
        drop(source);
        drop(original);
        assert!(source_ctx.upgrade().is_none());
        assert_eq!(source_pool.granted(), 0);
        drop(target);
        drop(copied);
        assert!(target_ctx.upgrade().is_some());
        assert!(target_pool.granted() >= 64 << 10);
        assert_eq!(shared.as_raw().get().len(), (64 << 10) + 2);
        drop(shared);
        assert!(target_ctx.upgrade().is_none());
        assert_eq!(target_pool.granted(), 0);
    }

    #[test]
    fn encoded_json_counts_only_real_structure_and_keeps_immutable_bytes_after_cancel() {
        let (result, pool) = fixture();
        let input = serde_json::json!({"escaped": "\\\"{[}]", "nested": [[{"x": true}]]});
        let output = result.output_budget().json(&input).unwrap();
        assert_eq!(
            json_depth(
                &result.output_budget().native_budget().unwrap(),
                output.as_str().as_bytes()
            )
            .unwrap(),
            4
        );
        result.inner.ctx.cancel.store(true, Ordering::SeqCst);
        // Serialization reads the existing immutable RawValue; it does not
        // decode or allocate another engine-owned value after cancellation.
        let application_oracle = serde_json::to_value(&output).unwrap();
        assert_eq!(application_oracle, input);
        drop(application_oracle);
        drop(result);
        assert!(pool.granted() > 0);
        drop(output);
        assert_eq!(pool.granted(), 0);
    }
}
