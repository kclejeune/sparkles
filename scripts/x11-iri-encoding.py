#!/usr/bin/env python3
"""x11-iri-encoding.py GEN_DIR: what X11 Phase 2b's `--iri-encoding auto` would encode in
a front-coded generation, and how much smaller vocab.dat and vocab.off would be.

Follows spec §4.2: prefixes up to the last non-digit of IRIs that end in digits, those
covering at least 1% of the distinct IRIs (at most 255), and an IRI is encoded when its
number is canonical (no leading zero unless it is 0) and below 2^52. The remaining keys
are front-coded again in blocks of 16, as VocabWriter writes them, to give the new size.
Run on the keys unchanged, the same simulation gives the size of vocab.dat itself."""
import collections
import struct
import sys

BLOCK = 16
LIMIT = 1 << 52


def vlen(v):
    n = 1
    while v >= 0x80:
        v >>= 7
        n += 1
    return n


def keys(data, off):
    """Every key of a front-coded vocabulary, in order."""
    if len(off) < 8:
        return
    count = struct.unpack_from('<Q', off, len(off) - 8)[0]
    blocks = len(off) // 8 - 1

    def varint(p):
        v = s = 0
        while True:
            b = data[p]
            p += 1
            v |= (b & 0x7f) << s
            if b < 0x80:
                return v, p
            s += 7

    for b in range(blocks):
        p = struct.unpack_from('<Q', off, b * 8)[0]
        prev = b''
        for _ in range(min(BLOCK, count - b * BLOCK)):
            sh, p = varint(p)
            ln, p = varint(p)
            k = prev[:sh] + data[p:p + ln]
            p += ln
            prev = k
            yield k


def split(k):
    """(prefix, digits) of an IRI key `<...` that ends in digits, else None."""
    j = len(k)
    while j > 1 and 48 <= k[j - 1] <= 57:
        j -= 1
    if j == len(k):
        return None
    return k[:j], k[j:]


def simulate(all_keys, keep):
    """The size of vocab.dat holding the keys `keep` accepts, and their number."""
    size = n = 0
    prev = b''
    for k in all_keys():
        if not keep(k):
            continue
        sh = 0
        if n % BLOCK:
            m = min(len(prev), len(k))
            while sh < m and prev[sh] == k[sh]:
                sh += 1
        size += vlen(sh) + vlen(len(k) - sh) + len(k) - sh
        prev = k
        n += 1
    return size, n


def analyze(data, off):
    all_keys = lambda: keys(data, off)  # noqa: E731
    iris = 0
    by_prefix = collections.Counter()
    for k in all_keys():
        if k[:1] != b'<':
            continue
        iris += 1
        s = split(k)
        if s:
            by_prefix[s[0]] += 1
    table = [p for p, n in by_prefix.most_common(255) if n * 100 >= iris]
    tset = set(table)

    def encoded(k):
        if k[:1] != b'<':
            return False
        s = split(k)
        if not s or s[0] not in tset:
            return False
        num = s[1]
        if len(num) > 1 and num[:1] == b'0':
            return False
        # 2^52 has 16 digits, so a longer number is never encoded, and int() of a very
        # long one would exceed Python's conversion limit
        return len(num) <= 16 and int(num) < LIMIT

    before, nb = simulate(all_keys, lambda k: True)
    after, na = simulate(all_keys, lambda k: not encoded(k))
    return {
        'keys': nb, 'iris': iris, 'table': table, 'by_prefix': by_prefix,
        'encoded': nb - na, 'before': before, 'after': after,
        'off_before': 8 * ((nb + BLOCK - 1) // BLOCK + 1),
        'off_after': 8 * ((na + BLOCK - 1) // BLOCK + 1),
    }


def smaller(a, b):
    return 0.0 if a == 0 else (1 - b / a) * 100


def main():
    d = sys.argv[1]
    data = open(d + '/vocab.dat', 'rb').read()
    off = open(d + '/vocab.off', 'rb').read()
    r = analyze(data, off)
    print(f'{d}: {r["keys"]} keys, {r["iris"]} IRIs, actual vocab.dat {len(data)} '
          f'(simulated {r["before"]})')
    print(f'prefixes in the table: {len(r["table"])}')
    for p in r['table'][:12]:
        print(f'  {r["by_prefix"][p] / r["iris"] * 100:6.2f}%  {p.decode("utf-8", "replace")}')
    print(f'encoded IRIs: {r["encoded"]} ({r["encoded"] / max(r["iris"], 1) * 100:.1f}% of IRIs)')
    b, a = r['before'], r['after']
    bo, ao = b + r['off_before'], a + r['off_after']
    print(f'vocab.dat {b} -> {a} ({smaller(b, a):.1f}% smaller); '
          f'with vocab.off {bo} -> {ao} ({smaller(bo, ao):.1f}% smaller)')


if __name__ == '__main__':
    main()
