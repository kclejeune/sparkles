#!/usr/bin/env python3
"""Materialize explicit version-1 input byte recipes, never matcher outcomes."""
from pathlib import Path
import argparse
import json

ROOT = Path(__file__).resolve().parent.parent


def package(name, version="1.0.0", dependencies=()):
    out = f'\n[[package]]\nname = "{name}"\nversion = "{version}"\n'
    if dependencies:
        out += "dependencies = [\n"
        out += "".join(f' "{token}",\n' for token in dependencies)
        out += "]\n"
    return out


def materialize(recipe):
    kind, p = recipe["kind"], recipe["parameters"]
    if kind == "supported-lock-padding":
        base = (ROOT / p["base"]).read_bytes()
        return base + b" " * (p["total_bytes"] - len(base))
    if kind == "package-count":
        return (
            "version = 4\n"
            + "".join(package(f"sparkles-fixture-only-bound-{i:05d}") for i in range(p["packages"]))
        ).encode()
    if kind == "dependency-count":
        leaf = "sparkles-fixture-only-bound-leaf"
        return (
            "version = 4\n"
            + package("sparkles-fixture-only-bound-root", "0.1.0", [leaf] * p["dependencies"])
            + package(leaf)
        ).encode()
    if kind == "aggregate-advisory-entries":
        package_value = {"ecosystem": "crates.io", "name": "sparkles-fixture-only-leaf"}
        value = {
            "schema_version": "1.9.2",
            "id": "x_SPARKLES_FIXTURE-0001",
            "modified": "2026-01-01T00:00:00Z",
            "affected": [
                {"package": package_value, "versions": [f"1.0.{i}" for i in range(p["first_list"])]},
                {"package": package_value, "versions": [f"2.0.{i}" for i in range(p["second_list"])]},
                {"package": package_value, "ranges": [{"type": "SEMVER", "events": [{"introduced": "0"}]}]},
            ],
        }
        return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
    if kind == "shared-hub-expansions":
        branches = [f"sparkles-fixture-only-bound-branch-{i:02d}" for i in range(p["branches"])]
        leaves = [f"sparkles-fixture-only-bound-leaf-{i:05d}" for i in range(p["leaves"])]
        root_dependencies = branches + ([leaves[0]] if p["direct_first_leaf"] else [])
        return (
            "version = 4\n"
            + package("sparkles-fixture-only-bound-root", "0.1.0", root_dependencies)
            + "".join(package(name, dependencies=["sparkles-fixture-only-bound-hub"]) for name in branches)
            + package("sparkles-fixture-only-bound-hub", dependencies=leaves)
            + "".join(package(name) for name in leaves)
        ).encode()
    raise ValueError(f"unknown recipe kind {kind!r}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("id", help="exact recipe ID in bound-recipes.json")
    parser.add_argument("--output", type=Path, required=True, help="new local file; existing paths refuse")
    args = parser.parse_args()
    recipes = json.loads((ROOT / "controls/bound-recipes.json").read_text())
    if recipes["recipe_version"] != 1:
        parser.error("unsupported recipe version")
    recipe = recipes["recipes"][args.id]
    payload = materialize(recipe)
    with args.output.open("xb") as output:
        output.write(payload)


if __name__ == "__main__":
    main()
