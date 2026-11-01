#!/usr/bin/env python3
"""Check local Markdown links and anchors on the public documentation entry pages.

This deliberately checks only inline links on these pages. It skips fenced examples and
external URLs. The source-path check covers a wider set of documents separately.
"""

from collections import Counter
from pathlib import Path
import re
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parent.parent
PAGES = (
    'README.md',
    'docs/README.md',
    'docs/GETTING-STARTED.md',
    'docs/FEATURES.md',
    'docs/BENCHMARKS.md',
    'docs/ROADMAP.md',
    'docs/COMPARISON.md',
)
LINK = re.compile(r'\[[^\]]+\]\(([^\s)]+)(?:\s+"[^"]*")?\)')
IMAGE = re.compile(r'<img\s+[^>]*src=["\']([^"\']+)')
FENCE = re.compile(r'^\s*(```|~~~)')


def anchors(path):
    """Collect explicit anchors and GitHub-style heading slugs."""
    contents = path.read_text()
    found = set(re.findall(r'<a\s+(?:id|name)=["\']([^"\']+)', contents))
    counts = Counter()
    in_fence = False
    for line in contents.splitlines():
        if FENCE.match(line):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        match = re.match(r'^#{1,6}\s+(.+?)\s*#*\s*$', line)
        if not match:
            continue
        heading = re.sub(r'<[^>]*>', '', match.group(1))
        heading = re.sub(r'\[([^]]+)\]\([^)]+\)', r'\1', heading).lower()
        slug = re.sub(r'[^\w\- ]', '', heading).replace(' ', '-')
        occurrence = counts[slug]
        counts[slug] += 1
        found.add(f'{slug}-{occurrence}' if occurrence else slug)
    return found


def check():
    errors = []
    anchor_cache = {}
    for name in PAGES:
        source = ROOT / name
        in_fence = False
        for number, line in enumerate(source.read_text().splitlines(), 1):
            if FENCE.match(line):
                in_fence = not in_fence
                continue
            if in_fence:
                continue
            targets = [m.group(1) for m in LINK.finditer(line)]
            targets.extend(m.group(1) for m in IMAGE.finditer(line))
            for target in targets:
                url = urlsplit(target)
                if url.scheme or url.netloc:
                    continue
                path = (source.parent / unquote(url.path)).resolve() if url.path else source
                if path.is_dir():
                    path /= 'README.md'
                if not path.exists():
                    errors.append(f'{name}:{number}: missing {target}')
                    continue
                if url.fragment and path.suffix.lower() == '.md':
                    if path not in anchor_cache:
                        anchor_cache[path] = anchors(path)
                    if unquote(url.fragment) not in anchor_cache[path]:
                        errors.append(f'{name}:{number}: missing anchor {target}')
    return errors


if __name__ == '__main__':
    errors = check()
    if errors:
        raise SystemExit('\n'.join(errors))
    print('Checked local links and anchors on public documentation entry pages.')
