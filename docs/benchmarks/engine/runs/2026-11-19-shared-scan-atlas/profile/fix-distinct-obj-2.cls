total 613660
region other: 585562
region next_batch_at_most: 26736
region Operator>::next_shared: 1362
== leaves of Operator>::next_shared
    662 __vdso_clock_gettime
    491 <sparkles_core::sparql::cursor::Operator>::next_shared
     77 <sparkles_core::sparql::ctx::Ctx>::check
     65 clock_gettime@@GLIBC_2.17
     40 <std::sys::pal::unix::time::Timespec>::now
     27 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   2487 __vdso_clock_gettime
   2395 mi_free
   2240 <sparkles_core::sparql::cursor::Operator>::next_inner
   1578 mi_malloc
   1033 sparkles_core::sparql::exec::execute_node
    860 __memmove_avx_unaligned_erms
    836 <sparkles_core::sparql::cursor::Operator>::next
    737 sparkles_core::sparql::exec::column_rows
    703 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    540 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    533 sparkles_core::sparql::exec::names
    505 <sparkles_core::sparql::ctx::OwnedCharge>::new
    502 sparkles_core::sparql::expr::eval
    429 <sparkles_core::sparql::ctx::Ctx>::check
    424 <sparkles_core::sparql::table::Table>::project
    410 sparkles_core::sparql::exec::execute_owned_inner
    381 <sparkles_core::sparql::cursor::Buffer>::project
    348 _mi_theap_realloc_zero
    333 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    315 sparkles_core::sparql::exec::compute_column
    306 clock_gettime@@GLIBC_2.17
    269 <sparkles_core::sparql::cursor::QueryCursor>::pull::{closure#0}
    269 mi_zalloc
    266 <sparkles_core::sparql::cursor::QueryCursor>::pull
    263 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
