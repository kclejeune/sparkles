"""Dataset replacement and writer-lock operations through catalog handles."""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

import pytest
import sparkles
from sparkles import Catalog


@pytest.mark.parametrize("kind", ["mem", "persistent"])
def test_recreated_alias_returns_the_current_dataset(tmp_path: Path, kind: str) -> None:
    with Catalog(tmp_path / "catalog") as cat:
        old = cat.create("wiki", kind=kind)
        old.update("INSERT DATA { <urn:old> <urn:p> 1 }")
        old_id = old.dataset_id
        assert cat.delete("wiki")
        current = cat.create("wiki", kind=kind)
        assert current is not old
        assert current.dataset_id == cat.info("wiki").id != old_id
        assert cat["wiki"] is current
        assert cat.get_by_id(current.dataset_id) is current
        assert cat.get_by_id(old_id) is None
        assert len(current) == 0
        current.update("INSERT DATA { <urn:new> <urn:p> 2 }")
        assert current.ask("ASK { <urn:new> <urn:p> 2 }")
        assert not current.ask("ASK { <urn:old> ?p ?o }")
        assert len(old) == 1
    assert old.closed and current.closed


def test_renamed_alias_does_not_shadow_a_replacement() -> None:
    with Catalog.memory() as cat:
        old = cat.create("wiki", kind="mem")
        old.update("INSERT DATA { <urn:old> <urn:p> 1 }")
        renamed = cat.rename("wiki", "docs")
        current = cat.create("wiki", kind="mem")
        assert cat["docs"] is renamed
        assert cat["wiki"] is current
        assert renamed.dataset_id == old.dataset_id != current.dataset_id
        assert len(renamed) == 1 and len(current) == 0


# The child deadline makes a missing lock guard a failed test rather than a hung
# test runner. It also exercises the native extension through ordinary Python calls.
LOCK_CALL = r'''
import sys
from pathlib import Path
import sparkles
from sparkles import Catalog, ConflictError

path = Path(sys.argv[1])
operation, kind, locked = sys.argv[2:]
with Catalog(path / "catalog") as cat:
    ds = cat.create("wiki", kind=kind)
    ds.update("INSERT DATA { <urn:base> <urn:p> 1 }")
    first = ds.head_commit.seq
    if kind == "persistent":
        ds.branches.create("work")
        work = ds.branch("work")
        work.update("INSERT DATA { <urn:work> <urn:p> 2 }")
        work_commit = work.head_commit.seq
    if operation.startswith("backup"):
        cat.repositories.add({"name": "local", "type": "fs", "path": str(path / "repo")})
        repo = cat.repositories.open("local")
        backups = ds.backups(repo)
        policy = {"name": "test", "repository": "local", "datasets": ["wi*"], "schedule": "every 1h"}
    calls = {
        "clone": lambda: ds.clone_to(path / "copy"),
        "memory_clone": lambda: ds.clone_to_memory(),
        "catalog_clone": lambda: cat.clone_dataset("wiki", "copy", kind="mem"),
    }
    if kind == "persistent":
        calls.update({
            "preview_merge": lambda: ds.branches.preview_merge("work"),
            "preview_revert": lambda: ds.branches.preview_revert(first),
            "preview_cherry_pick": lambda: ds.branches.preview_cherry_pick("work", work_commit),
            "preview_revert_work": lambda: ds.branches.preview_revert(work_commit, branch="work"),
        })
    if operation.startswith("backup"):
        calls.update({
            "backup_create": lambda: backups.create("one"),
            "backup_policy": lambda: backups.run_policy(policy),
            "backup_catalog_policy": lambda: cat.run_policy(policy),
        })
    owner = work if locked == "work" else cat.get_by_id(ds.dataset_id)
    with owner.transaction() as tx:
        tx.update("INSERT DATA { <urn:pending> <urn:p> 3 }")
        # Snapshot reads remain usable; only operations needing this writer lock fail.
        assert owner.ask("ASK { <urn:base> <urn:p> 1 }")
        try:
            calls[operation]()
        except ConflictError as error:
            assert "open transaction" in str(error)
        else:
            raise AssertionError(operation + " did not reject the open transaction")
        assert cat.info("copy") is None
        assert tx.query("ASK { <urn:pending> <urn:p> 3 }") is True
    assert owner.ask("ASK { <urn:pending> <urn:p> 3 }")
    calls[operation]()
'''


@pytest.mark.parametrize(
    "operation,kind,locked",
    [
        (operation, kind, "main")
        for kind in ("mem", "persistent")
        for operation in ("clone", "memory_clone", "catalog_clone", "backup_create", "backup_policy", "backup_catalog_policy")
    ]
    + [
        ("preview_merge", "persistent", "main"),
        ("preview_merge", "persistent", "work"),
        ("preview_revert", "persistent", "main"),
        ("preview_revert_work", "persistent", "work"),
        ("preview_cherry_pick", "persistent", "main"),
        ("preview_cherry_pick", "persistent", "work"),
    ],
)
def test_captures_reject_the_transaction_owner(
    tmp_path: Path, operation: str, kind: str, locked: str
) -> None:
    if operation.startswith("backup") and "backup" not in sparkles.FEATURES:
        pytest.skip("built without backup")
    result = subprocess.run(
        [sys.executable, "-c", LOCK_CALL, str(tmp_path), operation, kind, locked],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr
