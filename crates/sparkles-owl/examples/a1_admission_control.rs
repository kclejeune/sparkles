//! Frozen R4 diagnostic. Timing uses the server's direct, uninstrumented allocator.
#[global_allocator]
static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;

mod a1_r4;

struct Uninstrumented;
impl a1_r4::Meter for Uninstrumented {
    const NAME: &'static str = "uninstrumented-mimalloc";
    fn begin() {}
    fn snapshot() -> serde_json::Value { serde_json::Value::Null }
    fn end() -> serde_json::Value { serde_json::Value::Null }
}

fn main() { a1_r4::main::<Uninstrumented>(); }
