#!/usr/bin/env python3
"""Samples the write and flush counters of one block device in /proc/diskstats every
100 ms and prints one line per sample: the unix time, writes completed, milliseconds spent
writing, flushes completed and milliseconds spent flushing. Usage: dsample.py DEVICE SECONDS"""
import sys, time
dev, secs = sys.argv[1], float(sys.argv[2])
end = time.time() + secs
out = sys.stdout
while time.time() < end:
    with open("/proc/diskstats") as f:
        for line in f:
            p = line.split()
            if p[2] == dev:
                # fields: 7 writes completed, 10 ms writing, 18 flushes completed, 19 ms flushing
                out.write(f"{time.time():.3f} {p[7]} {p[10]} {p[18]} {p[19]}\n")
                break
    out.flush()
    time.sleep(0.1)
