total 877767
region other: 874653
region Operator>::next_shared: 2788
region next_batch_at_most: 326
== leaves of Operator>::next_shared
   2035 sparkles_core::sparql::exec::block_passes
    183 <sparkles_core::sparql::cursor::Operator>::next_shared
    131 <sparkles_core::index::Block>::key_range
     83 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     80 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     60 __vdso_clock_gettime
     42 <sparkles_core::index::BlockCache>::get_cols_cached
     31 __memmove_avx_unaligned_erms
     22 <sparkles_core::index::Block as core::clone::Clone>::clone
     18 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     17 <sparkles_core::sparql::ctx::OwnedCharge>::new
     16 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     11 <sparkles_core::index::PermIndex>::key_block_range
     11 clock_gettime@@GLIBC_2.17
     11 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     11 <sparkles_core::sparql::ctx::Ctx>::check
      9 sparkles_core::sparql::exec::scan_mask
      6 <std::sys::pal::unix::time::Timespec>::now
      4 mi_malloc
      3 <sparkles_core::sparql::ctx::Ctx>::nvars
      2 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::Block>>
      1 alloc::boxed::box_new_uninit
      1 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
     57 mi_malloc
     50 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     47 __vdso_clock_gettime
     24 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     19 <sparkles_core::sparql::cursor::Operator>::update_plan
     17 <sparkles_core::sparql::ctx::OwnedCharge>::new
     13 mi_free
     12 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
      9 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      8 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      7 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      7 <sparkles_core::sparql::ctx::Ctx>::check
      6 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      6 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      4 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      4 rust_psm_stack_pointer
      4 <std::sys::pal::unix::time::Timespec>::sub_timespec
      4 __memmove_avx_unaligned_erms
      4 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      3 _mi_malloc_generic
      3 clock_gettime@@GLIBC_2.17
      2 <std::sys::pal::unix::time::Timespec>::now
      2 <sparkles_core::sparql::cursor::QueryCursor>::terminate
