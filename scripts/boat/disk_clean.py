#!/usr/bin/env python3
"""Free a warm worker's disk without touching live work.

The provider emails the maintainer when a worker's root disk reaches 90% use, and a stop
snapshots whatever the disk holds into the next resume. This program runs on the worker.
With --if-above PCT it does nothing while the disk holding --home is below PCT% used. With
--force it always cleans, as the closure watch and a finish do before a stop.

A cleanup removes the finished gate and lane CI worktrees ~/gate/wt-NAME, whose unit
gate-NAME is not active, and then runs `mbx gc` with the job environment's store settings,
which keeps the targets and store entries that running commands use. It never removes a
path whose unit it cannot show is inactive, never follows a link and never touches the
store directly. One cleanup runs at a time under ~/gate/.disk-clean.lock, and a gate start
holds that lock from before it creates its worktree until its unit runs, so the unit list
that a cleanup reads stays true until it has removed what it chose.

The remote build checkouts under ~/lanes/nsc-build-work are left alone. A build moves its
sources there before its scope is listed, and their managed targets are in the store,
which `mbx gc` bounds.

--deadline-seconds bounds the whole cleanup. Each command, the removals included, runs in
its own process group, which is killed when the command's share of the deadline ends, and
no step starts after the deadline. SIGTERM, SIGHUP or SIGINT kill the running command's
group and end the program, and each command dies with the program even if it is killed
outright, so no process of the cleanup outlives it by more than a few seconds.

It prints one JSON record with the disk use before and after, and --log appends the same
record to a file.
"""
import argparse
import fcntl
import json
import os
import re
import signal
import stat
import subprocess
import time

# The module's docstring is not a docstring when the controller sends this file on stdin
# after its argv line, so the usage line is a constant.
DESCRIPTION = "Free a warm worker's disk without touching live work."
ACTIVE_STATES = 'active,activating,deactivating,reloading'
NAME = re.compile(r'[A-Za-z0-9][A-Za-z0-9._-]{0,99}')


