total 800839
region other: 797699
region Operator>::next_shared: 2794
region next_batch_at_most: 346
== leaves of Operator>::next_shared
   1802 sparkles_core::sparql::exec::block_passes
    155 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
    153 <sparkles_core::sparql::cursor::Operator>::next_shared
    145 <sparkles_core::index::Block>::key_range
     92 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     85 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     68 <sparkles_core::index::BlockCache>::get_cols_cached
     63 __vdso_clock_gettime
     50 mi_malloc
     29 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     28 <sparkles_core::sparql::ctx::OwnedCharge>::new
     15 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
     15 alloc::boxed::box_new_uninit
     12 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     12 <sparkles_core::sparql::ctx::Ctx>::check
     11 <sparkles_core::index::PermIndex>::key_block_range
      9 <sparkles_core::sparql::cursor::BaseScan>::park
      9 <std::sys::pal::unix::time::Timespec>::now
      9 <sparkles_core::sparql::cursor::BaseScan>::unpark
      7 <sparkles_core::sparql::cursor::Scan>::reads_base
      6 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
      6 __memmove_avx_unaligned_erms
      5 <sparkles_core::sparql::cursor::BaseScan>::clear_block
      5 clock_gettime@@GLIBC_2.17
      2 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
     69 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     58 __vdso_clock_gettime
     48 mi_malloc
     25 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     23 <sparkles_core::sparql::cursor::Operator>::update_plan
     19 mi_free
     17 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     13 <sparkles_core::sparql::ctx::OwnedCharge>::new
      9 <sparkles_core::sparql::ctx::Ctx>::check
      8 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      7 <sparkles_core::sparql::query_regex::RegexCache>::clear
      6 rust_psm_stack_pointer
      5 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      4 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      4 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      3 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      3 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      3 clock_gettime@@GLIBC_2.17
      3 __memmove_avx_unaligned_erms
      3 <std::sys::pal::unix::time::Timespec>::sub_timespec
      3 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      2 _mi_malloc_generic
      2 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      2 <sparkles_core::sparql::cursor::QueryCursor>::terminate
