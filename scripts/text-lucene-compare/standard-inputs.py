#!/usr/bin/env python3
"""Writes crates/sparkles-core/tests/data/lucene-analyzers/standard-tokenizer.in, the lines
on which crates/sparkles-core checks its tokenizer against Lucene's StandardTokenizer.

For the first and last code point of every range of standard_tables.rs, a few lines put
the character between letters, digits, Hebrew letters and Katakana, doubled and alone.
Seeded random lines mix characters of every class the grammar treats differently, emoji
sequences included, and long lines cross the 255 code unit limit of a word.
tokens.sh writes the expected tokens (standard-tokenizer.out).

    scripts/text-lucene-compare/standard-inputs.py
"""

import os
import random
import re

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TABLES = os.path.join(ROOT, "crates/sparkles-core/src/text/standard_tables.rs")
OUT = os.path.join(ROOT, "crates/sparkles-core/tests/data/lucene-analyzers/standard-tokenizer.in")

CONTEXTS = ["a{c}b", "{c}", "1{c}2", "{c}{c}", "א{c}ב", "カ{c}カ", "{c}\U0001F600"]
POOL = list("aZé1_.,;:'\"-#* אב") + [
    "́", "‍", "️", "︎", "⃣", "­", "⁠", "\U0001F600",
    "\U0001F44D", "\U0001F3FD", "\U0001F468", "\U0001F469", "\U0001F1EB", "\U0001F1F7",
    "❤", "©", "™", "\U000E0067", "\U000E007F", "\U0001F3F4", "カ",
    "ー", "あ", "東", "한", "ภ", "า", "ক", "্",
    "\U00010400", "々", "ｶ", "ﾞ", "٠", "٫", "’", "·",
]
LONG = list("abcxyz019_.'́‍") + ["\U00010400", "א", '"', "カ", ",", "\U0001F600"]


def main():
    ranges = re.findall(r"\(0x([0-9A-F]+), 0x([0-9A-F]+), 0x[0-9A-F]+\)", open(TABLES).read())
    cps = sorted({int(x, 16) for r in ranges for x in r} - {0x0A, 0x0D})
    rng = random.Random(29)
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        for cp in cps:
            c = chr(cp)
            for t in CONTEXTS:
                f.write(t.replace("{c}", c) + "\n")
        for _ in range(3000):
            f.write("".join(rng.choice(POOL) for _ in range(rng.randint(1, 25))) + "\n")
        for _ in range(30):
            f.write("".join(rng.choice(LONG) for _ in range(rng.randint(240, 600))) + "\n")
    print(f"{OUT}: {len(cps) * len(CONTEXTS) + 3030} lines")


if __name__ == "__main__":
    main()
