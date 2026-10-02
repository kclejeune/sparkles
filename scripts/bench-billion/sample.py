#!/usr/bin/env python3
"""Keep the N-Triples lines (stdin to stdout) whose subject falls into a fixed sample.

    sample.py <fraction>

A subject is in the sample when the CRC-32 of its N-Triples form is below <fraction> of
2^32. The choice depends on the subject alone, so a sample keeps every triple of the
subjects it holds, in every file, and the sample of a smaller fraction is contained in
that of a larger one.
"""

import sys
import zlib


def main():
    threshold = int(float(sys.argv[1]) * 2**32)
    out = sys.stdout.buffer
    crc = zlib.crc32
    for line in sys.stdin.buffer:
        end = line.find(b" ")
        if end > 0 and crc(line[:end]) < threshold:
            out.write(line)


if __name__ == "__main__":
    main()
