package io.github.kclejeune.sparkles.jena

import org.apache.jena.atlas.json.JSON
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.query.QueryCancelledException
import org.apache.jena.query.TxnType
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.exec.QueryExec
import org.apache.jena.update.UpdateAction
import org.junit.jupiter.api.Assertions.assertEquals
import org.junit.jupiter.api.Assertions.assertFalse
import org.junit.jupiter.api.Assertions.assertNotNull
import org.junit.jupiter.api.Assertions.assertThrows
import org.junit.jupiter.api.Assertions.assertTrue
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.io.TempDir
import java.nio.file.Path
import java.security.MessageDigest
import java.util.UUID

class EvidenceTest {
    private val sourcesG = "https://example.org/g/sources"
    private val proposalsG = "https://example.org/g/proposals"
    private val decisionsG = "https://example.org/g/decisions"
    private val supportsG = "https://example.org/g/supports"
    private val identityG = "https://example.org/g/identity"
    private val acceptedG = "https://example.org/g/accepted"
    private val mapOutG = "https://example.org/g/entities"

    private fun JsonObject.str(key: String): String = get(key).asString.value()
    private fun JsonObject.bool(key: String): Boolean = get(key).asBoolean.value()
    private fun JsonObject.obj(key: String): JsonObject = get(key).asObject
    private fun JsonObject.arr(key: String): JsonArray = get(key).asArray

    private fun sha256(text: String): String {
        val digest = MessageDigest.getInstance("SHA-256").digest(text.toByteArray(Charsets.UTF_8))
        return "sha256:" + digest.joinToString("") { "%02x".format(it) }
    }

    private fun settingsJson(enabled: Boolean = true, profile: Int = 1): JsonObject =
        JSON.parse(
            """
            {
              "format": 1,
              "profile": $profile,
              "enabled": $enabled,
              "sourceRecords": ["$sourcesG"],
              "proposals": ["$proposalsG"],
              "decisions": ["$decisionsG"],
              "supports": ["$supportsG"],
              "identity": ["$identityG"],
              "projections": ["$acceptedG", "$mapOutG"],
              "policies": {
                "review-v1": {
                  "version": 1,
                  "admittedKinds": ["reported"],
                  "requiredScope": [],
                  "conflicts": "refuse",
                  "boundary": ["$sourcesG", "$proposalsG"],
                  "decisions": "$decisionsG",
                  "supports": "$supportsG",
                  "output": "$acceptedG"
                }
              },
              "matchingPolicies": {
                "match-v1": {
                  "version": 1,
                  "boundary": ["$identityG", "$sourcesG", "$proposalsG"],
                  "decisions": "$decisionsG",
                  "maps": "$identityG",
                  "output": "$mapOutG"
                }
              }
            }
            """.trimIndent(),
        )

