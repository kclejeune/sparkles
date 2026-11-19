total 827121
region other: 823949
region Operator>::next_shared: 2813
region next_batch_at_most: 359
== leaves of Operator>::next_shared
   1965 sparkles_core::sparql::exec::block_passes
    196 <sparkles_core::sparql::cursor::Operator>::next_shared
    141 <sparkles_core::index::Block>::key_range
     84 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     82 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     78 __vdso_clock_gettime
     53 <sparkles_core::index::BlockCache>::get_cols_cached
     30 <sparkles_core::sparql::ctx::OwnedCharge>::new
     27 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     24 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     20 <sparkles_core::index::Block as core::clone::Clone>::clone
     19 <sparkles_core::sparql::ctx::Ctx>::check
     19 __memmove_avx_unaligned_erms
     13 sparkles_core::sparql::exec::scan_mask
     10 mi_malloc
      9 <std::sys::pal::unix::time::Timespec>::now
      9 clock_gettime@@GLIBC_2.17
      9 <sparkles_core::index::PermIndex>::key_block_range
      8 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::Block>>
      8 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      6 <std::sys::pal::unix::time::Timespec>::sub_timespec
      2 <sparkles_core::sparql::ctx::Ctx>::nvars
      1 alloc::boxed::box_new_uninit
== leaves of Scan>::next
== leaves of next_batch_at_most
     61 __vdso_clock_gettime
     54 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     43 mi_malloc
     25 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     22 <sparkles_core::sparql::ctx::OwnedCharge>::new
     18 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     17 <sparkles_core::sparql::cursor::Operator>::update_plan
     15 <sparkles_core::sparql::ctx::Ctx>::check
     13 mi_free
     10 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
     10 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
     10 _mi_malloc_generic
      7 __memmove_avx_unaligned_erms
      5 <std::sys::pal::unix::time::Timespec>::sub_timespec
      5 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      4 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      4 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      3 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      3 rust_psm_stack_pointer
      3 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      3 core::ptr::drop_glue::<sparkles_core::sparql::plan::ScanSpec>
      3 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      2 <std::sys::pal::unix::time::Timespec>::now
