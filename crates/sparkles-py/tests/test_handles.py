"""P06's native handles: ownership, administration, history and controlled work."""
from __future__ import annotations

import gc
import threading
import time
from pathlib import Path

import pytest
import sparkles
from sparkles import Catalog, Dataset, CancelToken, CancelledError, CatalogLockedError, ConflictError, InvalidInputError, NotFoundError

DATA = '@prefix ex: <http://ex.org/> . ex:a a ex:Person; ex:name "Ann" .'


def test_catalog_ownership_lock_and_identity(tmp_path: Path) -> None:
    with Catalog(tmp_path / "catalog") as cat:
        ds = cat.create("wiki")
        ds.load(DATA, "turtle")
        identity = ds.dataset_id
        assert cat.get("wiki") is ds
        assert cat.get_by_id(identity) is ds
        assert cat.get("missing") is None and cat.info("missing") is None
        with pytest.raises(KeyError):
            cat["missing"]
        with pytest.raises(CatalogLockedError):
            Catalog(tmp_path / "catalog")
        assert Catalog.inspect(tmp_path / "catalog")[0].id == identity
        with pytest.raises(ConflictError):
            cat.rename("wiki", "renamed")
        handle = ds.snapshots
        ds.close()
        with pytest.raises(InvalidInputError, match="closed"):
            handle.list()
        renamed = cat.rename("wiki", "renamed")
        assert renamed.dataset_id == identity and len(renamed) == 2
        copied = cat.clone_dataset("renamed", "copy")
        assert copied.dataset_id != identity and len(copied) == 2
        ds = cat["renamed"]
        ds.branches.create("work")
        child = ds.branch("work")
        del handle, ds, renamed
        gc.collect()
        reservation = cat.reserve("pending")
    assert child.closed and copied.closed
    # The context also closes a reservation and branch after its original wrapper dies.
    with Catalog(tmp_path / "catalog") as cat:
        assert cat.info("pending") is None
        cat.delete("copy")
        assert cat.get("copy") is None


