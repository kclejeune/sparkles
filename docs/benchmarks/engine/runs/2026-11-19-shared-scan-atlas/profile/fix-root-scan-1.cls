total 1031024
region other: 1028105
region Operator>::next_shared: 2481
region next_batch_at_most: 438
== leaves of Operator>::next_shared
   1592 sparkles_core::sparql::exec::block_passes
    214 <sparkles_core::sparql::cursor::Operator>::next_shared
    104 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     96 <sparkles_core::index::Block>::key_range
     87 __vdso_clock_gettime
     79 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     48 <sparkles_core::index::BlockCache>::get_cols_cached
     42 <sparkles_core::sparql::ctx::OwnedCharge>::new
     39 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     27 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     20 <sparkles_core::index::PermIndex>::key_block_range
     18 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
     17 <sparkles_core::sparql::cursor::Scan>::reads_base
     15 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     15 mi_malloc
     12 alloc::boxed::box_new_uninit
     11 <sparkles_core::sparql::cursor::BaseScan>::unpark
      9 <std::sys::pal::unix::time::Timespec>::now
      8 <sparkles_core::sparql::cursor::BaseScan>::park
      7 clock_gettime@@GLIBC_2.17
      6 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
      5 <sparkles_core::sparql::cursor::BaseScan>::clear_block
      5 <sparkles_core::sparql::ctx::Ctx>::check
      3 <std::sys::pal::unix::time::Timespec>::sub_timespec
      1 __memmove_avx_unaligned_erms
== leaves of Scan>::next
== leaves of next_batch_at_most
     66 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     50 mi_malloc
     49 __vdso_clock_gettime
     40 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     27 rust_psm_stack_pointer
     26 mi_free
     24 <sparkles_core::sparql::ctx::OwnedCharge>::new
     23 <sparkles_core::sparql::cursor::Operator>::update_plan
     16 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
     16 <sparkles_core::sparql::ctx::Ctx>::check
     14 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     10 clock_gettime@@GLIBC_2.17
      7 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      7 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      7 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      6 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      6 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      5 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      5 <sparkles_core::sparql::query_regex::RegexCache>::clear
      5 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      4 <std::sys::pal::unix::time::Timespec>::now
      4 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      4 __memmove_avx_unaligned_erms
      3 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      3 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
