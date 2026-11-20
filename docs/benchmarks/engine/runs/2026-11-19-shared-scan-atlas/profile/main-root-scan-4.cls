total 827435
region other: 824134
region Operator>::next_shared: 2949
region next_batch_at_most: 352
== leaves of Operator>::next_shared
   2027 sparkles_core::sparql::exec::block_passes
    131 <sparkles_core::sparql::cursor::Operator>::next_shared
    110 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
    110 <sparkles_core::index::Block>::key_range
     99 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     96 __vdso_clock_gettime
     93 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     45 <sparkles_core::index::BlockCache>::get_cols_cached
     37 <sparkles_core::sparql::ctx::OwnedCharge>::new
     29 <sparkles_core::index::Block>::downgrade
     28 mi_malloc
     21 __memmove_avx_unaligned_erms
     21 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     14 <sparkles_core::sparql::ctx::Ctx>::check
     12 <sparkles_core::index::PermIndex>::key_block_range
     11 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
      9 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      8 alloc::boxed::box_new_uninit
      7 <sparkles_core::sparql::cursor::BaseScan>::park
      7 <std::sys::pal::unix::time::Timespec>::now
      6 clock_gettime@@GLIBC_2.17
      5 <std::sys::pal::unix::time::Timespec>::sub_timespec
      5 <sparkles_core::sparql::cursor::Scan>::reads_base
      5 <sparkles_core::sparql::cursor::BaseScan>::unpark
      5 <sparkles_core::sparql::cursor::BaseScan>::clear_block
== leaves of Scan>::next
== leaves of next_batch_at_most
     72 mi_malloc
     52 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     34 __vdso_clock_gettime
     26 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     23 mi_free
     21 <sparkles_core::sparql::cursor::Operator>::update_plan
     14 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     13 <sparkles_core::sparql::ctx::OwnedCharge>::new
     10 <sparkles_core::sparql::ctx::Ctx>::check
      8 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      8 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      7 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      6 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      6 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      6 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      5 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      5 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      5 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      4 <sparkles_core::sparql::cursor::QueryCursor>::pull
      4 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      4 rust_psm_stack_pointer
      3 <std::sys::pal::unix::time::Timespec>::now
      3 clock_gettime@@GLIBC_2.17
      2 _mi_malloc_generic
