//! Headers added to the engine's outbound HTTP requests (SERVICE, LOAD).
//!
//! An embedder installs one process-wide hook, for example to propagate W3C Trace
//! Context (`traceparent`) from the current `tracing` span; the engine itself depends on
//! no telemetry SDK. The hook runs on the thread making the request, inside the
//! request's `tracing` span.

use std::sync::OnceLock;

type Hook = dyn Fn(&mut dyn FnMut(&str, &str)) + Send + Sync;

static HOOK: OnceLock<Box<Hook>> = OnceLock::new();

/// Install the hook: it is called once per outbound request and passes each header to
/// add to its argument. Returns `false` (and changes nothing) when a hook is already
/// installed.
pub fn set_headers_hook(hook: impl Fn(&mut dyn FnMut(&str, &str)) + Send + Sync + 'static) -> bool {
    HOOK.set(Box::new(hook)).is_ok()
}

/// Add the hook's headers to a request.
pub(crate) fn apply(rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
    let Some(hook) = HOOK.get() else {
        return rb;
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    hook(&mut |k, v| headers.push((k.to_string(), v.to_string())));
    headers.into_iter().fold(rb, |rb, (k, v)| rb.header(k, v))
}
