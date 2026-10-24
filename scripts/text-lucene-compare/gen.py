#!/usr/bin/env python3
"""Generates the corpus and the queries of the full-text analyzer comparison.

    gen.py WORDS.json OUT_DIR

WORDS.json holds, per language, families of inflected forms, filler words and stop words.
A family is a set of words that Lucene's analyzer or Snowball's stemmer reduces to one
stem, so the queries probe exactly where the two stem differently. The words come from
the Snowball project's test vocabularies (BSD-3-Clause) for English, French, Spanish,
Italian, Portuguese, German and Russian, from the OpenTaal Dutch word list (BSD-3-Clause
or CC BY 3.0), and for Swedish from inflections written for this comparison. The stop
words are Lucene's lists. The output is
OUT_DIR/corpus.nt, 300 rdfs:label literals per language, and OUT_DIR/queries.tsv, one
query per line as LANG<TAB>KIND<TAB>QUERY. Both depend only on WORDS.json and the seed.

The query kinds are words, conjunctions of two words, words with stop words, stop words
alone, prefixes, fuzzy words, wildcards and phrases. Each query except the stop words alone
is taken from a literal, so it finds at least that literal.
"""

import json
import random
import sys

LITERALS = 300
# queries per language and kind; 1,590 in all
COUNTS = {
    "en": 191,
    "fr": 182,
    "es": 182,
    "it": 182,
    "pt": 182,
    "de": 168,
    "nl": 168,
    "ru": 168,
    "sv": 167,
}
SHARES = [
    ("word", 0.36),
    ("and", 0.15),
    ("stop", 0.08),
    ("stoponly", 0.006),
    ("prefix", 0.11),
    ("fuzzy", 0.09),
    ("wildcard", 0.08),
    ("phrase", 0.124),
]


def literal(rng, lang, words):
    fams = words["families"]
    toks = []
    content = []
    for _ in range(rng.randint(2, 4)):
        w = rng.choice(rng.choice(fams))
        content.append(w)
        toks.append(w)
    for _ in range(rng.randint(1, 3)):
        toks.insert(rng.randrange(len(toks) + 1), rng.choice(words["stops"]))
    for _ in range(rng.randint(1, 3)):
        toks.insert(rng.randrange(len(toks) + 1), rng.choice(words["fillers"]))
    out = []
    for t in toks:
        r = rng.random()
        if lang == "en" and t in content and r < 0.12:
            t += "'s"
        elif lang == "fr" and t in content and t[0] in "aeiouéh" and r < 0.3:
            t = rng.choice(["l'", "d'", "qu'"]) + t
        elif lang == "it" and t in content and t[0] in "aeiou" and r < 0.3:
            t = rng.choice(["l'", "dell'", "un'"]) + t
        out.append(t)
    text = " ".join(out)
    if rng.random() < 0.3:
        i = text.find(" ", len(text) // 2)
        if i > 0:
            text = text[:i] + "," + text[i:]
    text = text[0].upper() + text[1:]
    if rng.random() < 0.5:
        text += "."
    return text, content


def strip_elision(w):
    for sep in ("'",):
        if sep in w:
            return w.split(sep, 1)[1]
    return w


def queries(rng, lang, words, lits):
    n = COUNTS[lang]
    kinds = []
    for k, share in SHARES:
        kinds += [k] * max(1, round(n * share))
    kinds = (kinds + ["word"] * n)[:n]
    out = []
    for k in kinds:
        text, content = rng.choice(lits)
        w = rng.choice(content)
        if len(w) < 4 and k in ("prefix", "fuzzy", "wildcard"):
            # too short to cut or change
            k = "word"
        if k == "word":
            q = w
        elif k == "and":
            others = [c for c in content if c != w] or content
            q = f"+{w} +{rng.choice(others)}"
        elif k == "stop":
            q = f"{rng.choice(words['stops'])} {w}"
        elif k == "stoponly":
            q = rng.choice(words["stops"])
        elif k == "prefix":
            q = w[: max(3, min(len(w) - 1, rng.randint(3, 6)))] + "*"
        elif k == "fuzzy":
            i = rng.randrange(1, len(w))
            chars = list(w)
            chars[i] = rng.choice([c for c in "aeioulnrst" if c != chars[i]])
            q = "".join(chars) + rng.choice(["~1", "~"])
        elif k == "wildcard":
            i = rng.randrange(1, len(w) - 1)
            q = w[:i] + ("?" + w[i + 1 :] if rng.random() < 0.6 else "*" + w[-2:])
        else:
            toks = [strip_elision(t.strip(",.").lower()) for t in text.split()]
            i = rng.randrange(len(toks) - 1)
            q = '"' + " ".join(toks[i : i + 2]) + '"'
        out.append((lang, k, q))
    return out


def main():
    words = json.load(open(sys.argv[1], encoding="utf-8"))
    out = sys.argv[2]
    rng = random.Random(20261024)
    triples = []
    qs = []
    for lang in COUNTS:
        lits = [literal(rng, lang, words[lang]) for _ in range(LITERALS)]
        for i, (text, _) in enumerate(lits):
            esc = text.replace("\\", "\\\\").replace('"', '\\"')
            triples.append(
                f'<http://example.org/{lang}/{i}> '
                f'<http://www.w3.org/2000/01/rdf-schema#label> "{esc}"@{lang} .'
            )
        qs += queries(rng, lang, words[lang], lits)
    with open(f"{out}/corpus.nt", "w", encoding="utf-8") as f:
        f.write("\n".join(triples) + "\n")
    with open(f"{out}/queries.tsv", "w", encoding="utf-8") as f:
        for q in qs:
            f.write("\t".join(q) + "\n")
    print(f"{len(triples)} literals, {len(qs)} queries")


if __name__ == "__main__":
    main()
