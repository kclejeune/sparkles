"""Tests of scripts/x11-iri-encoding.py, the estimate of X11 Phase 2b's IRI encoding."""
import importlib.util
import struct
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location(
    'x11_iri_encoding', Path(__file__).resolve().parents[1] / 'x11-iri-encoding.py')
enc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(enc)


def write(keys):
    """vocab.dat and vocab.off of sorted keys, front-coded as VocabWriter writes them."""
    data, offs, prev = bytearray(), [], b''
    for i, k in enumerate(keys):
        sh = 0
        if i % 16 == 0:
            offs.append(len(data))
        else:
            while sh < min(len(prev), len(k)) and prev[sh] == k[sh]:
                sh += 1
        for v in (sh, len(k) - sh):
            while v >= 0x80:
                data.append(v & 0x7f | 0x80)
                v >>= 7
            data.append(v)
        data += k[sh:]
        prev = k
    off = b''.join(struct.pack('<Q', o) for o in offs) + struct.pack('<Q', len(keys))
    return bytes(data), off


class IriEncodingTest(unittest.TestCase):
    def test_the_simulation_reproduces_the_file_and_counts_encoded_iris(self):
        keys = sorted([b'"a literal'] + [b'<http://e/x/%d' % i for i in range(100)]
                      + [b'<http://e/x/007', b'<http://other/%d' % 5])
        data, off = write(keys)
        r = enc.analyze(data, off)
        self.assertEqual(r['before'], len(data))
        self.assertEqual(r['keys'], len(keys))
        # the 100 canonical numbers of the dominant prefix; 007 has a leading zero, and
        # the other prefix covers 1 of 102 IRIs, which is under 1%
        self.assertEqual(r['encoded'], 100)
        self.assertLess(r['after'], r['before'])

    def test_a_number_too_long_to_encode_stays_in_the_vocabulary(self):
        keys = sorted([b'<urn:Q' + b'1' * 5000, b'<urn:Q12', b'<urn:Q' + b'9' * 17])
        r = enc.analyze(*write(keys))
        self.assertEqual(r['encoded'], 1)

    def test_an_empty_vocabulary_reports_no_savings(self):
        data, off = write([])
        r = enc.analyze(data, off)
        self.assertEqual((r['before'], r['after'], r['encoded']), (0, 0, 0))
        self.assertEqual(enc.smaller(r['before'], r['after']), 0.0)


if __name__ == '__main__':
    unittest.main()
