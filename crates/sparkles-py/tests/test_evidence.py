"""Evidence settings, durable holds, reference resolution, proposals, reviews,
withdrawals, identity resolution and dependent lookups through Python bindings (C25, item 509)."""

from __future__ import annotations

import hashlib
import uuid
from pathlib import Path
from typing import Any

import pytest

from sparkles import (
    ConflictError,
    Dataset,
    InvalidInputError,
    NotFoundError,
    UnsupportedError,
)

SOURCES_G = "https://example.org/g/sources"
PROPOSALS_G = "https://example.org/g/proposals"
DECISIONS_G = "https://example.org/g/decisions"
SUPPORTS_G = "https://example.org/g/supports"
IDENTITY_G = "https://example.org/g/identity"
ACCEPTED_G = "https://example.org/g/accepted"
MAP_OUT_G = "https://example.org/g/entities"


def _sha256(text: str) -> str:
    return "sha256:" + hashlib.sha256(text.encode("utf-8")).hexdigest()


def _settings(enabled: bool = True) -> dict[str, Any]:
    return {
        "format": 1,
        "profile": 1,
        "enabled": enabled,
        "sourceRecords": [SOURCES_G],
        "proposals": [PROPOSALS_G],
        "decisions": [DECISIONS_G],
        "supports": [SUPPORTS_G],
        "identity": [IDENTITY_G],
        "projections": [ACCEPTED_G, MAP_OUT_G],
        "policies": {
            "review-v1": {
                "version": 1,
                "admittedKinds": ["reported"],
                "requiredScope": [],
                "conflicts": "refuse",
                "boundary": [SOURCES_G, PROPOSALS_G],
                "decisions": DECISIONS_G,
                "supports": SUPPORTS_G,
                "output": ACCEPTED_G,
            }
        },
        "matchingPolicies": {
            "match-v1": {
                "version": 1,
                "boundary": [IDENTITY_G, SOURCES_G, PROPOSALS_G],
                "decisions": DECISIONS_G,
                "maps": IDENTITY_G,
                "output": MAP_OUT_G,
            }
        },
    }


def _claim_proposal(*, text: str = "Release 1 is affected by CVE-1.", idempotency_key: str | None = None) -> dict[str, Any]:
    quote = "Release 1 is affected"
    body: dict[str, Any] = {
        "revisions": [
            {
                "key": "rev1",
                "source": "https://example.org/source/advisory",
                "sourceKey": "adv-1",
                "content": {"text": text},
            }
        ],
        "renditions": [
            {
                "key": "ren1",
                "revision": {"key": "rev1"},
                "occurrenceKey": "text-v1",
                "conversion": {"method": "https://example.org/methods/norm", "version": "1"},
                "mediaType": "text/plain",
                "content": {"text": text},
            }
        ],
        "passages": [
            {
                "key": "p1",
                "revision": {"key": "rev1"},
                "rendition": {"key": "ren1"},
                "occurrenceKey": "span-1",
                "start": 0,
                "end": len(quote),
                "textDigest": _sha256(quote),
                "quote": quote,
            }
        ],
        "references": [
            {
                "key": "ref1",
                "occurrenceKey": "ref-1",
                "passage": {
                    "revision": {"key": "rev1"},
                    "rendition": {"key": "ren1"},
                    "passage": {"key": "p1"},
                },
            }
        ],
        "claims": [
            {
                "key": "c1",
                "occurrenceKey": "claim-1",
                "revision": {"key": "rev1"},
                "proposition": "<https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1>",
                "kind": "reported",
                "polarity": "positive",
                "author": "https://example.org/org/vendor",
            }
        ],
        "links": [
            {
                "key": "l1",
                "occurrenceKey": "link-1",
                "target": {"key": "c1"},
                "evidence": {"key": "ref1"},
                "relation": "supports",
                "author": "https://example.org/org/importer",
            }
        ],
        "assessments": [
            {
                "key": "a1",
                "occurrenceKey": "assess-1",
                "target": {"key": "c1"},
                "conclusion": "<https://example.org/status/high>",
                "method": {"method": "https://example.org/methods/triage", "version": "1"},
                "configurationDigest": _sha256("{}"),
                "evaluatedAt": "2026-01-01T12:00:00Z",
                "evaluator": "https://example.org/org/analyst",
                "inputs": [{"key": "c1"}],
            }
        ],
    }
    if idempotency_key is not None:
        body["idempotencyKey"] = idempotency_key
    return body


