total 651577
region other: 621515
region next_batch_at_most: 28556
region Operator>::next_shared: 1506
== leaves of Operator>::next_shared
    626 __vdso_clock_gettime
    474 <sparkles_core::sparql::cursor::Operator>::next_shared
    147 __memmove_avx_unaligned_erms
    104 clock_gettime@@GLIBC_2.17
     84 <sparkles_core::sparql::ctx::Ctx>::check
     47 <std::sys::pal::unix::time::Timespec>::now
     24 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   2497 mi_free
   2354 __vdso_clock_gettime
   2213 <sparkles_core::sparql::cursor::Operator>::next_inner
   2199 mi_malloc
   1245 sparkles_core::sparql::exec::execute_node
    978 <sparkles_core::sparql::cursor::Operator>::next
    960 __memmove_avx_unaligned_erms
    740 sparkles_core::sparql::exec::column_rows
    654 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    573 sparkles_core::sparql::expr::eval
    516 _mi_theap_realloc_zero
    514 <sparkles_core::sparql::ctx::OwnedCharge>::new
    499 sparkles_core::sparql::exec::names
    447 <sparkles_core::sparql::ctx::Ctx>::check
    428 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    415 sparkles_core::sparql::exec::execute_owned_inner
    369 <sparkles_core::sparql::table::Table>::project
    366 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
    362 mi_page_thread_collect_to_local
    342 <sparkles_core::sparql::cursor::QueryCursor>::pull::{closure#0}
    338 <sparkles_core::sparql::cursor::QueryCursor>::pull
    332 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    308 mi_zalloc
    306 <sparkles_core::sparql::cursor::Operator>::update_plan
    303 clock_gettime@@GLIBC_2.17