    private fun proposalJson(idempotencyKey: String? = null, dryRun: Boolean = false): JsonObject {
        val text = "Release 1 is affected by CVE-1."
        val quote = "Release 1 is affected"
        val keyField = if (idempotencyKey != null) "\"idempotencyKey\": \"$idempotencyKey\"," else ""
        return JSON.parse(
            """
            {
              $keyField
              "dryRun": $dryRun,
              "revisions": [{
                "key": "rev1",
                "source": "https://example.org/source/advisory",
                "sourceKey": "adv-1",
                "content": { "text": "$text" }
              }],
              "renditions": [{
                "key": "ren1",
                "revision": { "key": "rev1" },
                "occurrenceKey": "text-v1",
                "conversion": { "method": "https://example.org/methods/norm", "version": "1" },
                "mediaType": "text/plain",
                "content": { "text": "$text" }
              }],
              "passages": [{
                "key": "p1",
                "revision": { "key": "rev1" },
                "rendition": { "key": "ren1" },
                "occurrenceKey": "span-1",
                "start": 0,
                "end": ${quote.length},
                "textDigest": "${sha256(quote)}",
                "quote": "$quote"
              }],
              "references": [{
                "key": "ref1",
                "occurrenceKey": "ref-1",
                "passage": {
                  "revision": { "key": "rev1" },
                  "rendition": { "key": "ren1" },
                  "passage": { "key": "p1" }
                }
              }],
              "claims": [{
                "key": "c1",
                "occurrenceKey": "claim-1",
                "revision": { "key": "rev1" },
                "proposition": "<https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1>",
                "kind": "reported",
                "polarity": "positive",
                "author": "https://example.org/org/vendor"
              }],
              "links": [{
                "key": "l1",
                "occurrenceKey": "link-1",
                "target": { "key": "c1" },
                "evidence": { "key": "ref1" },
                "relation": "supports",
                "author": "https://example.org/org/importer"
              }],
              "assessments": [{
                "key": "a1",
                "occurrenceKey": "assess-1",
                "target": { "key": "c1" },
                "conclusion": "<https://example.org/status/high>",
                "method": { "method": "https://example.org/methods/triage", "version": "1" },
                "configurationDigest": "${sha256("{}")}",
                "evaluatedAt": "2026-01-01T12:00:00Z",
                "evaluator": "https://example.org/org/analyst",
                "inputs": [{ "key": "c1" }]
              }]
            }
            """.trimIndent(),
        )
    }

    @Test
    fun `settings configure detach and holds lifecycle`(@TempDir tmp: Path) {
        SparklesDatasets.open(tmp.resolve("db")).use { ds ->
            val initial = ds.evidence().settings()
            assertFalse(initial.obj("settings").bool("enabled"))
            assertTrue(initial.arr("managed").isEmpty())

            val configured = ds.evidence().configure(settingsJson())
            assertTrue(configured.obj("settings").bool("enabled"))
            assertTrue(configured.arr("managed").any { it.asString.value() == acceptedG })

            val protectedErr = assertThrows(SparklesTransactionException::class.java) {
                UpdateAction.parseExecute(
                    "INSERT DATA { GRAPH <$acceptedG> { <https://example.org/s> <https://example.org/p> <https://example.org/o> } }",
                    ds,
                )
            }
            assertEquals("profile-managed-graph", protectedErr.code)

            val detached = ds.evidence().detach(acceptedG)
            assertTrue(detached.arr("detached").any { it.asString.value() == acceptedG })
            UpdateAction.parseExecute(
                "INSERT DATA { GRAPH <$acceptedG> { <https://example.org/s> <https://example.org/p> <https://example.org/o> } }",
                ds,
            )
            val reconfigured = ds.evidence().configure(settingsJson())
            assertTrue(reconfigured.arr("managed").any { it.asString.value() == acceptedG })

            val unsupported = assertThrows(SparklesUnsupportedException::class.java) {
                ds.evidence().configure(settingsJson(profile = 99))
            }
            assertEquals("unsupported-profile", unsupported.code)

            UpdateAction.parseExecute("INSERT DATA { <https://example.org/a> <https://example.org/p> '1' }", ds)
            val c1 = ds.headCommit().seq
            UpdateAction.parseExecute("INSERT DATA { <https://example.org/a> <https://example.org/p> '2' }", ds)
            val c2 = ds.headCommit().seq

            val holdId = UUID.randomUUID().toString()
            val commits1 = JSON.parseAny("""[{"commit": $c1}]""").asArray
            val acq = ds.evidence().acquireHold(commits1, holdId)
            assertTrue(acq.bool("created"))
            assertEquals(holdId, acq.obj("hold").str("id"))

            val again = ds.evidence().acquireHold(commits1, holdId)
            assertFalse(again.bool("created"))

            val commits2 = JSON.parseAny("""[{"commit": $c2}]""").asArray
            val conflict = assertThrows(SparklesTransactionException::class.java) {
                ds.evidence().acquireHold(commits2, holdId)
            }
            assertEquals("hold-conflict", conflict.code)

            val page = ds.evidence().holds(onBranch = "main", limit = 10)
            assertEquals(1L, page.get("heldCommits").asNumber.value().toLong())
            assertEquals(1, page.arr("holds").size)

            assertTrue(ds.evidence().releaseHold(holdId))
            assertFalse(ds.evidence().releaseHold(holdId))
        }
    }

