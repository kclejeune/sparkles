#!/usr/bin/env python3
"""Run one complete BSBM driver cell under a wall deadline and record its outcome.

The deadline is a resource guard for the complete warm-up and measurement series. It
does not shorten that series into a throughput sample. Each child runs in a separate
process group so a driver that ignores termination cannot hold up later cells.
"""

import argparse
import importlib.util
import math
import os
from pathlib import Path
import signal
import subprocess


def positive_seconds(value):
    try:
        seconds = float(value)
    except ValueError:
        raise argparse.ArgumentTypeError("must be a positive finite number of seconds") from None
    if not math.isfinite(seconds) or seconds <= 0:
        raise argparse.ArgumentTypeError("must be a positive finite number of seconds")
    return seconds


def signal_group(process, sig):
    try:
        os.killpg(process.pid, sig)
    except ProcessLookupError:
        pass


def terminate(process, grace):
    # Defer another outer stop until the group has been killed and reaped. A signal
    # handler raising during the grace wait must not skip the final SIGKILL.
    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM, signal.SIGINT})
    try:
        signal_group(process, signal.SIGTERM)
        try:
            process.wait(timeout=grace)
        except subprocess.TimeoutExpired:
            pass
        # The leader can exit on TERM while one of its children keeps running.
        signal_group(process, signal.SIGKILL)
        return process.wait(timeout=grace)
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


def run(command, cwd, timeout, grace):
    process = subprocess.Popen(command, cwd=cwd, start_new_session=True)
    try:
        return process.wait(timeout=timeout), False
    except subprocess.TimeoutExpired:
        try:
            code = terminate(process, grace)
        except subprocess.TimeoutExpired:
            # Even an OS task that cannot be reaped after KILL cannot block the suite.
            code = -signal.SIGKILL
        return code, True
    except BaseException:
        terminate(process, grace)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--timeout", type=positive_seconds, required=True)
    parser.add_argument("--grace", type=positive_seconds, required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--xml", required=True)
    parser.add_argument("--out", required=True)
    parser.add_argument("--engine", required=True)
    parser.add_argument("--clients", required=True)
    parser.add_argument("--name", required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("a driver command is required")

    def interrupted(signum, _frame):
        raise SystemExit(128 + signum)

    # An outer suite deadline must also clean up the driver's separate process group.
    signal.signal(signal.SIGTERM, interrupted)

    # -I leaves the script directory off sys.path. Load only our sibling collector.
    spec = importlib.util.spec_from_file_location("bsbm_qmph", Path(__file__).with_name("qmph.py"))
    qmph = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(qmph)
    timed_out = False
    try:
        code, timed_out = run(command, args.cwd, args.timeout, args.grace)
        if timed_out:
            reason = f"the whole driver exceeded its {args.timeout:g} second wall budget"
        elif code:
            reason = f"the driver exited with status {code}"
        else:
            reason = None
    except OSError as error:
        code = 1
        reason = f"the driver could not run: {error}"
    qmph.add(args.xml, args.out, args.engine, args.clients, args.name, code,
             reason, timed_out=timed_out)


if __name__ == "__main__":
    main()
