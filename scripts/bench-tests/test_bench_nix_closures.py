"""Check that every Nix package the benchmark and test scripts resolve is one the binary
cache gets.

The flake's `cacheable` output builds the packages of nix/benchmarks.nix and the tools
listed in nix/benchmark-tools.nix. A script that starts to resolve another package
through nixbin, nixexe, nixpkg, flake_build, `nixpkgs#<name>` or `$ROOT#<name>` fails
this test until the package is added to one of the two files.
"""

from pathlib import Path
import re
import unittest


ROOT = Path(__file__).resolve().parents[2]

# helper names that the resolvers' own definitions and comments use, not packages
NOT_PACKAGES = {"to", "auxiliary", "unused"}


def flake_packages():
    text = (ROOT / "nix" / "benchmarks.nix").read_text()
    names = set()
    inherit = re.search(r"inherit \(pkgs\)(.*?);", text, re.S)
    names.update(inherit.group(1).split())
    names.update(re.findall(r"^\s*([a-z0-9-]+) = ", text, re.M))
    names.update(re.findall(r"\{ ([a-z0-9-]+) = pkgs\.callPackage", text))
    return names


def tool_packages():
    text = (ROOT / "nix" / "benchmark-tools.nix").read_text()
    return set(re.findall(r'^\s*"([A-Za-z0-9_.-]+)"', text, re.M))


def resolved_packages():
    """(package, file, line) for each literal package a script resolves."""
    patterns = [
        re.compile(r"\bnix(?:bin|exe|pkg) +([A-Za-z0-9_.-]+)"),
        re.compile(r"\bflake_build +([A-Za-z0-9_.-]+)"),
        re.compile(r"nixpkgs#([A-Za-z0-9_.-]+)"),
        re.compile(r"\$\{?ROOT\}?#([A-Za-z0-9_.-]+)"),
        re.compile(r"\$root#([A-Za-z0-9_.-]+)"),
    ]
    files = sorted((ROOT / "scripts").rglob("*.sh")) + [ROOT / "mise.toml"]
    found = []
    for path in files:
        for number, line in enumerate(path.read_text().splitlines(), 1):
            if line.lstrip().startswith("#"):
                continue
            for pattern in patterns:
                for name in pattern.findall(line):
                    # `fluree.version` reads an attribute of the package
                    name = name.split(".")[0]
                    if name not in NOT_PACKAGES:
                        found.append((name, path.relative_to(ROOT), number))
    return found


class BenchNixClosureTests(unittest.TestCase):
    def test_every_resolved_package_is_cached(self):
        known = flake_packages() | tool_packages()
        missing = [
            f"{file}:{line}: {name}"
            for name, file, line in resolved_packages()
            if name not in known
        ]
        self.assertEqual(
            missing,
            [],
            "add these packages to nix/benchmark-tools.nix or nix/benchmarks.nix",
        )

    def test_the_scan_finds_the_engines_and_tools(self):
        names = {name for name, _, _ in resolved_packages()}
        for expected in ("qlever", "apache-jena-fuseki", "oha", "jdk8_headless", "watdiv-gen"):
            self.assertIn(expected, names)


if __name__ == "__main__":
    unittest.main()
