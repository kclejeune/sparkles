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
        if kind == "persistent":
            # a live handle could write into the new directory, so delete refuses
            with pytest.raises(sparkles.ConflictError, match="live handles"):
                cat.delete("wiki")
            old.close()
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
        if kind == "mem":
            # an in-memory dataset keeps no files, so its old handle may live on
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
            "relink": lambda: ds.branches.relink("work"),
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
        ("relink", "persistent", "main"),
        ("relink", "persistent", "work"),
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


@pytest.mark.skipif("backup" not in sparkles.FEATURES, reason="built without backup")
@pytest.mark.parametrize("kind", ["mem", "persistent"])
def test_branch_backups_restore_independent_main_and_preserve_provenance(tmp_path: Path, kind: str) -> None:
    with Catalog(tmp_path / "catalog") as cat:
        ds = cat.create("wiki", kind=kind)
        ds.update("INSERT DATA { _:base <urn:p> 1 }")
        ds.branches.create("work")
        work = ds.branch("work")
        work.update("INSERT DATA { _:branch <urn:p> 2 }")
        cat.repositories.add({"name": "local", "type": "fs", "path": str(tmp_path / "repo")})
        repo = cat.repositories.open("local")
        captured = work.backups(repo).create("branch")
        provenance = captured.to_dict()["dataset"]["branch"]
        assert provenance["datasetId"] == ds.dataset_id
        assert provenance["id"] == work.dataset_id == captured.dataset_id
        assert captured.to_dict()["dataset"]["type"] == kind
        copy = cat.restore(repo, "branch", name="copy")
        assert copy.dataset_id != work.dataset_id
        assert len(copy) == 2 and len(ds) == 1
        assert [b["name"] for b in copy.branches.list()] == ["main"]
        copy.branches.create("new")
        copy.update("INSERT DATA { <urn:copy> <urn:p> 3 }")
        assert len(work) == 2
        with pytest.raises(sparkles.BackupError, match="new dataset identity"):
            cat.restore(repo, "branch", name="duplicate", identity="keep")
        assert cat.get("duplicate") is None


def test_catalog_pool_bounds_queries() -> None:
    cross = "SELECT ?a ?b ?x ?y WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y"
    cat = Catalog.memory(total_query_memory_bytes=1 << 20)
    ds = cat.create("pooled", kind="mem")
    body = " ".join(f"<urn:s{i}> <urn:p> {i} ." for i in range(1000))
    ds.update(f"INSERT DATA {{ {body} }}")
    with pytest.raises(sparkles.BudgetExceededError) as e:
        ds.query(cross)
    assert e.value.kind == "server-memory"  # type: ignore[attr-defined]
    with pytest.raises(sparkles.BudgetExceededError) as e:
        list(ds.select_cursor(cross))
    assert e.value.kind == "server-memory"  # type: ignore[attr-defined]
    with pytest.raises(sparkles.BudgetExceededError) as e:
        ds.query(cross, at="head")
    assert e.value.kind == "server-memory"  # type: ignore[attr-defined]
    # a transaction's update takes the same pool
    tx = ds.transaction()
    with pytest.raises(sparkles.BudgetExceededError) as e:
        tx.update(
            "DELETE { ?a <urn:none> ?b } WHERE { SELECT ?a ?b WHERE { ?a <urn:p> ?x . ?b <urn:p> ?y } ORDER BY ?x ?y }"
        )
    assert e.value.kind == "server-memory"  # type: ignore[attr-defined]


# The Python caller owns the transaction logically; its Rust worker holds the
# actual writer. Each child is killed and reaped by subprocess.run on a timeout.
BRANCH_TRANSACTION_CALL = r'''
import sys
import threading
from pathlib import Path
from sparkles import Catalog, ConflictError

path = Path(sys.argv[1])
kind, operation = sys.argv[2:]
with Catalog(path / "catalog") as cat:
    ds = cat.create("wiki", kind=kind)
    other = cat.create("other", kind=kind)
    ds.branches.create("work", note="original")
    ds.branches.create("peer")
    aliases = []
    opened = threading.Barrier(3)
    def open_alias():
        opened.wait(timeout=3)
        aliases.append(ds.branch("work"))
    openers = [threading.Thread(target=open_alias, daemon=True) for _ in range(2)]
    for opener in openers:
        opener.start()
    opened.wait(timeout=3)
    for opener in openers:
        opener.join(5)
        assert not opener.is_alive()
    assert len(aliases) == 2
    work = aliases[0]
    alias = cat.get_by_id(ds.dataset_id)
    calls = {
        "note": lambda: alias.branches.update("work", note="replacement"),
        "none": lambda: alias.branches.update("work", note=None),
        "delete": lambda: alias.branches.delete("work"),
        "force": lambda: alias.branches.delete("work", force=True),
        "get": lambda: alias.branches.get("work"),
    }
    with work.transaction() as tx:
        tx.update("INSERT DATA { <urn:pending> <urn:p> 3 }")
        if operation.startswith("thread"):
            entered = threading.Event()
            done = threading.Event()
            errors = []
            def run():
                entered.set()
                try:
                    if operation == "thread-note":
                        alias.branches.update("work", note="waited")
                    else:
                        alias.branches.delete("work")
                except BaseException as error:
                    errors.append(error)
                finally:
                    done.set()
            # A daemon control cannot keep the child alive after a failing assertion;
            # the outer deadline still bounds native or interpreter shutdown waits.
            worker = threading.Thread(target=run, daemon=True)
            worker.start()
            assert entered.wait(3)
            early = done.wait(0.1)
            # Roll back pending data so ordinary deletion is safe after release.
            tx.rollback()
            assert not early, errors
            assert done.wait(5), "other thread did not finish after writer release"
            worker.join(5)
            assert not worker.is_alive() and not errors, errors
        else:
            try:
                calls[operation]()
            except ConflictError as error:
                assert "open transaction" in str(error)
            else:
                raise AssertionError(operation + " accepted the transaction owner")
            assert tx.query("ASK { <urn:pending> <urn:p> 3 }") is True
            tx.update("INSERT DATA { <urn:continued> <urn:p> 4 }")
            # Unrelated note changes and another dataset keep working.
            ds.branches.update("peer", note="allowed")
            other.branches.update("main", note="independent")
    if operation.startswith("thread"):
        if operation == "thread-note":
            assert ds.branches.get("work")["note"] == "waited"
        else:
            assert [b["name"] for b in ds.branches.list()] == ["main", "peer"]
    else:
        assert ds.branches.get("work")["note"] == "original"
        assert work.ask("ASK { <urn:continued> <urn:p> 4 }")
        if operation in ("delete", "force"):
            old_id = work.dataset_id
            ds.branches.delete("work", force=True)
            ds.branches.create("work")
            assert ds.branch("work").dataset_id != old_id
        else:
            calls[operation]()
            # Explicit None currently means omission, tracked by roadmap 755.
            expected = "replacement" if operation == "note" else "original"
            assert ds.branches.get("work")["note"] == expected
'''


