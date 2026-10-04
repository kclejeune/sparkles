package io.github.kclejeune.sparkles.jena.contract

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import io.github.kclejeune.sparkles.jena.SparklesDatasets
import io.github.kclejeune.sparkles.jena.SparklesOptions
import org.apache.jena.graph.Graph
import org.apache.jena.graph.test.AbstractTestGraph

/**
 * jena-core's Graph contract (a JUnit 3 suite) on the default graph of a Sparkles dataset
 * with autocommit, since the suite writes outside transactions.
 */
class TestGraphSparkles(name: String) : AbstractTestGraph(name) {
    private val opened = ArrayList<DatasetGraphSparkles>()

    override fun getNewGraph(): Graph {
        val dsg = SparklesDatasets.memory(SparklesOptions.builder().autocommit(true).build())
        opened.add(dsg)
        return dsg.defaultGraph
    }

    override fun tearDown() {
        opened.forEach { it.close() }
        opened.clear()
        super.tearDown()
    }

    // These two add generalized triples, whose predicate is a blank node or a literal.
    // Sparkles stores RDF triples only and refuses them, where TDB2 stores them.

    override fun testContainsConcrete() {}

    override fun testContainsNode() {}
}
