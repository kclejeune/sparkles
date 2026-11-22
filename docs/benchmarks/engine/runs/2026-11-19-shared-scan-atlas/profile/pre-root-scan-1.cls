total 1150329
region other: 1146974
region Operator>::next_shared: 2778
region next_batch_at_most: 577
== leaves of Operator>::next_shared
   1692 sparkles_core::sparql::exec::block_passes
    424 <sparkles_core::sparql::cursor::Operator>::next_shared
     78 __vdso_clock_gettime
     71 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     69 <sparkles_core::index::BlockCache>::get_cols_cached
     49 <sparkles_core::index::Block as core::clone::Clone>::clone
     47 <sparkles_core::sparql::ctx::OwnedCharge>::new
     46 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     42 <sparkles_core::index::Block>::key_range
     40 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     38 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     35 __memmove_avx_unaligned_erms
     33 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     27 sparkles_core::sparql::exec::scan_mask
     21 <sparkles_core::index::PermIndex>::key_block_range
     16 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::Block>>
     15 <sparkles_core::sparql::ctx::Ctx>::check
     10 mi_malloc
      9 <std::sys::pal::unix::time::Timespec>::now
      8 clock_gettime@@GLIBC_2.17
      4 <std::sys::pal::unix::time::Timespec>::sub_timespec
      2 <sparkles_core::sparql::ctx::Ctx>::nvars
      1 alloc::boxed::box_new_uninit
      1 _mi_malloc_generic
== leaves of Scan>::next
== leaves of next_batch_at_most
    128 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     66 mi_malloc
     59 __vdso_clock_gettime
     45 <sparkles_core::sparql::cursor::Operator>::update_plan
     38 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     29 <sparkles_core::sparql::ctx::Ctx>::check
     26 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
     24 mi_free
     24 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
     22 <sparkles_core::sparql::ctx::OwnedCharge>::new
     13 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     11 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
     10 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      9 rust_psm_stack_pointer
      8 <std::sys::pal::unix::time::Timespec>::now
      8 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      7 __memmove_avx_unaligned_erms
      6 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      6 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      6 clock_gettime@@GLIBC_2.17
      6 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      4 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      4 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      4 _mi_malloc_generic
      3 <sparkles_core::sparql::cursor::QueryCursor>::terminate
