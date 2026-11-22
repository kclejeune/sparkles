total 798755
region other: 795817
region Operator>::next_shared: 2608
region next_batch_at_most: 330
== leaves of Operator>::next_shared
   1780 sparkles_core::sparql::exec::block_passes
    211 <sparkles_core::sparql::cursor::Operator>::next_shared
    157 <sparkles_core::index::Block>::key_range
     78 __vdso_clock_gettime
     76 <quick_cache::sync::Cache<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter>>::get::<(u64, u32, u32)>
     73 <quick_cache::shard::CacheShard<(u64, u32, u32), sparkles_core::index::Col, sparkles_core::index::BlockWeighter, quick_cache::DefaultHashBuilder, quic
     55 <sparkles_core::index::BlockCache>::get_cols_cached
     31 __memmove_avx_unaligned_erms
     28 <sparkles_core::sparql::ctx::OwnedCharge>::new
     21 <sparkles_core::sparql::cursor::Scan>::next_shared::{closure#2}
     21 <sparkles_core::index::Block as core::clone::Clone>::clone
     17 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
     12 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
     10 sparkles_core::sparql::exec::scan_mask
      8 <sparkles_core::sparql::ctx::Ctx>::check
      7 <sparkles_core::index::PermIndex>::key_block_range
      5 <std::sys::pal::unix::time::Timespec>::now
      4 <std::sys::pal::unix::time::Timespec>::sub_timespec
      4 core::ptr::drop_glue::<core::option::Option<sparkles_core::index::Block>>
      4 clock_gettime@@GLIBC_2.17
      3 <sparkles_core::sparql::ctx::Ctx>::nvars
      2 mi_malloc
      1 alloc::boxed::box_new_uninit
== leaves of Scan>::next
== leaves of next_batch_at_most
     54 mi_malloc
     53 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
     46 __vdso_clock_gettime
     24 <sparkles_core::sparql::cursor::QueryCursor>::publish_batch
     18 mi_free
     16 <sparkles_core::sparql::cursor::Operator>::update_plan
     16 <sparkles_core::sparql::ctx::OwnedCharge>::new
      9 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
      8 rust_psm_stack_pointer
      8 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
      7 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
      7 <sparkles_core::sparql::ctx::Ctx>::check
      7 core::ptr::drop_glue::<sparkles_core::sparql::cursor::Operator>
      6 _mi_malloc_generic
      6 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanInfo>
      6 <sparkles_core::sparql::cursor::QueryCursor>::terminate::stopped
      6 <stacker::STACK_LIMIT::{K#0}::{closure#1} as core::ops::function::FnOnce<(core::option::Option<&mut core::option::Option<core::cell::Cell<core::option
      5 <alloc::vec::Vec<sparkles_core::sparql::ctx::PlanWarning> as core::clone::Clone>::clone
      4 core::ptr::drop_glue::<sparkles_core::sparql::cursor::State>
      3 __memmove_avx_unaligned_erms
      3 core::ptr::drop_glue::<sparkles_core::sparql::plan::Node>
      3 core::ptr::drop_glue::<sparkles_core::sparql::exec::PlanRefs>
      2 <sparkles_core::sparql::ctx::Ctx>::release_decode_envelope
      2 <sparkles_core::sparql::query_regex::RegexCache>::clear
      2 <std::sys::pal::unix::time::Timespec>::sub_timespec
