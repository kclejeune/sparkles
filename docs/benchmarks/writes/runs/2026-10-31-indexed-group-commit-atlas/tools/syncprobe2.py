#!/usr/bin/env python3
"""Per-operation latency and device flushes of commit primitives on two preallocated files.
Modes: buf1 (pwrite+fdatasync one file), buf2 (both files, fdatasync in two threads),
dir1 (O_DIRECT|O_DSYNC pwrite one file), dir2 (both files in two threads).
Usage: syncprobe2.py DIR N DEV"""
import mmap, os, statistics, sys, threading, time
d, n, dev = sys.argv[1], int(sys.argv[2]), sys.argv[3]
size = 64 << 20
def flushes():
    for line in open("/proc/diskstats"):
        f = line.split()
        if f[2] == dev:
            return int(f[7]), int(f[18]), int(f[19])
def prep(path):
    fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_TRUNC, 0o644)
    z = b"\0" * (1 << 20)
    for i in range(size >> 20):
        os.pwrite(fd, z, i << 20)
    os.fsync(fd); os.close(fd)
paths = [os.path.join(d, f"probe-{i}") for i in range(2)]
for p in paths: prep(p)
buf = mmap.mmap(-1, 4096); buf.write(b"x" * 4096)
bfd = [os.open(p, os.O_RDWR) for p in paths]
dfd = [os.open(p, os.O_RDWR | os.O_DIRECT | os.O_DSYNC) for p in paths]
def run(name, op):
    xs = []
    w0, f0, m0 = flushes()
    for i in range(n):
        off = (i * 4096) % size
        t = time.perf_counter(); op(off); xs.append(time.perf_counter() - t)
    w1, f1, m1 = flushes()
    xs.sort()
    print(f"{name}: p50={statistics.median(xs)*1e3:.3f} p90={xs[int(.9*n)]*1e3:.3f} mean={statistics.mean(xs)*1e3:.3f} ms writes/op={(w1-w0)/n:.2f} flushes/op={(f1-f0)/n:.2f} flushms/op={(m1-m0)/n:.3f}", flush=True)
def par(fs):
    ts = [threading.Thread(target=f) for f in fs[1:]]
    for t in ts: t.start()
    fs[0]()
    for t in ts: t.join()
def buf1(off): os.pwrite(bfd[0], buf, off); os.fdatasync(bfd[0])
def buf2(off):
    os.pwrite(bfd[0], buf, off); os.pwrite(bfd[1], buf, off)
    par([lambda: os.fdatasync(bfd[0]), lambda: os.fdatasync(bfd[1])])
import ctypes
libc = ctypes.CDLL("libc.so.6", use_errno=True)
libc.sync_file_range.argtypes = [ctypes.c_int, ctypes.c_longlong, ctypes.c_longlong, ctypes.c_uint]
SFR = 1 | 2 | 4
def sfr(fd, off):
    if libc.sync_file_range(fd, off, 4096, SFR) != 0:
        raise OSError(ctypes.get_errno(), "sync_file_range")
def sfr2(off):
    os.pwrite(bfd[0], buf, off); os.pwrite(bfd[1], buf, off)
    par([lambda: sfr(bfd[0], off), lambda: sfr(bfd[1], off)])
    os.fdatasync(bfd[0])
def sfr2s(off):
    os.pwrite(bfd[0], buf, off); os.pwrite(bfd[1], buf, off)
    sfr(bfd[1], off)
    os.fdatasync(bfd[0])
def dir1(off): os.pwrite(dfd[0], buf, off)
def dir2(off): par([lambda: os.pwrite(dfd[0], buf, off), lambda: os.pwrite(dfd[1], buf, off)])
for _ in range(int(sys.argv[4]) if len(sys.argv) > 4 else 2):
    for name, op in (("buf1", buf1), ("buf2", buf2), ("dir1", dir1), ("dir2", dir2), ("sfr2", sfr2), ("sfr2s", sfr2s)):
        run(name, op)
for p in paths: os.unlink(p)
