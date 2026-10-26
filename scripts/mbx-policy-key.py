#!/usr/bin/env python3
"""Fingerprint effective non-secret mbx build policy for mise task validation."""
import hashlib
import subprocess
import sys

# Settings come from mbx's own environment/global-config resolver. Workspace
# policy is also tracked as a normal mise source input. Never print settings.
BUILD_KEYS = {
    "incremental", "learned_incremental", "eager_incremental", "seed_incremental",
    "build_script_execution", "share_out_dir", "share_workspace_root", "cache_links",
    "cc", "verify_sample_rate",
}


def policy_key():
    result = subprocess.run(["mbx", "settings", "ls"], capture_output=True, text=True)
    if result.returncode:
        raise RuntimeError("Boxington policy lookup failed; run mbx doctor for details")
    selected = []
    for line in result.stdout.splitlines():
        key = line.split(" ", 1)[0]
        if key in BUILD_KEYS or key.startswith("linker."):
            selected.append(line)
    if not selected:
        raise RuntimeError("Boxington returned no recognized build policy")
    return hashlib.sha256("\n".join(sorted(selected)).encode()).hexdigest()


if __name__ == "__main__":
    try:
        print(policy_key())
    except (OSError, RuntimeError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
