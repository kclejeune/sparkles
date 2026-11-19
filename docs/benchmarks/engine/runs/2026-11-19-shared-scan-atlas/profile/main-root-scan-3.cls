total 873925
region other: 870444
region Operator>::next_shared: 3103
region next_batch_at_most: 378
== leaves of Operator>::next_shared
   2072 sparkles_core::sparql::exec::block_passes
    179 <sparkles_core::sparql::cursor::Operator>::next_shared
    133 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
    116 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
    105 <sparkles_core::index::Block>::key_range
     95 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     81 __vdso_clock_gettime
     53 <sparkles_core::index::BlockCache>::get_cols_cached
     43 <sparkles_core::index::Block>::downgrade
     34 <sparkles_core::sparql::ctx::OwnedCharge>::new
     33 __memmove_avx_unaligned_erms
     18 mi_malloc
     17 <sparkles_core::index::PermIndex>::key_block_range
     17 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     15 <sparkles_core::sparql::ctx::Ctx>::check
     15 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     13 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
     11 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
      9 <sparkles_core::sparql::cursor::Scan>::reads_base
      9 clock_gettime@@GLIBC_2.17
      8 <sparkles_core::sparql::cursor::BaseScan>::unpark
      8 alloc::boxed::box_new_uninit
      5 <std::sys::pal::unix::time::Timespec>::now
      4 <sparkles_core::sparql::cursor::BaseScan>::remember_block
      4 <sparkles_core::sparql::cursor::BaseScan>::park
== leaves of Scan>::next
== leaves of next_batch_at_most
     65 __vdso_clock_gettime
     60 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     55 mi_malloc
     39 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     19 <sparkles_core::sparql::cursor::Operator>::update_plan
     18 <sparkles_core::sparql::ctx::Ctx>::check
     17 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     16 mi_free
     12 <sparkles_core::sparql::ctx::OwnedCharge>::new
     10 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      8 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      8 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      6 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      5 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      4 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      4 <std::sys::pal::unix::time::Timespec>::sub_timespec
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      4 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      3 rust_psm_stack_pointer
      2 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      2 clock_gettime@@GLIBC_2.17
      2 _mi_malloc_generic
      2 <sparkles_core::sparql::cursor::QueryCursor>::pull
      2 <std::sys::pal::unix::time::Timespec>::now
