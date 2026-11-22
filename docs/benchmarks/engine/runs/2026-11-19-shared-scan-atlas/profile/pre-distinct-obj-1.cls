total 523550
region other: 499771
region next_batch_at_most: 22781
region Operator>::next_shared: 998
== leaves of Operator>::next_shared
    420 __vdso_clock_gettime
    281 <sparkles_core::sparql::cursor::Operator>::next_shared
    108 __memmove_avx_unaligned_erms
     71 clock_gettime@@GLIBC_2.17
     60 <sparkles_core::sparql::ctx::Ctx>::check
     48 <std::sys::pal::unix::time::Timespec>::now
     10 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   2154 mi_free
   2012 __vdso_clock_gettime
   1865 <sparkles_core::sparql::cursor::Operator>::next_inner
   1193 mi_malloc
   1036 __memmove_avx_unaligned_erms
    852 sparkles_core::sparql::exec::column_rows
    843 <sparkles_core::sparql::cursor::Operator>::next
    756 sparkles_core::sparql::exec::execute_node
    661 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    620 <sparkles_core::sparql::ctx::OwnedCharge>::new
    511 sparkles_core::sparql::expr::eval
    416 sparkles_core::sparql::exec::compute_column
    399 sparkles_core::sparql::exec::names
    363 <sparkles_core::sparql::ctx::Ctx>::check
    330 <sparkles_core::sparql::table::Table>::project
    326 sparkles_core::sparql::exec::execute_owned_inner
    300 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
    293 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    275 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    264 mi_zalloc
    246 core::ptr::drop_glue::<sparkles_core::sparql::exprcache::Report>
    245 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
    242 core::ptr::drop_glue::<sparkles_core::sparql::plan::Kind>
    231 <sparkles_core::sparql::cursor::QueryCursor>::pull::{closure#0}
    230 clock_gettime@@GLIBC_2.17
