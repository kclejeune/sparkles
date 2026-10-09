//! Hand-written JNI entry points for the calls that small reads make most often (P04 §5.4).
//!
//! Every other call goes through UniFFI and JNA. These functions call the same Rust methods
//! as the UniFFI exports and pass the same bytes, so only the transport changes. They take
//! the handles that UniFFI gave the Kotlin objects and borrow the objects behind them. The
//! Kotlin side holds the object's UniFFI call counter for the duration of the call
//! (`uniffiBorrowHandle`, added to the generated classes by the `ffiBindings` task), so the
//! object cannot be freed while it is borrowed. Objects these functions create are handed
//! out as UniFFI handles, which the Kotlin side wraps in the generated classes, and freed
//! as UniFFI frees them.
//!
//! An error is thrown as `SparklesJniFailure` carrying the error in UniFFI's own
//! serialization, which the Kotlin side reads back with the generated converter, and a
//! panic is thrown as one carrying its message, which becomes UniFFI's
//! `InternalException`. Neither unwinds through the JVM.

use crate::error::{ErrorKind, FfiError, FfiResult};
use crate::query::{Execution, FfiQuery, QueryOpts};
use crate::read::FfiCursor;
use crate::{FfiDataset, FfiReadTxn, UniFfiTag};
use jni_sys::{
    JNIEnv, JNINativeInterface__1_6, jboolean, jbyte, jbyteArray, jclass, jint, jlong, jlongArray,
    jsize, jvalue,
};
use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::null_mut;
use std::sync::Arc;
use uniffi::{Handle, HandleAlloc, Lift, Lower};

/// Changes when an entry point's signature changes; the Kotlin side checks it before it
/// turns these calls on.
const ABI_VERSION: jint = 1;

type Env = *mut JNIEnv;

/// What the failure's `kind` says its bytes hold.
const FAILURE_ERROR: jint = 0;
const FAILURE_PANIC: jint = 1;

/// The function table of the JVM's JNI 1.6 interface.
///
/// # Safety
/// `env` must be the environment the JVM passed to the current native call.
unsafe fn jni<'a>(env: Env) -> &'a JNINativeInterface__1_6 {
    unsafe { &(**env).v1_6 }
}

/// The object behind a handle that UniFFI made with `Arc::into_raw`, borrowed.
///
/// # Safety
/// The handle must be one UniFFI made for a `T`, and the Kotlin object that owns it must
/// hold its call counter until the borrow ends.
unsafe fn borrow<'a, T>(handle: jlong) -> &'a T {
    unsafe { &*(handle as u64 as usize as *const T) }
}

/// A new object as a UniFFI handle, which the Kotlin side wraps in its generated class.
fn handle<T: HandleAlloc<UniFfiTag>>(value: Arc<T>) -> jlong {
    T::new_handle(value).as_raw() as jlong
}

/// Free an object that a handle owns, as UniFFI's `free` function does.
///
/// # Safety
/// The handle must be one UniFFI made for a `T`, and must not be used again.
unsafe fn free<T: HandleAlloc<UniFfiTag>>(handle: jlong) {
    if handle != 0 {
        drop(unsafe { T::consume_handle(Handle::from_raw_unchecked(handle as u64)) });
    }
}

/// The bytes of a Java `byte[]`, copied.
unsafe fn read_bytes(env: Env, array: jbyteArray) -> Vec<u8> {
    unsafe {
        let f = jni(env);
        let n = (f.GetArrayLength)(env, array).max(0) as usize;
        let mut v = Vec::<u8>::with_capacity(n);
        (f.GetByteArrayRegion)(env, array, 0, n as jsize, v.as_mut_ptr() as *mut jbyte);
        v.set_len(n);
        v
    }
}

/// A new Java `byte[]` with these bytes, or null with an `OutOfMemoryError` pending.
unsafe fn new_bytes(env: Env, bytes: &[u8]) -> jbyteArray {
    unsafe {
        let f = jni(env);
        let Ok(n) = jsize::try_from(bytes.len()) else {
            return null_mut();
        };
        let array = (f.NewByteArray)(env, n);
        if !array.is_null() {
            (f.SetByteArrayRegion)(env, array, 0, n, bytes.as_ptr() as *const jbyte);
        }
        array
    }
}

/// Store `value` in `out[0]`.
unsafe fn set_out(env: Env, out: jlongArray, value: jlong) {
    unsafe { (jni(env).SetLongArrayRegion)(env, out, 0, 1, &value) }
}

