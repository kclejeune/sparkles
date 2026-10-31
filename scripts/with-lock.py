#!/usr/bin/env python3
"""Run a command while holding an exclusive lock on a file, without depending on flock(1),
which macOS does not ship.

The lock is taken with flock(2) on an open file that the command inherits, so it lasts
until the command and every descendant that keeps the descriptor have exited. Processes
that close inherited descriptors, such as the Gradle daemon a JVM build starts, do not
keep it.

Usage: scripts/with-lock.py LOCK_FILE COMMAND [ARGUMENT...]
"""
import fcntl
import os
import sys

if len(sys.argv) < 3:
    sys.exit(__doc__.strip().splitlines()[-1])
path, command = sys.argv[1], sys.argv[2:]
os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
fd = os.open(path, os.O_CREAT | os.O_RDWR, 0o644)
fcntl.flock(fd, fcntl.LOCK_EX)
os.set_inheritable(fd, True)
os.execvp(command[0], command)