    @Test
    fun `propose resolve review withdraw identities and dependents`(@TempDir tmp: Path) {
        SparklesDatasets.open(tmp.resolve("db")).use { ds ->
            UpdateAction.parseExecute("INSERT DATA { <https://example.org/fact> <https://example.org/p> 'kept' }", ds)
            val factCommit = ds.headCommit().seq
            val dsId = ds.datasetId()

            val unconfigured = assertThrows(SparklesUnsupportedException::class.java) {
                ds.evidence().propose(proposalJson())
            }
            assertEquals("unsupported-profile", unconfigured.code)

            ds.evidence().configure(settingsJson())

            val preview = ds.evidence().propose(proposalJson(dryRun = true))
            assertFalse(preview.bool("committed"))
            assertTrue(preview.obj("dryRun").bool("wouldCommit"))

            val proposed = ds.evidence().propose(
                proposalJson(idempotencyKey = "jvm-batch-1"),
                principal = "https://example.org/users/alice",
            )
            assertTrue(proposed.bool("committed"))
            assertFalse(proposed.bool("replayed"))
            val records = proposed.arr("records").associate {
                val obj = it.asObject
                obj.str("key") to obj
            }
            val claim = records.getValue("c1")
            val assessment = records.getValue("a1")

            val replayed = ds.evidence().propose(
                proposalJson(idempotencyKey = "jvm-batch-1"),
                principal = "https://example.org/users/alice",
            )
            assertTrue(replayed.bool("replayed"))
            assertEquals(proposed.str("operation"), replayed.str("operation"))

            val refs = JSON.parseAny(
                """
                [
                  {
                    "key": "f1",
                    "storedFact": {
                      "datasetId": "$dsId",
                      "branchId": "$dsId",
                      "commit": $factCommit,
                      "triple": "<https://example.org/fact> <https://example.org/p> \"kept\""
                    }
                  },
                  {
                    "key": "r1",
                    "record": {
                      "iri": "${claim.str("iri")}",
                      "kinds": ["Claim"]
                    }
                  }
                ]
                """.trimIndent(),
            ).asArray
            val resolved = ds.evidence().resolve(refs)
            assertEquals(2, resolved.arr("resolved").size)

            val missingErr = assertThrows(SparklesNotFoundException::class.java) {
                ds.evidence().resolve(
                    JSON.parseAny("""[{"key": "m1", "record": {"iri": "https://example.org/missing", "kinds": ["Claim"]}}]""").asArray,
                )
            }
            assertEquals("evidence-unavailable", missingErr.code)

            val deps = ds.evidence().findDependents(listOf(claim.str("iri")), transitive = true)
            assertTrue(
                deps.arr("dependents").any {
                    it.asObject.str("assessment") == assessment.str("iri")
                },
            )

            val head = ds.headCommit().seq
            val reviewed = ds.evidence().review(
                JSON.parse(
                    """
                    {
                      "policy": "review-v1",
                      "policyVersion": 1,
                      "ifHead": $head,
                      "decisions": [{
                        "key": "d1",
                        "target": "${claim.str("iri")}",
                        "targetDigest": "${claim.str("recordDigest")}",
                        "disposition": "accepted"
                      }]
                    }
                    """.trimIndent(),
                ),
                principal = "https://example.org/users/reviewer",
            )
            assertTrue(reviewed.bool("committed"))
            assertEquals(1, reviewed.obj("changed").arr("inserted").size)
            assertTrue(
                QueryExec.dataset(ds)
                    .query("ASK { GRAPH <$acceptedG> { <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> } }")
                    .ask(),
            )

            val staleErr = assertThrows(SparklesTransactionException::class.java) {
                ds.evidence().review(
                    JSON.parse(
                        """
                        {
                          "policy": "review-v1",
                          "policyVersion": 1,
                          "ifHead": $head,
                          "decisions": [{
                            "key": "d2",
                            "target": "${claim.str("iri")}",
                            "targetDigest": "${claim.str("recordDigest")}",
                            "disposition": "rejected"
                          }]
                        }
                        """.trimIndent(),
                    ),
                )
            }
            assertEquals("head-changed", staleErr.code)

            val withdrawn = ds.evidence().withdraw(
                JSON.parse(
                    """
                    {
                      "withdrawals": [{
                        "key": "w1",
                        "target": "${claim.str("iri")}",
                        "targetDigest": "${claim.str("recordDigest")}"
                      }]
                    }
                    """.trimIndent(),
                ),
                principal = "https://example.org/users/reviewer",
            )
            assertTrue(withdrawn.bool("committed"))
            assertEquals(1, withdrawn.obj("changed").arr("removed").size)
            val wdStates = ds.evidence().claimStates(
                "review-v1",
                listOf(claim.str("iri"), "https://example.org/missing"),
            )
            assertEquals("review-v1", wdStates.str("policy"))
            assertEquals("withdrawn", wdStates.arr("claims").get(0).asObject.str("state"))
            assertEquals("source", wdStates.arr("claims").get(0).asObject.str("origin"))
            assertTrue(wdStates.arr("claims").get(1).isNull)
            assertFalse(
                QueryExec.dataset(ds)
                    .query("ASK { GRAPH <$acceptedG> { <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> } }")
                    .ask(),
            )

            val idProp = ds.evidence().propose(
                JSON.parse(
                    """
                    {
                      "identities": [{
                        "key": "id1",
                        "occurrenceKey": "match-1",
                        "entities": ["https://example.org/e/a", "https://example.org/e/b"],
                        "relation": "possibleMatch",
                        "preferredEntity": "https://example.org/e/a",
                        "author": "https://example.org/org/linker"
                      }]
                    }
                    """.trimIndent(),
                ),
            )
            val idRec = idProp.arr("records").get(0).asObject
            val idRev = ds.evidence().review(
                JSON.parse(
                    """
                    {
                      "policy": "match-v1",
                      "policyVersion": 1,
                      "ifHead": ${ds.headCommit().seq},
                      "decisions": [{
                        "key": "md1",
                        "target": "${idRec.str("iri")}",
                        "targetDigest": "${idRec.str("recordDigest")}",
                        "disposition": "accepted"
                      }]
                    }
                    """.trimIndent(),
                ),
            )
            assertNotNull(idRev.get("mapRevision"))
            val ent = ds.evidence().resolveEntities("match-v1", listOf("https://example.org/e/b"))
            val e0 = ent.arr("entities").get(0).asObject
            assertEquals("https://example.org/e/a", e0.str("representative"))
        }
    }

