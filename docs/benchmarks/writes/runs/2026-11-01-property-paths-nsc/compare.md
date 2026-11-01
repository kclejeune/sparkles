# incr gate: candidate against baseline

- baseline: host stanley, commit None, started None
- candidate: host stanley, commit None, started None
- threshold: 10.0% slower
- compared although different: manifest compilers: unknown in the baseline and the candidate
- compared although different: manifest build_config: unknown in the baseline and the candidate

| metric | baseline | candidate | ratio | flag |
|---|--:|--:|--:|---|
| commits/0-views/median | 0.049079 | 0.048126 | 0.981 |  |
| commits/0-views/p99 | 0.087559 | 0.087299 | 0.997 |  |
| commits/0-views/rate | 19076.8 | 19671.4 | 0.970 |  |
| commits/0-views/catch-up | 0.000641 | 0.000811 | 1.265 |  |
| commits/10-views/median | 0.048067 | 0.046482 | 0.967 |  |
| commits/10-views/p99 | 0.067921 | 0.069351 | 1.021 |  |
| commits/10-views/rate | 20080.9 | 20799.4 | 0.965 |  |
| commits/10-views/catch-up | 268.592 | 258.022 | 0.961 |  |
| commits/100-views/median | 0.046569 | 0.047524 | 1.021 |  |
| commits/100-views/p99 | 0.06555 | 0.074001 | 1.129 |  |
| commits/100-views/rate | 21122.4 | 20250.5 | 1.043 |  |
| commits/100-views/catch-up | 3502.07 | 3487.43 | 0.996 |  |
| commit-path/no-views/median | 0.047473 | 0.047011 | 0.990 |  |
| commit-path/no-views/p99 | 0.075242 | 0.068791 | 0.914 |  |
| commit-path/no-views/rate | 20170.1 | 20283.9 | 0.994 |  |
| commit-path/live-query/median | 0.05019 | 0.050223 | 1.001 |  |
| commit-path/live-query/p99 | 0.072052 | 0.068982 | 0.957 |  |
| commit-path/live-query/rate | 19341.6 | 19347.7 | 1.000 |  |
| inferred/rdfs-team-sizes/query | 17.1687 | 19.6937 | 1.147 |  |
| inferred/rdfs-team-sizes/round-median | 12.2945 | 12.8351 | 1.044 |  |
| inferred/rdfs-team-sizes/round-p95 | 15.8343 | 19.8499 | 1.254 |  |
| inferred/rdfs-team-sizes/unrelated-commit | 0.04753 | 0.047938 | 1.009 |  |
| inferred/rdfs-agents/query | 4.48345 | 4.57665 | 1.021 |  |
| inferred/rdfs-agents/round-median | 4.45064 | 4.37421 | 0.983 |  |
| inferred/rdfs-agents/round-p95 | 5.091 | 5.86407 | 1.152 |  |
| inferred/rdfs-agents/unrelated-commit | 0.054916 | 0.055453 | 1.010 |  |
| inferred/materialized-agents/query | 2.95181 | 3.05452 | 1.035 |  |
| inferred/materialized-agents/round-median | 0.004248 | 0.004308 | 1.014 |  |
| inferred/materialized-agents/round-p95 | 2.93469 | 3.14681 | 1.072 |  |
| inferred/materialized-agents/unrelated-commit | 0.009608 | 0.01075 | 1.119 |  |
| inferred/materialized-team-sizes/query | 1.46317 | 1.51823 | 1.038 |  |
| inferred/materialized-team-sizes/round-median | 1.50004 | 1.48856 | 0.992 |  |
| inferred/materialized-team-sizes/round-p95 | 1.52896 | 1.52681 | 0.999 |  |
| inferred/materialized-team-sizes/unrelated-commit | 0.011163 | 0.01063 | 0.952 |  |
| pass/team-sizes/build | 36.205 | 46.9531 | 1.297 |  |
| pass/team-sizes/query | 0.211274 | 0.277025 | 1.311 |  |
| pass/team-sizes/pass-median | 0.009915 | 0.009118 | 0.920 |  |
| pass/team-sizes/pass-p95 | 0.018418 | 0.018178 | 0.987 |  |
| pass/team-sizes/state | 0.167847 | 0.167847 | 1.000 |  |
| pass/team-ages/build | 45.4886 | 58.4481 | 1.285 |  |
| pass/team-ages/query | 3.22567 | 3.35634 | 1.041 |  |
| pass/team-ages/pass-median | 0.01616 | 0.015736 | 0.974 |  |
| pass/team-ages/pass-p95 | 0.03111 | 0.03059 | 0.983 |  |
| pass/team-ages/state | 0.378784 | 0.378784 | 1.000 |  |
| pass/colleagues-known/build | 9.32211 | 9.58904 | 1.029 |  |
| pass/colleagues-known/query | 7.94075 | 7.68636 | 0.968 |  |
| pass/colleagues-known/pass-median | 0.117063 | 0.123554 | 1.055 |  |
| pass/colleagues-known/pass-p95 | 0.199398 | 0.203602 | 1.021 |  |
| pass/colleagues-known/state | 0 | 0 | 1.000 |  |
| pass/labelled/build | 76.1962 | 82.9038 | 1.088 |  |
| pass/labelled/query | 1.12725 | 1.28243 | 1.138 |  |
| pass/labelled/pass-median | 0.006592 | 0.006623 | 1.005 |  |
| pass/labelled/pass-p95 | 0.014497 | 0.014678 | 1.012 |  |
| pass/labelled/state | 22.934 | 22.934 | 1.000 |  |
| pass/teams/build | 21.4763 | 19.8769 | 0.926 |  |
| pass/teams/query | 0.556473 | 0.552635 | 0.993 |  |
| pass/teams/pass-median | 0.0042 | 0.00414 | 0.986 |  |
| pass/teams/pass-p95 | 0.01176 | 0.011973 | 1.018 |  |
| pass/teams/state | 0.0915527 | 0.0915527 | 1.000 |  |
| breakdown/team-sizes/read | 0.28 | 0.29 | 1.036 |  |
| breakdown/team-sizes/old | 3.158 | 3.226 | 1.022 |  |
| breakdown/team-sizes/advance | 1.943 | 2.085 | 1.073 |  |
| breakdown/team-ages/read | 0.32 | 0.321 | 1.003 |  |
| breakdown/team-ages/old | 3.077 | 3.066 | 0.996 |  |
| breakdown/team-ages/advance | 3.519 | 3.829 | 1.088 |  |
| breakdown/colleagues-known/read | 0.521 | 0.501 | 0.962 |  |
| breakdown/colleagues-known/old | 4.45 | 4.399 | 0.989 |  |
| breakdown/colleagues-known/advance | 124.722 | 128.022 | 1.026 |  |
| breakdown/labelled/read | 0.401 | 0.411 | 1.025 |  |
| breakdown/labelled/old | 3.667 | 3.668 | 1.000 |  |
| breakdown/labelled/advance | 2.787 | 2.887 | 1.036 |  |
| breakdown/teams/read | 0.461 | 0.461 | 1.000 |  |
| breakdown/teams/old | 3.788 | 3.779 | 0.998 |  |
| breakdown/teams/advance | 1.924 | 1.894 | 0.984 |  |
| paths/reach-one/build | 52.5566 | 175.891 | 3.347 | slower |
| paths/reach-one/query | 19.4871 | 15.9791 | 0.820 |  |
| paths/reach-one/pass-median | 0.010904 | 0.001684 | 0.154 |  |
| paths/reach-one/pass-p95 | 59.6169 | 0.027154 | 0.000 |  |
| paths/reach-one/state | 18.3142 | 17.9071 | 0.978 |  |
| paths/unit-ancestors/build | 67.9746 | 16.4421 | 0.242 |  |
| paths/unit-ancestors/query | 43.3276 | 43.3796 | 1.001 |  |
| paths/unit-ancestors/pass-median | 0.014731 | 0.001954 | 0.133 |  |
| paths/unit-ancestors/pass-p95 | 73.1945 | 0.066681 | 0.001 |  |
| paths/unit-ancestors/state | 14.4086 | 15.953 | 1.107 | slower |
| paths/under-root/build | 2.93035 | 7.73831 | 2.641 | slower |
| paths/under-root/query | 0.714413 | 0.702424 | 0.983 |  |
| paths/under-root/pass-median | 2.17022 | 0.005872 | 0.003 |  |
| paths/under-root/pass-p95 | 2.29858 | 0.022824 | 0.010 |  |
| paths/under-root/state | 1.84204 | 2.01477 | 1.094 |  |
| paths/people-under/build | 664.269 | 522.967 | 0.787 |  |
| paths/people-under/query | 62.8622 | 61.355 | 0.976 |  |
| paths/people-under/pass-median | 0.043861 | 0.026314 | 0.600 |  |
| paths/people-under/pass-p95 | 82.1515 | 0.234906 | 0.003 |  |
| paths/people-under/state | 16.2886 | 17.79 | 1.092 |  |

94 metrics compared, 3 flagged, 0 failed checks, 0 missing checks, 0 changed fingerprints.
