total 737934
region other: 735117
region Operator>::next_shared: 2539
region next_batch_at_most: 278
== leaves of Operator>::next_shared
   1765 sparkles_core::sparql::exec::block_passes
    138 <sparkles_core::index::Block>::key_range
     97 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     93 <sparkles_core::sparql::cursor::Operator>::next_shared
     89 __vdso_clock_gettime
     77 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     75 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     38 <sparkles_core::index::BlockCache>::get_cols_cached
     30 mi_malloc
     27 <sparkles_core::sparql::ctx::OwnedCharge>::new
     17 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     14 alloc::boxed::box_new_uninit
     13 <sparkles_core::index::PermIndex>::key_block_range
     10 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
      9 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      8 clock_gettime@@GLIBC_2.17
      7 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
      6 <sparkles_core::sparql::ctx::Ctx>::check
      5 <sparkles_core::sparql::cursor::Scan>::reads_base
      4 <sparkles_core::sparql::cursor::BaseScan>::set_block
      4 <sparkles_core::sparql::cursor::BaseScan>::unpark
      4 <std::sys::pal::unix::time::Timespec>::now
      3 __memmove_avx_unaligned_erms
      3 <std::sys::pal::unix::time::Timespec>::sub_timespec
      2 <sparkles_core::sparql::cursor::BaseScan>::clear_block
== leaves of Scan>::next
== leaves of next_batch_at_most
     47 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     47 __vdso_clock_gettime
     37 mi_malloc
     29 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     20 mi_free
     15 <sparkles_core::sparql::ctx::OwnedCharge>::new
     10 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
      8 <sparkles_core::sparql::cursor::Operator>::update_plan
      8 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      7 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      6 <sparkles_core::sparql::ctx::Ctx>::check
      4 <std::sys::pal::unix::time::Timespec>::now
      4 __memmove_avx_unaligned_erms
      4 <sparkles_core::sparql::query_regex::RegexCache>::clear
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      4 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      3 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      3 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      3 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      2 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      2 core::ptr::drop_glue::<sparkles_core::sparql::plan::ScanSpec>
      2 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      2 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      2 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      2 rust_psm_stack_pointer