def usage(path, statvfs=os.statvfs):
    """Disk use of the filesystem holding path, with df's rounding of the percentage."""
    fs = statvfs(path)
    used = (fs.f_blocks - fs.f_bfree) * fs.f_frsize
    avail = fs.f_bavail * fs.f_frsize
    whole = used + avail
    pct = 0 if whole == 0 else -(-used * 100 // whole)
    return {'used_pct': pct, 'used_bytes': used, 'avail_bytes': avail, 'size_bytes': fs.f_blocks * fs.f_frsize}


# The process group of the command that runs now, which a cancellation kills.
RUNNING = set()


def prctl(*args):
    import ctypes
    return ctypes.CDLL(None, use_errno=True).prctl(*args)


def die_with(parent):
    """The child's setup before exec: die with this program, or not start at all.

    PR_SET_PDEATHSIG makes the kernel kill the command when this program dies, even by
    SIGKILL. A program that died between the fork and that call is no longer the parent,
    so the child then exits before it runs the command, as it does when the call fails."""
    def setup():
        try:
            armed = prctl(1, signal.SIGKILL, 0, 0, 0) == 0
        except (OSError, AttributeError):
            armed = False
        if not armed or os.getppid() != parent:
            os._exit(125)
    return setup


def kill_group(pid):
    # A group of 0 or 1 would be this program's own or init's, never a command's.
    if pid <= 1:
        return
    try:
        os.killpg(pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def run_command(argv, env=None, seconds=60):
    """Run argv in a new process group for at most seconds, and kill the group after that.

    The group is also killed when this program is cancelled, and the command itself dies
    with this program, so no command outlives it."""
    if seconds <= 0:
        return 'no_time', ''
    try:
        p = subprocess.Popen(argv, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, start_new_session=True, preexec_fn=die_with(os.getpid()))
    except OSError:
        return 'unavailable', ''
    RUNNING.add(p.pid)
    try:
        out, _ = p.communicate(timeout=seconds)
        return p.returncode, out.decode(errors='replace')
    except subprocess.TimeoutExpired:
        kill_group(p.pid)
        p.communicate()
        return 'timeout', ''
    finally:
        kill_group(p.pid)
        RUNNING.discard(p.pid)


def cancel(signum, frame):
    """End the cleanup on SIGTERM, SIGHUP or SIGINT, killing the command that runs."""
    for pid in list(RUNNING):
        kill_group(pid)
    os._exit(128 + signum)


def active_units(run):
    """The names of the user's active units, or None when systemd cannot list them."""
    code, text = run(['systemctl', '--user', 'list-units', '--all', '--plain', '--no-legend',
                      '--state=' + ACTIVE_STATES], None, 15)
    if code != 0:
        return None
    return {line.split()[0] for line in text.splitlines() if line.split()}


def plain_directory(path):
    try:
        return stat.S_ISDIR(os.lstat(path).st_mode)
    except OSError:
        return False


def remove_tree(path, run, seconds):
    """Remove path with rm in a bounded, owned process. rm never follows a link it meets."""
    run(['rm', '-rf', '--one-file-system', '--', path], None, seconds)
    return not os.path.lexists(path)


def candidates(home, units):
    """The finished jobs' directories, each with the reason it may go."""
    found = []
    if units is None:
        return found
    gate = os.path.join(home, 'gate')
    if plain_directory(gate):
        for entry in sorted(os.listdir(gate)):
            name = entry[3:]
            path = os.path.join(gate, entry)
            if entry.startswith('wt-') and NAME.fullmatch(name) and plain_directory(path):
                if not any(u in units for u in ('gate-' + name + '.service', 'gate-' + name + '.scope')):
                    found.append(('gate-worktree', path))
    return found


def clean(home, threshold=None, force=False, env=None, deadline_seconds=600, run=run_command,
          statvfs=os.statvfs, now=time.time, clock=time.monotonic):
    end = clock() + deadline_seconds
    left = lambda cap: max(0, min(cap, end - clock()))
    before = usage(home, statvfs)
    record = {'schema': 1, 'observed_unix': now(), 'home': home, 'mode': 'force' if force else 'if-above',
              'threshold_pct': threshold, 'before': before, 'ran': False, 'removed': [], 'gc_exit': None}
    if not force and before['used_pct'] < threshold:
        record['after'] = before
        return record
    gate = os.path.join(home, 'gate')
    os.makedirs(gate, exist_ok=True)
    lock = os.open(os.path.join(gate, '.disk-clean.lock'), os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            record['after'] = before
            record['skipped'] = 'another cleanup holds the lock'
            return record
        record['ran'] = True
        units = active_units(lambda argv, e, cap: run(argv, e, left(cap)))
        record['units_listed'] = units is not None
        repo = os.path.join(gate, 'repo.git')
        for kind, path in candidates(home, units):
            if left(1) <= 0:
                record['deadline_reached'] = True
                break
            if remove_tree(path, run, left(deadline_seconds)):
                record['removed'].append({'kind': kind, 'path': path})
        if plain_directory(repo):
            run(['git', '-C', repo, 'worktree', 'prune'], None, left(60))
        gc_env = dict(os.environ)
        gc_env.update(env or {})
        record['gc_exit'], _ = run(['mbx', 'gc'], gc_env, left(deadline_seconds))
    finally:
        os.close(lock)
    record['after'] = usage(home, statvfs)
    return record


def main(argv=None):
    parser = argparse.ArgumentParser(description=DESCRIPTION)
    parser.add_argument('--home', default=os.path.expanduser('~'))
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument('--if-above', type=int, metavar='PCT')
    mode.add_argument('--force', action='store_true')
    parser.add_argument('--env', default='{}', help='a JSON object of environment settings for mbx gc')
    parser.add_argument('--deadline-seconds', type=int, default=600)
    parser.add_argument('--log')
    a = parser.parse_args(argv)
    if a.if_above is not None and not 1 <= a.if_above <= 100:
        parser.error('--if-above takes a percentage from 1 to 100')
    env = json.loads(a.env)
    if not isinstance(env, dict) or not all(isinstance(k, str) and isinstance(v, str) for k, v in env.items()):
        parser.error('--env takes a JSON object of strings')
    if not 1 <= a.deadline_seconds <= 3600:
        parser.error('--deadline-seconds takes 1 to 3600')
    for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(signum, cancel)
    record = clean(a.home, a.if_above, a.force, env, a.deadline_seconds)
    line = json.dumps(record, sort_keys=True)
    if a.log and record['ran']:
        with open(a.log, 'a') as f:
            f.write(line + '\n')
    print(line)


if __name__ == '__main__':
    main()
