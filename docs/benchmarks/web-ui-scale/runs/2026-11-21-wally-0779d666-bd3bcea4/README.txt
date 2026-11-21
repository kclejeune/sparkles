The ontology summary route and its head, before and after roadmap item 735.

Host: wally, a 12th Gen Intel Core i7-12800H with 20 threads, 31 GiB of memory and Linux
7.3.1, under ~/lanes/.heavy.lock with no other job of the lane on it. The run ended at
2026-11-21T21:13:19Z.

Binaries: release builds made on wally. The base arm is 0779d666 (sha256
81898fffebd69e62c34b12a5cde50d83c77ad619f3ef3b541a7e95b91a4e6847) and the tip arm is
bd3bcea4 (sha256 a83933aff93e01033524c723c6f3a618f11f3fc9bf1804449dfaeb9d6753f710).

Method: measure-job.sh runs measure.sh, whose comment naming atlas is older than this run, with three repetitions. Each dataset is loaded once.
For each repetition, dataset and arm a fresh server starts on the dataset. The base arm
times two calls of GET /$/ontology/{ds}. The tip arm times two calls with view=head and then
one full call. VmRSS and VmHWM of the server are read at the start and after each call, in
KiB. Times are curl's time_total in seconds.

Correctness: raw/*-diff.txt are empty and runs.txt ends with one line per dataset saying
that the head equals the full summary and that the tip's full summary equals the base's,
comparing the head's dataset, commit, mode, graphs, imports, warnings, header, entities and
closure with the full summary's, and the two full answers without their cached member.

Files: runs.txt holds one line per server, inputs.sha256 the checksums of the three input
files, which equal those of the 2026-11-04 run, and raw/*.json the answers of the last
repetition.
