#!/usr/bin/env python3
"""x11-iri-encoding.py GEN_DIR: what X11 Phase 2b's `--iri-encoding auto` would encode in a
front-coded generation, and how much smaller vocab.dat and vocab.off would be.

Follows spec §4.2: prefixes up to the last non-digit of IRIs that end in digits, those
covering at least 1% of the distinct IRIs (at most 255), and an IRI is encoded when its
number is canonical (no leading zero unless it is 0) and below 2^52. The remaining keys
are front-coded again in blocks of 16 to give the new size."""
import collections, struct, sys

d = sys.argv[1]
data = open(d + '/vocab.dat', 'rb').read()
off = open(d + '/vocab.off', 'rb').read()
count = struct.unpack_from('<Q', off, len(off) - 8)[0]
nb = len(off) // 8 - 1


def varint(p):
    v = s = 0
    while True:
        b = data[p]; p += 1
        v |= (b & 0x7f) << s
        if b < 0x80:
            return v, p
        s += 7


def vlen(v):
    n = 1
    while v >= 0x80:
        v >>= 7; n += 1
    return n


def keys():
    for b in range(nb):
        p = struct.unpack_from('<Q', off, b * 8)[0]
        prev = b''
        for i in range(min(16, count - b * 16)):
            sh, p = varint(p)
            ln, p = varint(p)
            k = prev[:sh] + data[p:p + ln]
            p += ln
            prev = k
            yield k


def split(k):
    # IRI key `<...`: (prefix, digits) when it ends in digits
    j = len(k)
    while j > 1 and 48 <= k[j - 1] <= 57:
        j -= 1
    if j == len(k):
        return None
    return k[:j], k[j:]


iris = 0
by_prefix = collections.Counter()
for k in keys():
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
    return int(num) < 1 << 52


# both sizes come from the same simulation of the writer, so they compare like for like
def simulate(keep):
    size = 0; n = 0; prev = b''
    for k in keys():
        if not keep(k):
            continue
        sh = 0
        if n % 16:
            m = min(len(prev), len(k))
            while sh < m and prev[sh] == k[sh]:
                sh += 1
        size += vlen(sh) + vlen(len(k) - sh) + len(k) - sh
        prev = k; n += 1
    return size, n


before, nb_keys = simulate(lambda k: True)
after, na_keys = simulate(lambda k: not encoded(k))
off_before = 8 * ((nb_keys + 15) // 16 + 1)
off_after = 8 * ((na_keys + 15) // 16 + 1)
print(f'{d}: {count} keys, {iris} IRIs, actual vocab.dat {len(data)} (simulated {before})')
print(f'prefixes in the table: {len(table)}')
for p in table[:12]:
    print(f'  {by_prefix[p] / iris * 100:6.2f}%  {p.decode("utf-8", "replace")}')
print(f'encoded IRIs: {nb_keys - na_keys} ({(nb_keys - na_keys) / max(iris, 1) * 100:.1f}% of IRIs)')
print(f'vocab.dat {before} -> {after} ({(1 - after / before) * 100:.1f}% smaller); '
      f'with vocab.off {before + off_before} -> {after + off_after} '
      f'({(1 - (after + off_after) / (before + off_before)) * 100:.1f}% smaller)')
