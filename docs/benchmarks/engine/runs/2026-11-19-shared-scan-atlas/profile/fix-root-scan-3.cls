total 778519
region other: 775424
region Operator>::next_shared: 2798
region next_batch_at_most: 297
== leaves of Operator>::next_shared
   2044 sparkles_core::sparql::exec::block_passes
    130 <sparkles_core::sparql::cursor::Operator>::next_shared
    113 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     95 <sparkles_core::index::Block>::key_range
     84 __vdso_clock_gettime
     79 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     73 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     30 <sparkles_core::index::BlockCache>::get_cols_cached
     23 mi_malloc
     16 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     14 <sparkles_core::sparql::cursor::Scan>::reads_base
     14 clock_gettime@@GLIBC_2.17
     12 <sparkles_core::sparql::ctx::OwnedCharge>::new
     10 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
      9 <sparkles_core::index::PermIndex>::key_block_range
      8 <std::sys::pal::unix::time::Timespec>::now
      7 alloc::boxed::box_new_uninit
      6 <std::sys::pal::unix::time::Timespec>::sub_timespec
      6 <sparkles_core::sparql::cursor::BaseScan>::unpark
      5 <sparkles_core::sparql::ctx::Ctx>::check
      4 _mi_malloc_generic
      4 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
      4 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      3 <sparkles_core::sparql::cursor::BaseScan>::clear_block
      2 <sparkles_core::sparql::cursor::BaseScan>::park
== leaves of Scan>::next
== leaves of next_batch_at_most
     49 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     43 __vdso_clock_gettime
     41 mi_malloc
     22 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     22 mi_free
     19 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     16 <sparkles_core::sparql::cursor::Operator>::update_plan
     11 <sparkles_core::sparql::ctx::OwnedCharge>::new
     10 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      8 <sparkles_core::sparql::ctx::Ctx>::check
      6 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      5 clock_gettime@@GLIBC_2.17
      5 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      4 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      4 rust_psm_stack_pointer
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      3 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      3 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      3 <std::sys::pal::unix::time::Timespec>::now
      2 core::ptr::drop_glue::<sparkles_core::sparql::cursor::BaseScan>
      2 __memmove_avx_unaligned_erms
      2 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      2 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      2 _mi_malloc_generic
