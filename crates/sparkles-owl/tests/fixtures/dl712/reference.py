#!/usr/bin/env python3
"""reference.py HERMIT KONCLUDE FILE...: classifies each `NAME.ofn` with HermiT's
`hermit-classify` into `NAME.hermit.ofn` and with Konclude into `NAME.konclude.ofn`, in the
same functional-style form of SubClassOf and EquivalentClasses axioms between named
classes. HermiT 1.4.5.519 and Konclude 0.7.0-1138 made the files of this directory."""
import os
import subprocess
import sys
import tempfile
import xml.etree.ElementTree as ET

OWL = "{http://www.w3.org/2002/07/owl#}"


def konclude(binary, src, dst):
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, "out.owx")
        subprocess.run([binary, "classification", "-i", src, "-o", out],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        root = ET.parse(out).getroot()
    lines = []
    for el in root:
        tag = el.tag.removeprefix(OWL)
        iris = [c.get("IRI") for c in el if c.tag == f"{OWL}Class"]
        if tag in ("SubClassOf", "EquivalentClasses") and len(iris) == len(el) >= 2:
            lines.append(f"{tag}({' '.join(f'<{i}>' for i in iris)})")
        elif tag == "Declaration" and iris:
            lines.append(f"Declaration(Class(<{iris[0]}>))")
    with open(dst, "w") as f:
        f.write("\n".join(sorted(lines)) + "\n")


def main():
    hermit, konc = sys.argv[1], sys.argv[2]
    for src in sys.argv[3:]:
        stem = src.removesuffix(".ofn")
        subprocess.run([hermit, "--taxonomy", f"{stem}.hermit.ofn", src],
                       check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        konclude(konc, src, f"{stem}.konclude.ofn")


if __name__ == "__main__":
    main()
