| query | base ms | tip/base | 95% interval | added µs | tip-second ratio | tip-first ratio | same-build ratio | gate |
|---|--:|--:|---|--:|--:|--:|--:|---|
| q-ef-128 | 0.2276 | 1.0069 | 1.0023-1.0115 | 1.58 | 1.0110 | 0.9972 |  | pass |
| q-ef-16 | 0.0563 | 1.0162 | 1.0099-1.0302 | 0.91 | 1.0302 | 0.9976 |  | FAIL |
| q-ef-256 | 0.5681 | 1.0056 | 1.0011-1.0093 | 3.19 | 1.0040 | 0.9928 |  | pass |
| q-ef-32 | 0.0753 | 1.0132 | 1.0083-1.0216 | 0.99 | 1.0256 | 0.9991 |  | FAIL |
| q-ef-64 | 0.1116 | 1.0114 | 1.0040-1.0158 | 1.27 | 1.0241 | 1.0012 |  | pass |
| q-exact-true | 10.7966 | 1.0003 | 0.9999-1.0007 | 3.75 | 0.9999 | 0.9992 |  | pass |

FAIL: q-ef-16: tip/base 1.0162 up to 1.0302, 0.91 µs added
FAIL: q-ef-32: tip/base 1.0132 up to 1.0216, 0.99 µs added
the latency gate fails
