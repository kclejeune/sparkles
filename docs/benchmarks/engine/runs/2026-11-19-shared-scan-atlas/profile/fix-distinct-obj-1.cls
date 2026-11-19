total 570581
region other: 543474
region next_batch_at_most: 25801
region Operator>::next_shared: 1306
== leaves of Operator>::next_shared
    617 __vdso_clock_gettime
    474 <sparkles_core::sparql::cursor::Operator>::next_shared
     70 clock_gettime@@GLIBC_2.17
     70 <sparkles_core::sparql::ctx::Ctx>::check
     46 <std::sys::pal::unix::time::Timespec>::now
     29 <std::sys::pal::unix::time::Timespec>::sub_timespec
== leaves of Scan>::next
== leaves of next_batch_at_most
   2551 __vdso_clock_gettime
   2316 mi_free
   2171 <sparkles_core::sparql::cursor::Operator>::next_inner
   1783 mi_malloc
    911 sparkles_core::sparql::exec::execute_node
    867 __memmove_avx_unaligned_erms
    863 <sparkles_core::sparql::cursor::Operator>::next
    696 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::new_owner
    652 sparkles_core::sparql::exec::column_rows
    500 sparkles_core::sparql::exec::names
    493 <sparkles_core::sparql::ctx::Ctx>::check_output_bytes
    473 sparkles_core::sparql::exec::execute_owned_inner
    464 <sparkles_core::sparql::ctx::Ctx>::check
    459 <sparkles_core::sparql::ctx::OwnedCharge>::new
    432 sparkles_core::sparql::expr::eval
    362 _mi_theap_realloc_zero
    359 <sparkles_core::sparql::table::Table>::project
    327 <sparkles_core::sparql::cursor::Buffer>::project
    311 <sparkles_core::sparql::cursor::QueryCursor>::next_batch_at_most
    304 clock_gettime@@GLIBC_2.17
    298 <sparkles_core::sparql::query_memory::QueryMemoryLedger>::release
    289 <sparkles_core::sparql::query_memory::QueryMemoryLease>::resize
    283 <sparkles_core::sparql::query_memory::Release>::finish
    280 <sparkles_core::sparql::cursor::QueryCursor>::pull
    278 <sparkles_core::sparql::cursor::QueryCursor>::pull::{closure#0}