    @Test
    fun `closed dataset and transaction guards`(@TempDir tmp: Path) {
        val ds = SparklesDatasets.open(tmp.resolve("db"))
        ds.evidence().configure(settingsJson())

        ds.begin(TxnType.WRITE)
        try {
            // Table-only hold listing and committed claim-state reads succeed inside an open
            // write transaction without deadlocking.
            assertEquals(0L, ds.evidence().holds().get("heldCommits").asNumber.value().toLong())
            SparklesOperation(200L).use { op ->
                assertEquals(
                    0,
                    ds.evidence().claimStates("review-v1", emptyList(), null, null, op).arr("claims").size,
                )
            }
            assertThrows(JenaTransactionException::class.java) { ds.evidence().configure(settingsJson()) }
            assertThrows(JenaTransactionException::class.java) { ds.evidence().detach(acceptedG) }
            assertThrows(JenaTransactionException::class.java) {
                ds.evidence().acquireHold(JSON.parseAny("""[{"commit": 0}]""").asArray)
            }
            assertThrows(JenaTransactionException::class.java) {
                ds.evidence().releaseHold(UUID.randomUUID().toString())
            }
            assertThrows(JenaTransactionException::class.java) { ds.evidence().propose(proposalJson()) }
            assertThrows(JenaTransactionException::class.java) {
                ds.evidence().review(
                    JSON.parse("""{"policy": "review-v1", "policyVersion": 1, "ifHead": 0, "decisions": []}"""),
                )
            }
            assertThrows(JenaTransactionException::class.java) {
                ds.evidence().withdraw(JSON.parse("""{"withdrawals": []}"""))
            }
        } finally {
            ds.abort()
            ds.end()
        }

        // Open transaction on a live branch refuses main-handle evidence writes with accurate message,
        // while committed claim-state reads on that branch still succeed immediately.
        ds.branches().create("work")
        ds.branch("work").use { br ->
            br.begin(TxnType.WRITE)
            try {
                SparklesOperation(200L).use { op ->
                    assertEquals(
                        0,
                        ds.evidence().claimStates("review-v1", emptyList(), "work", null, op).arr("claims").size,
                    )
                }
                val txErr = assertThrows(JenaTransactionException::class.java) {
                    ds.evidence().configure(settingsJson())
                }
                assertTrue(
                    txErr.message?.contains("evidence.configure") == true,
                    "expected evidence.configure in message, got: ${txErr.message}",
                )
            } finally {
                br.abort()
                br.end()
            }

            // Branch handles refuse all evidence operations uniformly with SparklesInvalidException.
            assertThrows(SparklesInvalidException::class.java) { br.evidence().settings() }
            assertThrows(SparklesInvalidException::class.java) { br.evidence().holds() }
            assertThrows(SparklesInvalidException::class.java) {
                br.evidence().configure(settingsJson(enabled = false))
            }
            assertThrows(SparklesInvalidException::class.java) {
                br.evidence().claimStates("review-v1", emptyList())
            }
        }

        // Unknown branch surfaces as SparklesNotFoundException with code="no-such-branch".
        val missingBranch = assertThrows(SparklesNotFoundException::class.java) {
            ds.evidence().resolve(JsonArray(), branch = "missing")
        }
        assertEquals("no-such-branch", missingBranch.code)

        // Pre-cancelled SparklesOperation refuses controlled evidence calls.
        SparklesOperation().use { op ->
            op.cancel()
            assertThrows(QueryCancelledException::class.java) {
                ds.evidence().configure(settingsJson(), op)
            }
            assertThrows(QueryCancelledException::class.java) {
                ds.evidence().resolve(JsonArray(), null, op)
            }
            assertThrows(QueryCancelledException::class.java) {
                ds.evidence().claimStates("review-v1", emptyList(), null, null, op)
            }
        }

        val ev = ds.evidence()
        ds.close()
        assertThrows(SparklesInvalidException::class.java) { ev.settings() }
        assertThrows(SparklesInvalidException::class.java) { ev.holds() }
        assertThrows(SparklesInvalidException::class.java) { ev.resolve(JsonArray()) }
        assertThrows(SparklesInvalidException::class.java) { ev.configure(settingsJson()) }
        assertThrows(SparklesInvalidException::class.java) {
            ev.claimStates("review-v1", emptyList())
        }
        assertThrows(SparklesInvalidException::class.java) {
            ev.resolveEntities("match-v1", listOf("https://example.org/e/a"))
        }
        assertThrows(SparklesInvalidException::class.java) {
            ev.findDependents(listOf("https://example.org/c/1"))
        }

        // Read-only dataset refuses mutating evidence operations.
        SparklesDatasets.open(tmp.resolve("db"), SparklesOptions.builder().readOnly(true).build()).use { ro ->
            assertTrue(ro.evidence().settings().obj("settings").bool("enabled"))
            assertThrows(SparklesNotPermittedException::class.java) {
                ro.evidence().configure(settingsJson())
            }
            assertThrows(SparklesNotPermittedException::class.java) {
                ro.evidence().propose(proposalJson())
            }
        }
    }
}
