total 1195134
region other: 1191585
region Operator>::next_shared: 2891
region next_batch_at_most: 658
== leaves of Operator>::next_shared
   1595 sparkles_core::sparql::exec::block_passes
    356 <sparkles_core::sparql::cursor::Operator>::next_shared
    165 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
    106 __vdso_clock_gettime
     78 <sparkles_core::index::Block>::key_range
     64 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     58 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     48 <sparkles_core::index::BlockCache>::get_cols_cached
     46 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     43 <sparkles_core::sparql::ctx::OwnedCharge>::new
     41 __memmove_avx_unaligned_erms
     39 <sparkles_core::index::Block>::downgrade
     37 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     35 <sparkles_core::sparql::cursor::Scan>::reads_base
     35 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
     31 mi_malloc
     17 <sparkles_core::index::PermIndex>::key_block_range
     17 clock_gettime@@GLIBC_2.17
     16 <sparkles_core::sparql::cursor::BaseScan>::park
     10 <sparkles_core::sparql::cursor::BaseScan>::clear_block
     10 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
     10 <sparkles_core::sparql::cursor::BaseScan>::remember_block
      9 alloc::boxed::box_new_uninit
      8 <sparkles_core::sparql::cursor::BaseScan>::unpark
      6 <std::sys::pal::unix::time::Timespec>::now
== leaves of Scan>::next
== leaves of next_batch_at_most
    147 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     67 <sparkles_core::sparql::cursor::Operator>::update_plan
     59 __vdso_clock_gettime
     51 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     49 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
     45 mi_malloc
     42 rust_psm_stack_pointer
     32 <sparkles_core::sparql::ctx::Ctx>::check
     27 mi_free
     17 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
     16 <sparkles_core::sparql::ctx::OwnedCharge>::new
     15 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     13 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     11 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      8 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      8 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      7 clock_gettime@@GLIBC_2.17
      7 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      5 <std::sys::pal::unix::time::Timespec>::now
      5 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      5 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      4 _mi_malloc_generic
      3 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      3 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      2 <std::sys::pal::unix::time::Timespec>::sub_timespec
