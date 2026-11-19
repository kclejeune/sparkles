total 868111
region other: 864677
region Operator>::next_shared: 3089
region next_batch_at_most: 345
== leaves of Operator>::next_shared
   1945 sparkles_core::sparql::exec::block_passes
    199 <sparkles_core::sparql::cursor::Operator>::next_shared
    180 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
    156 <sparkles_core::index::Block>::key_range
    100 __vdso_clock_gettime
     86 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     66 <sparkles_core::index::Block>::downgrade
     49 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     48 <sparkles_core::index::BlockCache>::get_cols_cached
     43 mi_malloc
     31 __memmove_avx_unaligned_erms
     24 <sparkles_core::sparql::ctx::OwnedCharge>::new
     19 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     19 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     17 <sparkles_core::index::PermIndex>::key_block_range
     17 alloc::boxed::box_new_uninit
     14 <sparkles_core::sparql::cursor::Scan>::next_shared_base::{closure#0}
     13 <sparkles_core::sparql::ctx::Ctx>::check
     11 <std::sys::pal::unix::time::Timespec>::sub_timespec
     10 <sparkles_core::sparql::cursor::BaseScan>::unpark
      9 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::WeakBlock>>
      7 <sparkles_core::sparql::cursor::Scan>::reads_base
      6 <std::sys::pal::unix::time::Timespec>::now
      6 clock_gettime@@GLIBC_2.17
      5 <sparkles_core::sparql::cursor::BaseScan>::remember_block
== leaves of Scan>::next
== leaves of next_batch_at_most
     76 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     57 mi_malloc
     41 __vdso_clock_gettime
     26 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     17 mi_free
     14 <sparkles_core::sparql::ctx::OwnedCharge>::new
     14 <sparkles_core::sparql::cursor::Operator>::update_plan
     13 <sparkles_core::sparql::ctx::Ctx>::check
     13 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
      9 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      7 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      6 core::ptr::drop_glue::<core::option::Option<alloc::boxed::Box<sparkles_core::sparql::cursor::Ahead>>>
      6 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      6 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      5 rust_psm_stack_pointer
      5 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      4 <std::sys::pal::unix::time::Timespec>::now
      4 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      4 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      3 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      3 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      3 clock_gettime@@GLIBC_2.17
      2 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      2 <sparkles_core::sparql::cursor::QueryCursor>::terminate
      2 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
