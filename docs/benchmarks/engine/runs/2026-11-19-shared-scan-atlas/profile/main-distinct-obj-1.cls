total 586360
region other: 559536
region next_batch_at_most: 25406
region Operator>::next_shared: 1418
== leaves of Operator>::next_shared
    653 __vdso_clock_gettime
    508 <sparkles_core::sparql::cursor::Operator>::next_shared
    100 clock_gettime@@GLIBC_2.17
     71 <sparkles_core::sparql::ctx::Ctx>::check
     61 <std::sys::pal::unix::time::Timespec>::now
     25 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   2433 mi_free
   2356 __vdso_clock_gettime
   2220 <sparkles_core::sparql::cursor::Operator>::next_inner
   1270 mi_malloc
   1055 __memmove_avx_unaligned_erms
    971 sparkles_core::sparql::exec::execute_node
    859 sparkles_core::sparql::exec::column_rows
    702 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    696 <sparkles_core::sparql::cursor::Operator>::next
    574 <sparkles_core::sparql::ctx::OwnedCharge>::new
    488 sparkles_core::sparql::exec::names
    487 sparkles_core::sparql::exec::execute_owned_inner
    449 sparkles_core::sparql::expr::eval
    401 <sparkles_core::sparql::ctx::Ctx>::check
    396 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    327 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    325 <sparkles_core::sparql::cursor::Buffer>::project
    321 clock_gettime@@GLIBC_2.17
    309 <sparkles_core::sparql::table::Table>::project
    278 <sparkles_core::sparql::cursor::QueryCursor>::pull
    269 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
    267 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::admit
    259 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::release
    254 mi_zalloc
    245 _mi_theap_realloc_zero