/// Throw a `SparklesJniFailure` unless an exception is pending already.
unsafe fn throw(env: Env, kind: jint, payload: &[u8]) {
    unsafe {
        let f = jni(env);
        if (f.ExceptionCheck)(env) {
            return;
        }
        let class: jclass = (f.FindClass)(
            env,
            c"io/github/kclejeune/sparkles/jena/internal/ffi/SparklesJniFailure".as_ptr(),
        );
        if class.is_null() {
            return;
        }
        let ctor = (f.GetMethodID)(env, class, c"<init>".as_ptr(), c"(I[B)V".as_ptr());
        if ctor.is_null() {
            return;
        }
        let bytes = new_bytes(env, payload);
        if bytes.is_null() {
            return;
        }
        let args = [jvalue { i: kind }, jvalue { l: bytes }];
        let failure = (f.NewObjectA)(env, class, ctor, args.as_ptr());
        if !failure.is_null() {
            (f.Throw)(env, failure);
        }
    }
}

/// The message of a panic, as UniFFI reports it.
fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(s) = panic.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = panic.downcast_ref::<String>() {
        s.clone()
    } else {
        "Unknown panic!".to_string()
    }
}

/// Run `f`, and throw its error or its panic as a Java exception. The return value is then
/// `failed`, which the Java side never sees because the exception is pending.
fn call<R>(env: Env, failed: R, f: impl FnOnce() -> FfiResult<R>) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            let mut buf = Vec::new();
            <FfiError as Lower<UniFfiTag>>::write(e, &mut buf);
            unsafe { throw(env, FAILURE_ERROR, &buf) };
            failed
        }
        Err(panic) => {
            let message = panic_message(panic.as_ref());
            unsafe { throw(env, FAILURE_PANIC, message.as_bytes()) };
            failed
        }
    }
}

/// The options of a query, in UniFFI's serialization of the record.
fn query_opts(bytes: &[u8]) -> FfiResult<QueryOpts> {
    let mut slice = bytes;
    <QueryOpts as Lift<UniFfiTag>>::try_read(&mut slice)
        .map_err(|e| FfiError::new(ErrorKind::Malformed, format!("query options: {e}")))
}

fn utf8(bytes: Vec<u8>) -> FfiResult<String> {
    String::from_utf8(bytes)
        .map_err(|e| FfiError::new(ErrorKind::Malformed, format!("query text: {e}")))
}

