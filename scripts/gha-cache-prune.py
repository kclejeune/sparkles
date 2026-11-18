#!/usr/bin/env python3
"""Delete superseded GitHub Actions cache entries of one ref.

The workflows save a new mbx store for every commit on main, under a key that ends in the
commit's SHA, and Swatinem/rust-cache saves a new entry whenever its environment or lock
hash changes. The older entries stay until GitHub's 10 GB repository limit evicts them,
and that eviction has removed the test build's caches. This script keeps the newest entry
of each family and deletes the others.

A family is a key with its trailing version stripped. For keys starting with mbx-store-
that is a 40-character commit SHA, and for keys starting with v0-rust- it is rust-cache's
two 8-character hashes. A key without such a suffix is never deleted. Only entries whose
key starts with one of the given prefixes and whose ref is the given ref are considered,
and the newest entry of a family is kept by its creation time, so two overlapping runs
both keep the latest commit's entry.

Usage: gha-cache-prune.py [--ref REF] [--dry-run] PREFIX...
It reads GITHUB_TOKEN, GITHUB_REPOSITORY and GITHUB_API_URL from the environment.
"""

import argparse
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

SUFFIXES = (
    ("mbx-store-", re.compile(r"-[0-9a-f]{40}$")),
    ("v0-rust-", re.compile(r"-[0-9a-f]{8}-[0-9a-f]{8}$")),
)


def family(key):
    """The key without its version suffix, or None when the key has no known suffix."""
    for prefix, suffix in SUFFIXES:
        if key.startswith(prefix):
            match = suffix.search(key)
            if match and match.start() > len(prefix):
                return key[: match.start()]
    return None


def superseded(entries, prefixes):
    """The entries to delete: every entry of a family except its newest."""
    families = {}
    for entry in entries:
        if not any(entry["key"].startswith(prefix) for prefix in prefixes):
            continue
        name = family(entry["key"])
        if name is not None:
            families.setdefault(name, []).append(entry)
    doomed = []
    for name in sorted(families):
        members = sorted(families[name], key=lambda e: (e["created_at"], e["id"]))
        doomed.extend(members[:-1])
    return doomed


class Api:
    def __init__(self, base, repository, token):
        self.base = base.rstrip("/") + f"/repos/{repository}/actions/caches"
        self.headers = {
            "Authorization": f"Bearer {token}",
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
        }

    def request(self, method, url):
        with urllib.request.urlopen(urllib.request.Request(url, method=method, headers=self.headers),
                                    timeout=30) as response:
            data = response.read()
        return json.loads(data) if data else {}

    def entries(self, ref, prefix):
        page = 1
        while True:
            query = urllib.parse.urlencode({"ref": ref, "key": prefix, "per_page": 100, "page": page})
            batch = self.request("GET", f"{self.base}?{query}")["actions_caches"]
            yield from batch
            if len(batch) < 100:
                return
            page += 1

    def delete(self, entry_id):
        self.request("DELETE", f"{self.base}/{entry_id}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("prefixes", nargs="+", metavar="PREFIX")
    parser.add_argument("--ref", default="refs/heads/main")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)
    api = Api(os.environ.get("GITHUB_API_URL", "https://api.github.com"),
              os.environ["GITHUB_REPOSITORY"], os.environ["GITHUB_TOKEN"])
    entries = {}
    for prefix in args.prefixes:
        for entry in api.entries(args.ref, prefix):
            if entry.get("ref") == args.ref:
                entries[entry["id"]] = entry
    doomed = superseded(entries.values(), args.prefixes)
    kept = len(entries) - len(doomed)
    freed = sum(entry["size_in_bytes"] for entry in doomed)
    for entry in doomed:
        verb = "would delete" if args.dry_run else "delete"
        print(f"{verb} {entry['key']} ({entry['size_in_bytes'] / 1e9:.2f} GB, {entry['created_at']})")
        if args.dry_run:
            continue
        try:
            api.delete(entry["id"])
        except urllib.error.HTTPError as error:
            # Another run may have deleted it first.
            if error.code != 404:
                print(f"::warning::could not delete {entry['key']}: HTTP {error.code}")
    print(f"{len(doomed)} superseded entries ({freed / 1e9:.2f} GB) of {len(entries)}, {kept} kept")
    return 0


if __name__ == "__main__":
    sys.exit(main())
