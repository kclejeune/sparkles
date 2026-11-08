#!/usr/bin/env python3
"""Rebuild or check the maintained source seal. This grants no worker authority."""
from pathlib import Path
import argparse
import hashlib
import json
import stat


def body(root):
    members = [p for p in root.iterdir() if p.suffix == '.py' or p.name == 'README.md' or p.name.endswith('.FALSE.json')]
    files = {}
    for path in sorted(members):
        info = path.lstat()
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            raise ValueError('source member is not a singleton regular file')
        raw = path.read_bytes()
        files[path.name] = {'bytes':len(raw), 'mode':stat.S_IMODE(info.st_mode), 'sha256':hashlib.sha256(raw).hexdigest()}
    return (json.dumps({'schema':1, 'source_contract':'explicit owned-worker/profile/release; no implicit provider authority', 'files':files},indent=2,sort_keys=True)+'\n').encode()


def main():
    parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--check',action='store_true');args=parser.parse_args()
    root=Path(__file__).resolve().parent;value=body(root);path=root/'runtime-sources.json'
    if args.check:
        if path.read_bytes()!=value:raise SystemExit('runtime source seal differs; rebuild before independent review')
    else:path.write_bytes(value)
    print(hashlib.sha256(value).hexdigest())
if __name__=='__main__':main()
