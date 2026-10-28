#!/usr/bin/env python3
"""Compute the fixture's framed key for an explicit JSON array of fields."""
import hashlib
import json
import struct
import sys


def frame(fields):
    output = b'SPKSWIR\0' + struct.pack('>II', 1, len(fields))
    for field in fields:
        if field is None:
            output += b'\0' + struct.pack('>Q', 0)
        else:
            value = field.encode('utf-8')
            output += b'\1' + struct.pack('>Q', len(value)) + value
    return output


def main():
    fields = json.loads(sys.stdin.read())
    data = frame(fields)
    print(json.dumps({'hex': data.hex(), 'key': fields[0] + ':' + hashlib.sha256(data).hexdigest()}))


if __name__ == '__main__':
    main()