@pytest.mark.parametrize("kind", ["mem", "persistent"])
@pytest.mark.parametrize("operation", ["note", "none", "delete", "force", "get", "thread-note", "thread-delete"])
def test_branch_transaction_owner(tmp_path: Path, kind: str, operation: str) -> None:
    result = subprocess.run(
        [sys.executable, "-c", BRANCH_TRANSACTION_CALL, str(tmp_path), kind, operation],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr


BROKEN_BRANCH_CALL = r'''
import shutil
import sys
from pathlib import Path
from sparkles import Catalog

root = Path(sys.argv[1])
damage = sys.argv[2]
with Catalog(root / "catalog") as cat:
    ds = cat.create("wiki", kind="persistent")
    info = ds.branches.create("broken", note="original")
    branch_path = Path(cat.info("wiki").path) / "branches" / info["id"]
    ds.close()
if damage == "missing":
    shutil.rmtree(branch_path)
with Catalog(root / "catalog") as cat:
    ds = cat.get("wiki")
    if damage == "missing":
        assert ds.branches.get("broken")["broken"] is True
        updated = ds.branches.update("broken", note="metadata only")
        assert updated["broken"] is True and updated["note"] == "metadata only"
    else:
        # Damage only after main opens, so force-delete remains available even
        # though opening this cold branch's Store will now fail.
        (branch_path / "CURRENT").write_text("not-a-generation\n")
    ds.branches.delete("broken", force=True)
    assert [branch["name"] for branch in ds.branches.list()] == ["main"]
'''


@pytest.mark.parametrize("damage", ["missing", "corrupt"])
def test_branch_transaction_guard_keeps_broken_branch_administration(tmp_path: Path, damage: str) -> None:
    result = subprocess.run(
        [sys.executable, "-c", BROKEN_BRANCH_CALL, str(tmp_path), damage],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr


CACHE_OWNER_CALL = r'''
import sys
from pathlib import Path
from sparkles import Catalog, ConflictError, Dataset, DatasetLockedError

root = Path(sys.argv[1])
mode = sys.argv[2]
if mode == "closed-parent":
    ds = Dataset.open(root / "standalone")
    ds.branches.create("work")
    work = ds.branch("work")
    tx = work.transaction()
    ds.close()
    work.close()
    try:
        Dataset.open(root / "standalone")
    except DatasetLockedError:
        pass
    else:
        raise AssertionError("reopened a main around its retained branch worker")
    tx.rollback()
    with Dataset.open(root / "standalone") as reopened:
        reopened.branches.update("work", note="released")
        assert reopened.branches.get("work")["note"] == "released"
else:
    for cycle in range(2):
        with Catalog(root / "catalog") as cat:
            ds = cat.create("wiki", kind="persistent") if cycle == 0 else cat.get("wiki")
            if cycle == 0:
                ds.branches.create("work", note="original")
            assert cat.get_by_id(ds.dataset_id) is ds
            # branch(main) constructs a distinct Python wrapper around the
            # same cached main state, unlike Catalog's deduplicated wrapper.
            alias = ds.branch("main")
            assert alias is not ds
            work = ds.branch("work")
            before = alias.branches.get("work")["note"]
            with work.transaction() as tx:
                # Closing one Python main wrapper does not discard its catalog's
                # shared main state or the live alias's cached target state.
                ds.close()
                replacement = cat.get_by_id(alias.dataset_id)
                assert replacement is not ds
                for current in (alias, replacement):
                    try:
                        current.branches.update("work", note="refused")
                    except ConflictError:
                        pass
                    else:
                        raise AssertionError("catalog alias missed the cached transaction")
                tx.update("INSERT DATA { <urn:reopened> <urn:p> 1 }")
            assert alias.branches.get("work")["note"] == before
            alias.branches.update("work", note="cycle " + str(cycle))
'''


@pytest.mark.parametrize("mode", ["closed-parent", "catalog-reopen"])
def test_branch_transaction_cache_ownership(tmp_path: Path, mode: str) -> None:
    result = subprocess.run(
        [sys.executable, "-c", CACHE_OWNER_CALL, str(tmp_path), mode],
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr
