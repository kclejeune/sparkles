package io.github.kclejeune.sparkles.jena.contract

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import io.github.kclejeune.sparkles.jena.SparklesOptions
import org.apache.jena.graph.Graph
import org.apache.jena.graph.Node
import org.apache.jena.query.Dataset
import org.apache.jena.query.DatasetFactory
import org.apache.jena.query.ReadWrite
import org.apache.jena.query.TxnType
import org.apache.jena.shared.PrefixMapping
import org.apache.jena.sparql.JenaTransactionException
import org.apache.jena.sparql.core.AbstractDatasetGraphFind
import org.apache.jena.sparql.core.AbstractDatasetGraphFindPatterns
import org.apache.jena.sparql.core.AbstractDatasetGraphTests
import org.apache.jena.sparql.core.AbstractTestDynamicDataset
import org.apache.jena.sparql.core.AbstractTestGraphOverDatasetGraph
import org.apache.jena.sparql.core.DatasetGraph
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.graph.AbstractTestGraphAddDelete
import org.apache.jena.sparql.graph.AbstractTestPrefixMappingView
import org.apache.jena.sparql.modify.AbstractTestUpdateGraph
import org.apache.jena.sparql.sse.SSE
import org.apache.jena.sparql.transaction.AbstractTestTransPromote
import org.apache.jena.sparql.transaction.AbstractTestTransactionLifecycle
import org.apache.jena.system.Txn
import org.junit.jupiter.api.AfterEach
import org.junit.jupiter.api.BeforeEach
import org.junit.jupiter.api.Disabled
import org.junit.jupiter.api.Test

/*
 * Jena's contract tests for datasets, graphs, transactions, updates and prefixes, run on
 * Sparkles datasets as TDB2's own subclasses run them on TDB2 (P04 §7). The suites that
 * write outside a transaction get a dataset with autocommit, since a Sparkles dataset
 * refuses such writes by default, as TDB2 does.
 */

/** Every dataset a test opened, closed after it. */
private val opened = ThreadLocal.withInitial { ArrayList<DatasetGraphSparkles>() }

private fun memory(options: SparklesOptions = SparklesOptions.DEFAULT): DatasetGraphSparkles =
    SparklesDatasets.memory(options).also { opened.get().add(it) }

private fun autocommit(): DatasetGraphSparkles = memory(SparklesOptions.builder().autocommit(true).build())

private fun closeAll() {
    val l = opened.get()
    for (d in l) {
        if (d.isInTransaction) {
            try {
                d.abort()
            } catch (_: RuntimeException) {
            }
            try {
                d.end()
            } catch (_: RuntimeException) {
            }
        }
        d.close()
    }
    l.clear()
}

class TestDatasetGraphSparkles : AbstractDatasetGraphTests() {
    private val dsg = memory()

    @BeforeEach
    fun before() = dsg.begin(ReadWrite.WRITE)

    @AfterEach
    fun after() = closeAll()

    override fun emptyDataset(): DatasetGraph = dsg
}

class TestDatasetGraphFindSparkles : AbstractDatasetGraphFind() {
    override fun create(): DatasetGraph = autocommit()

    override fun create(data: Collection<Quad>): DatasetGraph {
        val dsg = memory()
        Txn.executeWrite(dsg) { data.forEach(dsg::add) }
        return dsg
    }

    @AfterEach
    fun after() = closeAll()
}

class TestDatasetGraphFindPatternsSparkles : AbstractDatasetGraphFindPatterns() {
    override fun create(): DatasetGraph = autocommit()

    override fun create(data: Collection<Quad>): DatasetGraph {
        val dsg = memory()
        Txn.executeWrite(dsg) { data.forEach(dsg::add) }
        return dsg
    }

    @AfterEach
    fun after() = closeAll()
}

class TestGraphOverDatasetSparkles : AbstractTestGraphOverDatasetGraph() {
    private var dsg: DatasetGraphSparkles? = null

    override fun createBaseDSG(): DatasetGraph {
        val d = dsg ?: memory().also {
            it.begin(ReadWrite.WRITE)
            dsg = it
        }
        return d
    }

    override fun makeNamedGraph(dsg: DatasetGraph, gn: Node): Graph = dsg.getGraph(gn)

    override fun makeDefaultGraph(dsg: DatasetGraph): Graph = dsg.defaultGraph

    @AfterEach
    fun after2() = closeAll()
}

class TestGraphAddDeleteDefaultSparkles : AbstractTestGraphAddDelete() {
    private val dsg = memory()
    private lateinit var graph: Graph

    @BeforeEach
    fun before() {
        dsg.begin(ReadWrite.WRITE)
        graph = dsg.defaultGraph
    }

    @AfterEach
    fun after() = closeAll()

    override fun emptyGraph(): Graph {
        graph.clear()
        return graph
    }

    override fun returnGraph(g: Graph) {}
}

class TestGraphAddDeleteNamedSparkles : AbstractTestGraphAddDelete() {
    private val dsg = memory()
    private lateinit var graph: Graph

    @BeforeEach
    fun before() {
        dsg.begin(ReadWrite.WRITE)
        graph = dsg.getGraph(SSE.parseNode("<http://example/namedGraph>"))
    }

    @AfterEach
    fun after() = closeAll()

    override fun emptyGraph(): Graph {
        graph.clear()
        return graph
    }

    override fun returnGraph(g: Graph) {}
}

class TestTransactionLifecycleSparkles : AbstractTestTransactionLifecycle() {
    override fun create(): Dataset = DatasetFactory.wrap(memory())

    @AfterEach
    fun after() = closeAll()
}

class TestTransPromoteSparkles : AbstractTestTransPromote(arrayOf()) {
    override fun getTransactionExceptionClass(): Class<out Exception> = JenaTransactionException::class.java

    /**
     * TDB2 counts every write transaction as a new data version, so a writer that commits
     * nothing makes a later promotion fail. Sparkles makes no commit for a transaction that
     * changed nothing, so the promotion succeeds (P04 §3.3 and §3.11).
     */
    @Test
    @Disabled("Sparkles allows a promotion after a write transaction that committed no change (P04 §3.11)")
    override fun promote_active_writer_1() {}

    override fun create(): DatasetGraph = memory()

    @AfterEach
    fun after() = closeAll()
}

class TestDynamicDatasetSparkles : AbstractTestDynamicDataset() {
    override fun createDataset(): Dataset {
        val dsg = memory()
        dsg.begin(ReadWrite.WRITE)
        return DatasetFactory.wrap(dsg)
    }

    // the superclass's @AfterEach calls this, after any of this class's would run
    override fun releaseDataset(ds: Dataset) {
        ds.abort()
        ds.end()
        closeAll()
    }
}

class TestUpdateGraphSparkles : AbstractTestUpdateGraph() {
    override fun getEmptyDatasetGraph(): DatasetGraph = autocommit()

    @AfterEach
    fun after() = closeAll()
}

class TestPrefixMappingSparkles : AbstractTestPrefixMappingView() {
    private val dsg = memory()
    private var last: PrefixMapping? = null

    @BeforeEach
    fun before() = dsg.begin(TxnType.READ_PROMOTE)

    @AfterEach
    fun after() {
        dsg.commit()
        dsg.end()
        closeAll()
    }

    override fun create(): PrefixMapping {
        last = dsg.defaultGraph.prefixMapping
        return view()
    }

    override fun view(): PrefixMapping = last!!
}
