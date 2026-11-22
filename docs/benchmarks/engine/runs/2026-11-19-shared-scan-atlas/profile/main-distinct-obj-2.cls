total 814210
region other: 778610
region next_batch_at_most: 33907
region Operator>::next_shared: 1693
== leaves of Operator>::next_shared
    706 <sparkles_core::sparql::cursor::Operator>::next_shared
    677 __vdso_clock_gettime
    111 clock_gettime@@GLIBC_2.17
     87 <std::sys::pal::unix::time::Timespec>::now
     85 <sparkles_core::sparql::ctx::Ctx>::check
     27 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   3039 <sparkles_core::sparql::cursor::Operator>::next_inner
   2725 mi_free
   2303 __vdso_clock_gettime
   1932 mi_malloc
   1622 sparkles_core::sparql::exec::execute_node
   1196 __memmove_avx_unaligned_erms
    925 <sparkles_core::sparql::cursor::Operator>::next
    881 sparkles_core::sparql::exec::column_rows
    828 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    719 sparkles_core::sparql::exec::execute_owned_inner
    620 sparkles_core::sparql::expr::eval
    587 <sparkles_core::sparql::table::Table>::project
    580 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
    557 sparkles_core::sparql::exec::names
    543 <sparkles_core::sparql::ctx::OwnedCharge>::new
    515 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    494 <sparkles_core::sparql::ctx::Ctx>::check
    475 <sparkles_core::sparql::cursor::Buffer>::project
    456 <sparkles_core::sparql::cursor::QueryCursor>::pull
    446 _mi_theap_realloc_zero
    430 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    425 sparkles_core::sparql::exec::execute_owned
    423 sparkles_core::sparql::exec::compute_column
    393 <sparkles_core::sparql::pool::PoolGrant>::cover_transient
    393 clock_gettime@@GLIBC_2.17