def test_native_properties_replace_and_settings(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        ds.load(DATA, "turtle")
        for old in ("create_snapshot", "delete_snapshot", "set_retention", "reason", "validate_shacl", "enable_text", "vector_index", "set_write_validation"):
            assert not hasattr(ds, old)
        assert ds.snapshots.get("missing") is None
        with pytest.raises(AttributeError):
            ds.snapshots = None
        ds.settings.quota.set(max_bytes=1 << 20)
        assert ds.settings.quota.get()["maxBytes"] == 1 << 20
        ds.settings.quota.reset()
        assert ds.settings.quota.get()["maxBytes"] is None
        ds.settings.describe.set("scbd")
        assert ds.settings.describe.get()["mode"] == "scbd"
        ds.settings.describe.reset()
        ds.set_prefix("ex", "http://ex.org/")
        assert ds.remove_prefix("ex")
        assert "ex" not in ds.prefixes
        assert ds.stats()["quads"] == 2
        assert isinstance(ds.explain("SELECT * WHERE {?s ?p ?o}")["plan"], str)
        ds.clear_cache()
        ds.replace('<http://ex.org/b> <http://ex.org/p> 1 .', format="turtle")
        assert len(ds) == 1


def test_diff_feed_schema_and_immutable_records() -> None:
    with Dataset() as ds:
        ds.settings.retention.set(keep_commits=10)
        ds.load(DATA, "turtle")
        first = ds.head_commit.seq
        snap = ds.snapshots.create("before")
        ds.load('@prefix ex: <http://ex.org/> . ex:b a ex:Book .', "turtle")
        diff = ds.history.diff(first)
        assert diff.added == 1 and diff.removed == 0
        assert len(diff) == 1 and list(diff)[0][0] == "+"
        with pytest.raises(AttributeError):
            diff.added = 99
        with pytest.raises(AttributeError):
            snap.name = "changed"
        page = ds.history.changes(first)
        assert page.next == ds.head_commit.seq
        assert len(page.commits) == 1 and list(page)[0].added == 1
        assert list(page.commits[0])[0][0] == "+"
        assert ds.history.commit(first).seq == first
        assert ds.history.commit(999) is None
        change = ds.schema.diff(first)
        assert [c["iri"] for c in change["classes"]["added"]] == ["http://ex.org/Book"]
        assert ds.schema.report()["dataset"] == "dataset"
        assert isinstance(ds.schema.void(), str)
        assert isinstance(ds.schema.draft_shapes(format="json"), dict)


def test_stored_query_parameters_and_versions() -> None:
    with Dataset() as ds:
        ds.load(DATA, "turtle")
        query = "SELECT ?name WHERE {?person <http://ex.org/name> ?name}"
        saved = ds.queries.put("by-person", query, parameters={"person": {"type": "iri", "required": True}})
        assert saved["created"]
        rows = list(ds.queries.run("by-person", {"person": "http://ex.org/a"}))
        assert rows[0]["name"].value == "Ann"
        with pytest.raises(InvalidInputError):
            ds.queries.run("by-person", {})
        with pytest.raises(ConflictError):
            ds.queries.put("by-person", query, if_version=99)
        assert ds.queries.get("by-person")["version"] == 1
        assert len(ds.queries.versions("by-person")) == 1
        assert ds.queries.delete("by-person")
        with pytest.raises(NotFoundError):
            ds.queries.run("by-person")


@pytest.mark.skipif("graphql" not in sparkles.FEATURES, reason="built without GraphQL")
def test_graphql_draft_install_execute() -> None:
    with Dataset() as ds:
        ds.load(DATA, "turtle")
        draft = ds.graphql.draft()
        ds.graphql.config.set({"sdl": draft["sdl"]})
        assert "type Person" in ds.graphql.sdl()
        assert ds.graphql.execute("{ allPerson { nodes { name } } }")["data"] == {"allPerson": {"nodes": [{"name": "Ann"}]}}
        assert len(ds.graphql.versions()) == 1
        ds.graphql.config.reset()
        assert ds.graphql.config.get() is None


@pytest.mark.skipif("backup" not in sparkles.FEATURES, reason="built without backup")
def test_backup_restore_progress_and_callback_failure(tmp_path: Path) -> None:
    repo = sparkles.BackupRepository.open((tmp_path / "repo").as_uri())
    with pytest.raises(InvalidInputError):
        repo.gc(grace=1e300)
    with Catalog(tmp_path / "catalog") as cat:
        ds = cat.create("wiki")
        ds.load(DATA, "turtle")
        seen = []
        backup = ds.backups(repo).create("one", progress=lambda f, m: seen.append(f))
        assert seen and seen == sorted(seen) and seen[-1] == 1.0
        assert backup.dataset_id == ds.dataset_id
        assert ds.backups(repo).list()[0].name == "one"
        copied = cat.restore(repo, "one", name="copy")
        assert copied.dataset_id != ds.dataset_id and len(copied) == len(ds)
        assert len(repo.backups()) == 1
        token = CancelToken()
        failure = RuntimeError("stop backup")
        def stop(f, m):
            raise failure
        with pytest.raises(RuntimeError) as caught:
            ds.backups(repo).create("cancelled", cancel=token, progress=stop)
        assert caught.value is failure and token.cancelled
        assert ds.backups(repo).get("cancelled") is None
        with pytest.raises(sparkles.BackupError) as error:
            ds.backups(repo).verify("missing")
        assert error.value.code


def test_wait_for_commit_releases_gil_and_cancels() -> None:
    with Dataset() as ds:
        def write():
            time.sleep(0.05)
            ds.load(DATA, "turtle")
        worker = threading.Thread(target=write)
        worker.start()
        assert ds.history.wait_for_commit(0, timeout=2) == 1
        worker.join()
        assert ds.history.wait_for_commit(1, timeout=0.01) is None
        token = CancelToken()
        token.cancel()
        with pytest.raises(CancelledError):
            ds.history.wait_for_commit(1, cancel=token)
        with pytest.raises(CancelledError):
            ds.schema.report(cancel=token)


def test_branches_preview_merge_and_close(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        ds.load(DATA, "turtle")
        ds.branches.create("work")
        branch = ds.branch("work")
        branch.load('<http://ex.org/b> <http://ex.org/name> "Bee" .', "turtle")
        preview = ds.branches.preview_merge("work")
        assert preview["changes"]["inserted"] == 1 and preview["commit"] is None
        merged = ds.branches.merge("work", message="reviewed")
        assert merged["merged"] and merged["commit"]["message"] == "reviewed"
        assert len(ds) == 3
        assert ds.branches.commit_graph()["commits"][0]["seq"] == ds.head_commit.seq
        with pytest.raises(ConflictError):
            ds.branches.create("work")
        with pytest.raises(NotFoundError):
            ds.branch("missing")
        ds.close()
        # A branch is a separate dataset view and remains usable until closed.
        assert len(branch) == 3
        branch.close()


def test_syntax_utilities() -> None:
    assert sparkles.check_iri("http://ex.org/a")["errors"] == []
    assert sparkles.check_langtag("en-us")["canonical"] == "en-US"
    assert sparkles.check_data("<a> <b> .", "turtle")["message"]
    assert "SELECT" in sparkles.parse_query("SELECT * WHERE {?s ?p ?o}")
    assert "INSERT" in sparkles.parse_update("INSERT DATA {<a:s> <a:p> 1}")


@pytest.mark.skipif("shacl" not in sparkles.FEATURES, reason="built without SHACL")
def test_validation_cancellation_and_callback_exception() -> None:
    ds = Dataset()
    ds.load(DATA, "turtle")
    token = CancelToken()
    token.cancel()
    with pytest.raises(CancelledError):
        ds.validation.shacl("", cancel=token)
    failure = ValueError("validator callback")
    def stop(f, m):
        raise failure
    with pytest.raises(ValueError) as error:
        ds.validation.shacl("", progress=stop)
    assert error.value is failure
    assert len(ds) == 2


def test_stored_query_progress_and_pre_cancel() -> None:
    ds = Dataset()
    ds.queries.put("all", "SELECT * WHERE {?s ?p ?o}")
    seen = []
    assert list(ds.queries.run("all", progress=lambda f, m: seen.append(f))) == []
    assert seen[0] == 0 and seen[-1] == 1
    token = CancelToken()
    token.cancel()
    with pytest.raises(CancelledError):
        ds.queries.run("all", cancel=token)


def test_branch_conflict_preview_contains_cells(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        ds.load('<urn:a> <urn:p> "base" .', "turtle")
        ds.branches.create("work")
        branch = ds.branch("work")
        ds.update('DELETE WHERE {<urn:a> <urn:p> ?o}; INSERT DATA {<urn:a> <urn:p> "ours"}')
        branch.update('DELETE WHERE {<urn:a> <urn:p> ?o}; INSERT DATA {<urn:a> <urn:p> "theirs"}')
        preview = ds.branches.preview_merge("work")
        assert preview["conflictCount"] == 1 and preview["cells"]
        with pytest.raises(ConflictError):
            ds.branches.merge("work")
        merged = ds.branches.merge("work", on_conflict="theirs")
        assert merged["conflicts"]["resolved"] == 1
        branch.close()
