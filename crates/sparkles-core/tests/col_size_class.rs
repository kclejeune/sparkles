//! A full decoded column keeps mimalloc's 256 KiB size class under the server's allocator.
//! A column whose reference counts sit in front of its values, as `Arc<[u64]>` puts them,
//! asks for 16 bytes more and takes 320 KiB.

use sparkles_core::index::{BLOCK_ROWS, Col};

#[global_allocator]
static GLOBAL: sparkles_alloc::Mimalloc = sparkles_alloc::Mimalloc;

#[test]
fn a_full_column_keeps_its_size_class() {
    let col = Col::from(vec![7u64; BLOCK_ROWS]);
    let bytes = BLOCK_ROWS * size_of::<u64>();
    assert_eq!(bytes, 256 << 10);
    // SAFETY: a column's values are a live allocation of their own from the global
    // allocator, and the pointer is its start.
    let usable = unsafe { sparkles_alloc::usable_size(col.as_ptr().cast()) };
    assert_eq!(usable, bytes);
    let clone = col.clone();
    assert_eq!(clone.as_ptr(), col.as_ptr());
}