def test_evidence_settings_configure_and_detach(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        initial = ds.evidence.settings()
        assert initial["settings"]["enabled"] is False
        assert initial["digest"] is None
        assert initial["managed"] == []
        assert initial["detached"] == []

        configured = ds.evidence.configure(_settings())
        assert configured["settings"]["enabled"] is True
        assert configured["digest"] is not None
        assert configured["revision"] >= 1
        assert ACCEPTED_G in configured["managed"]
        assert configured["detached"] == []

        # Managed graph refuses direct SPARQL update with profile-managed-graph.
        with pytest.raises(ConflictError) as exc:
            ds.update(f"INSERT DATA {{ GRAPH <{ACCEPTED_G}> {{ <https://example.org/s> <https://example.org/p> <https://example.org/o> }} }}")
        assert exc.value.code == "profile-managed-graph"

        # Detach opens the graph for ordinary writes and reports it in detached.
        detached = ds.evidence.detach(ACCEPTED_G)
        assert ACCEPTED_G in detached["detached"]
        assert ACCEPTED_G not in detached["managed"]
        ds.update(f"INSERT DATA {{ GRAPH <{ACCEPTED_G}> {{ <https://example.org/s> <https://example.org/p> <https://example.org/o> }} }}")

        # Re-configuring re-protects the detached graph.
        reconfigured = ds.evidence.configure(_settings())
        assert ACCEPTED_G in reconfigured["managed"]
        assert reconfigured["detached"] == []

        # Unsupported profile version refuses cleanly.
        bad_profile = _settings()
        bad_profile["profile"] = 99
        with pytest.raises(UnsupportedError) as exc2:
            ds.evidence.configure(bad_profile)
        assert exc2.value.code == "unsupported-profile"


def test_evidence_holds_lifecycle_and_conflicts(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        ds.update("INSERT DATA { <https://example.org/a> <https://example.org/p> '1' }")
        c1 = ds.head_commit.seq
        ds.update("INSERT DATA { <https://example.org/a> <https://example.org/p> '2' }")
        c2 = ds.head_commit.seq

        hold_id = str(uuid.uuid4())
        acq = ds.evidence.acquire_hold([{"commit": c1}], id=hold_id)
        assert acq["created"] is True
        assert acq["hold"]["id"] == hold_id
        assert acq["hold"]["commits"][0]["commit"] == c1

        # Re-acquiring with the same id and commits is idempotent.
        again = ds.evidence.acquire_hold([{"commit": c1}], id=hold_id)
        assert again["created"] is False

        # Re-acquiring the same id with different commits raises hold-conflict.
        with pytest.raises(ConflictError) as exc:
            ds.evidence.acquire_hold([{"commit": c2}], id=hold_id)
        assert exc.value.code == "hold-conflict"

        listed = ds.evidence.holds(on_branch="main", limit=10)
        assert listed["heldCommits"] == 1
        assert len(listed["holds"]) == 1
        assert listed["holds"][0]["id"] == hold_id

        assert ds.evidence.release_hold(hold_id) is True
        assert ds.evidence.release_hold(hold_id) is False
        assert ds.evidence.holds()["heldCommits"] == 0


def test_evidence_propose_resolve_review_withdraw_identities_and_dependents(tmp_path: Path) -> None:
    with Dataset(tmp_path / "db") as ds:
        ds.update("INSERT DATA { <https://example.org/fact> <https://example.org/p> 'kept' }")
        fact_commit = ds.head_commit.seq
        ds_id = ds.dataset_id

        # Unconfigured dataset refuses proposal with unsupported-profile.
        with pytest.raises(UnsupportedError) as exc:
            ds.evidence.propose(_claim_proposal())
        assert exc.value.code == "unsupported-profile"

        ds.evidence.configure(_settings())

        # Dry run previews without committing.
        preview = ds.evidence.propose({**_claim_proposal(), "dryRun": True})
        assert preview["committed"] is False
        assert preview["dryRun"]["wouldCommit"] is True

        # Propose claim, source records, passage reference, link and assessment.
        proposed = ds.evidence.propose(
            _claim_proposal(idempotency_key="batch-1"),
            principal="https://example.org/users/alice",
        )
        assert proposed["committed"] is True
        assert proposed["replayed"] is False
        by_key = {r["key"]: r for r in proposed["records"]}
        claim = by_key["c1"]
        assessment = by_key["a1"]

        # Idempotent replay returns same receipt without a new commit.
        replayed = ds.evidence.propose(
            _claim_proposal(idempotency_key="batch-1"),
            principal="https://example.org/users/alice",
        )
        assert replayed["replayed"] is True
        assert replayed["operation"] == proposed["operation"]

        # Resolve both a stored fact and the authenticated claim record.
        resolved = ds.evidence.resolve(
            [
                {
                    "key": "f1",
                    "storedFact": {
                        "datasetId": ds_id,
                        "branchId": ds_id,
                        "commit": fact_commit,
                        "triple": '<https://example.org/fact> <https://example.org/p> "kept"',
                    },
                },
                {
                    "key": "r1",
                    "record": {
                        "iri": claim["iri"],
                        "kinds": ["Claim"],
                    },
                },
            ]
        )
        assert len(resolved["resolved"]) == 2
        assert resolved["resolved"][0]["storedFact"]["commit"] == fact_commit
        assert resolved["resolved"][1]["record"]["iri"] == claim["iri"]
        assert resolved["resolved"][1]["record"]["kind"] == "Claim"

        # Missing record resolves to NotFoundError with evidence-unavailable code.
        with pytest.raises(NotFoundError) as exc_missing:
            ds.evidence.resolve(
                [{"key": "m1", "record": {"iri": "https://example.org/no-such-claim", "kinds": ["Claim"]}}]
            )
        assert exc_missing.value.code == "evidence-unavailable"

        # Find dependents of the claim finds the assessment.
        deps = ds.evidence.find_dependents([claim["iri"]], transitive=True)
        assert any(d["assessment"] == assessment["iri"] for d in deps["dependents"])

        # Review accepts the claim and projects its triple into ACCEPTED_G.
        head = ds.head_commit.seq
        reviewed = ds.evidence.review(
            {
                "policy": "review-v1",
                "policyVersion": 1,
                "ifHead": head,
                "decisions": [
                    {
                        "key": "d1",
                        "target": claim["iri"],
                        "targetDigest": claim["recordDigest"],
                        "disposition": "accepted",
                    }
                ],
            },
            principal="https://example.org/users/reviewer",
        )
        assert reviewed["committed"] is True
        assert len(reviewed["changed"]["inserted"]) == 1
        assert ds.ask(
            f"ASK {{ GRAPH <{ACCEPTED_G}> {{ <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> }} }}"
        )

        # Stale ifHead refuses with ConflictError(code="head-changed").
        with pytest.raises(ConflictError) as exc_head:
            ds.evidence.review(
                {
                    "policy": "review-v1",
                    "policyVersion": 1,
                    "ifHead": head,
                    "decisions": [
                        {
                            "key": "d2",
                            "target": claim["iri"],
                            "targetDigest": claim["recordDigest"],
                            "disposition": "rejected",
                        }
                    ],
                }
            )
        assert exc_head.value.code == "head-changed"

        # Withdraw removes the sole support and removes the projected triple.
        withdrawn = ds.evidence.withdraw(
            {
                "withdrawals": [
                    {
                        "key": "w1",
                        "target": claim["iri"],
                        "targetDigest": claim["recordDigest"],
                    }
                ]
            },
            principal="https://example.org/users/reviewer",
        )
        assert withdrawn["committed"] is True
        assert len(withdrawn["changed"]["removed"]) == 1
        assert not ds.ask(
            f"ASK {{ GRAPH <{ACCEPTED_G}> {{ <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> }} }}"
        )

        # Identity proposal, review under matching policy, and resolve_entities.
        id_prop = ds.evidence.propose(
            {
                "identities": [
                    {
                        "key": "id1",
                        "occurrenceKey": "match-1",
                        "entities": ["https://example.org/e/a", "https://example.org/e/b"],
                        "relation": "possibleMatch",
                        "preferredEntity": "https://example.org/e/a",
                        "author": "https://example.org/org/linker",
                    }
                ]
            }
        )
        id_rec = id_prop["records"][0]
        id_rev = ds.evidence.review(
            {
                "policy": "match-v1",
                "policyVersion": 1,
                "ifHead": ds.head_commit.seq,
                "decisions": [
                    {
                        "key": "md1",
                        "target": id_rec["iri"],
                        "targetDigest": id_rec["recordDigest"],
                        "disposition": "accepted",
                    }
                ],
            }
        )
        assert id_rev["mapRevision"] is not None
        ent = ds.evidence.resolve_entities("match-v1", ["https://example.org/e/b"])
        assert ent["entities"][0]["representative"] == "https://example.org/e/a"
        assert set(ent["entities"][0]["members"]) == {
            "https://example.org/e/a",
            "https://example.org/e/b",
        }


def test_evidence_closed_dataset_and_transaction_guards(tmp_path: Path) -> None:
    ds = Dataset(tmp_path / "db")
    ds.evidence.configure(_settings())
    ds.update("INSERT DATA { <https://example.org/a> <https://example.org/p> '1' }")

    # Active transaction on this thread refuses all mutating evidence operations.
    with ds.transaction():
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.configure(_settings())
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.detach(ACCEPTED_G)
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.acquire_hold([{"commit": 1}])
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.release_hold(str(uuid.uuid4()))
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.propose(_claim_proposal())
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.review(
                {
                    "policy": "review-v1",
                    "policyVersion": 1,
                    "ifHead": 1,
                    "decisions": [],
                }
            )
        with pytest.raises(ConflictError, match="open transaction"):
            ds.evidence.withdraw({"withdrawals": []})

    # Closed dataset refuses both read and write evidence operations.
    ev = ds.evidence
    ds.close()
    with pytest.raises(InvalidInputError, match="closed"):
        ev.settings()
    with pytest.raises(InvalidInputError, match="closed"):
        ev.holds()
    with pytest.raises(InvalidInputError, match="closed"):
        ev.configure(_settings())
    with pytest.raises(InvalidInputError, match="closed"):
        ev.resolve([])
    with pytest.raises(InvalidInputError, match="closed"):
        ev.resolve_entities("match-v1", ["https://example.org/e/a"])
    with pytest.raises(InvalidInputError, match="closed"):
        ev.find_dependents(["https://example.org/c/1"])
