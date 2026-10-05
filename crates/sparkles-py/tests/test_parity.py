"""Every shipped Python binding decision resolves through the extension stub."""

import ast

import sparkles._sparkles as native

import pytest
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 remains supported.
    tomllib = pytest.importorskip("tomli", reason="Python 3.10 source parity needs tomli")

ROOT = Path(__file__).resolve().parents[3]
STUB = ROOT / "crates/sparkles-py/python/sparkles/_sparkles.pyi"
BINDINGS = ROOT / "crates/sparkles/bindings.toml"


def _return_class(annotation: ast.expr | None) -> str | None:
    if isinstance(annotation, ast.Name):
        return annotation.id
    if isinstance(annotation, ast.Constant) and isinstance(annotation.value, str):
        return annotation.value
    return None


def _resolve(tree: ast.Module, name: str) -> None:
    classes = {node.name: node for node in tree.body if isinstance(node, ast.ClassDef)}
    scope = tree.body
    native_scope = native
    parts = name.split(".")
    for index, part in enumerate(parts):
        node = next((node for node in scope if getattr(node, "name", None) == part), None)
        assert hasattr(native_scope, part), f"native binding has no `{part}` in `{native_scope}`"
        assert node is not None, f"no member `{part}` in `{'.'.join(parts[:index]) or 'module'}`"
        if index == len(parts) - 1:
            assert isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))
        elif isinstance(node, ast.ClassDef):
            scope = node.body
            native_scope = getattr(native_scope, part)
        elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            result = _return_class(node.returns)
            assert result in classes, f"`{part}` has no handle class return annotation"
            scope = classes[result].body
            native_scope = getattr(native, result)
        else:
            raise AssertionError(f"`{part}` cannot contain a binding method")


def test_python_binding_names_resolve() -> None:
    if not STUB.is_file() or not BINDINGS.is_file():
        pytest.skip("source parity needs the repository stub and bindings.toml")
    tree = ast.parse(STUB.read_text())
    bindings = tomllib.loads(BINDINGS.read_text())
    for key, entry in bindings.items():
        name = entry["python"]
        assert not name.startswith("planned:"), f"`{key}` still has a planned Python binding"
        if name.startswith("skip:"):
            assert key in {"fmt.format", "fmt.lint"}, f"unexpected Python exemption: {key}"
            assert name.split(":", 1)[1].strip(), f"`{key}` needs a reason"
            continue
        try:
            _resolve(tree, name)
        except AssertionError as exc:
            raise AssertionError(
                f"bindings.toml gives `{key}` the Python name `{name}`, but _sparkles.pyi has {exc}. "
                "Add the native binding and stub member."
            ) from exc


def test_handle_property_return_annotations_are_followed() -> None:
    tree = ast.parse(
        "class Dataset:\n"
        "    @property\n"
        "    def snapshots(self) -> Snapshots: ...\n"
        "class Snapshots:\n"
        "    def create(self, name: str) -> None: ...\n"
    )
    _resolve(tree, "Dataset.snapshots.create")
    try:
        _resolve(tree, "Dataset.snapshots.missing")
    except AssertionError:
        pass
    else:
        raise AssertionError("a missing handle method must fail parity")