// The entry points. Their names follow JNI's mangling of
// io.github.kclejeune.sparkles.jena.internal.ffi.SparklesJni.

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_abiVersion(
    _env: Env,
    _class: jclass,
) -> jint {
    ABI_VERSION
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_beginRead(
    env: Env,
    _class: jclass,
    dataset: jlong,
) -> jlong {
    call(env, 0, || {
        let ds = unsafe { borrow::<FfiDataset>(dataset) };
        Ok(handle(ds.begin_read()))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_containsHead(
    env: Env,
    _class: jclass,
    dataset: jlong,
    pattern: jbyteArray,
) -> jboolean {
    call(env, false, || {
        let ds = unsafe { borrow::<FfiDataset>(dataset) };
        ds.contains(unsafe { read_bytes(env, pattern) })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_containsRead(
    env: Env,
    _class: jclass,
    txn: jlong,
    pattern: jbyteArray,
) -> jboolean {
    call(env, false, || {
        let t = unsafe { borrow::<FfiReadTxn>(txn) };
        t.contains(unsafe { read_bytes(env, pattern) })
    })
}

/// The first batch of a `find`, with the cursor's handle (0 when the batch held every
/// match) in `out[0]`.
unsafe fn find_result(env: Env, r: crate::FindResult, out: jlongArray) -> jbyteArray {
    unsafe {
        let array = new_bytes(env, &r.batch);
        if array.is_null() {
            // the cursor is dropped here, and the pending OutOfMemoryError reaches Java
            return null_mut();
        }
        set_out(env, out, r.cursor.map_or(0, handle));
        array
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_findHead(
    env: Env,
    _class: jclass,
    dataset: jlong,
    pattern: jbyteArray,
    first_rows: jint,
    out: jlongArray,
) -> jbyteArray {
    call(env, null_mut(), || {
        let ds = unsafe { borrow::<FfiDataset>(dataset) };
        let r = ds.find(
            unsafe { read_bytes(env, pattern) },
            first_rows.max(0) as u32,
        )?;
        Ok(unsafe { find_result(env, r, out) })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_findRead(
    env: Env,
    _class: jclass,
    txn: jlong,
    pattern: jbyteArray,
    first_rows: jint,
    out: jlongArray,
) -> jbyteArray {
    call(env, null_mut(), || {
        let t = unsafe { borrow::<FfiReadTxn>(txn) };
        let r = t.find(
            unsafe { read_bytes(env, pattern) },
            first_rows.max(0) as u32,
        )?;
        Ok(unsafe { find_result(env, r, out) })
    })
}

/// The next batch of a cursor; `out[0]` is 1 when no quads follow it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_cursorNext(
    env: Env,
    _class: jclass,
    cursor: jlong,
    max_rows: jint,
    out: jlongArray,
) -> jbyteArray {
    call(env, null_mut(), || {
        let c = unsafe { borrow::<FfiCursor>(cursor) };
        let b = c.next_batch(max_rows.max(0) as u32)?;
        unsafe {
            let array = new_bytes(env, &b.batch);
            if !array.is_null() {
                set_out(env, out, b.done as jlong);
            }
            Ok(array)
        }
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_prepareHead(
    env: Env,
    _class: jclass,
    dataset: jlong,
    text: jbyteArray,
    options: jbyteArray,
) -> jlong {
    call(env, 0, || {
        let ds = unsafe { borrow::<FfiDataset>(dataset) };
        let text = utf8(unsafe { read_bytes(env, text) })?;
        let opts = query_opts(&unsafe { read_bytes(env, options) })?;
        Ok(handle(ds.prepare_query(text, opts)?))
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_prepareRead(
    env: Env,
    _class: jclass,
    txn: jlong,
    text: jbyteArray,
    options: jbyteArray,
) -> jlong {
    call(env, 0, || {
        let t = unsafe { borrow::<FfiReadTxn>(txn) };
        let text = utf8(unsafe { read_bytes(env, text) })?;
        let opts = query_opts(&unsafe { read_bytes(env, options) })?;
        Ok(handle(t.prepare_query(text, opts)?))
    })
}

/// `FfiQuery::execute`, its `Execution` in UniFFI's serialization of the record.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_execute(
    env: Env,
    _class: jclass,
    query: jlong,
    first_rows: jint,
) -> jbyteArray {
    call(env, null_mut(), || {
        let q = unsafe { borrow::<FfiQuery>(query) };
        let e = q.execute(first_rows.max(0) as u32)?;
        let mut buf = Vec::with_capacity(e.batch.len() + 64);
        <Execution as Lower<UniFfiTag>>::write(e, &mut buf);
        Ok(unsafe { new_bytes(env, &buf) })
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_freeReadTxnNative(
    env: Env,
    _class: jclass,
    txn: jlong,
) {
    call(env, (), || {
        unsafe { free::<FfiReadTxn>(txn) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_freeQueryNative(
    env: Env,
    _class: jclass,
    query: jlong,
) {
    call(env, (), || {
        unsafe { free::<FfiQuery>(query) };
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_freeCursorNative(
    env: Env,
    _class: jclass,
    cursor: jlong,
) {
    call(env, (), || {
        unsafe { free::<FfiCursor>(cursor) };
        Ok(())
    })
}

/// A check of the failure paths: `kind` 1 fails with an error, 2 panics, and others
/// return `kind`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_kclejeune_sparkles_jena_internal_ffi_SparklesJni_selfTestNative(
    env: Env,
    _class: jclass,
    kind: jint,
) -> jint {
    call(env, -1, || match kind {
        1 => Err(FfiError::new(ErrorKind::Invalid, "the JNI self-test error")),
        2 => panic!("the JNI self-test panic"),
        _ => Ok(kind),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `borrow` reads a handle as the object's address, which holds while UniFFI makes
    /// handles with `Arc::into_raw`.
    #[test]
    fn handles_are_object_addresses() {
        fn check<T: HandleAlloc<UniFfiTag>>(value: Arc<T>) {
            let address = Arc::as_ptr(&value) as usize as jlong;
            let h = handle(value);
            assert_eq!(h, address);
            unsafe { free::<T>(h) };
        }
        let ds = FfiDataset::memory(crate::DatasetOptions {
            blank_node_labels: crate::BlankNodeMode::Dataset,
            read_only: false,
            term_cache_size: 1000,
        });
        let txn = ds.begin_read();
        let query = txn
            .prepare_query("ASK {}".into(), query_opts_for_test())
            .unwrap();
        check(query);
        check(txn);
        check(ds);
    }

    fn query_opts_for_test() -> QueryOpts {
        QueryOpts {
            base_iri: None,
            union_default_graph: None,
            include_inferred: false,
            timeout_ms: None,
            max_rows: None,
            max_memory_bytes: None,
            max_rows_produced: None,
            allow_service: false,
            allow_private_network: false,
            binding_names: vec![],
            binding_values: vec![],
            no_cache: false,
        }
    }

    #[test]
    fn query_options_read_back() {
        let opts = QueryOpts {
            base_iri: Some("http://example.org/".into()),
            union_default_graph: Some(true),
            include_inferred: false,
            timeout_ms: Some(5),
            max_rows: None,
            max_memory_bytes: Some(1 << 20),
            max_rows_produced: None,
            allow_service: false,
            allow_private_network: true,
            binding_names: vec!["x".into()],
            binding_values: vec![1, 2, 3],
            no_cache: true,
        };
        let mut buf = Vec::new();
        <QueryOpts as Lower<UniFfiTag>>::write(opts, &mut buf);
        let back = query_opts(&buf).unwrap();
        assert_eq!(back.max_memory_bytes, Some(1 << 20));
        assert_eq!(back.binding_values, vec![1, 2, 3]);
        assert!(back.no_cache);
        assert!(query_opts(&buf[..3]).is_err());
    }
}
