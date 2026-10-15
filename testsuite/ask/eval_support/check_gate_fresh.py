#!/usr/bin/env python3
"""Run only credential-free, finite loopback fixture checks with the retained CLI."""

import argparse
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from cli_checks import run_fresh


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--output", required=True)
    ap.add_argument("--sparkles", required=True)
    ap.add_argument("--fake-upstream-only", required=True, action="store_true")
    args = ap.parse_args()
    run_fresh(args.output, args.sparkles)


if __name__ == "__main__":
    main()
