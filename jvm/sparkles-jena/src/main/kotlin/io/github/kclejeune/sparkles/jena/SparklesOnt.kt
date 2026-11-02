package io.github.kclejeune.sparkles.jena

import io.github.kclejeune.sparkles.jena.internal.TxnState
import io.github.kclejeune.sparkles.jena.internal.decodeTriples
import io.github.kclejeune.sparkles.jena.internal.encodePattern
import io.github.kclejeune.sparkles.jena.internal.ffi
import org.apache.jena.atlas.json.JsonArray
import org.apache.jena.atlas.json.JsonObject
import org.apache.jena.atlas.json.JsonValue
import org.apache.jena.graph.Graph
import org.apache.jena.graph.Node
import org.apache.jena.graph.NodeFactory
import org.apache.jena.graph.Triple
import org.apache.jena.graph.GraphMemFactory
import org.apache.jena.ontapi.OntModelFactory
import org.apache.jena.ontapi.OntSpecification
import org.apache.jena.ontapi.model.OntModel
import org.apache.jena.query.ReadWrite
import org.apache.jena.sparql.core.Quad
import org.apache.jena.sparql.graph.GraphWrapper

/**
 * The OWL 2 ontology of a Sparkles graph on the JVM (spec F21 §2.3). [model] runs Jena's
 * own `jena-ontapi` over a copy of the graph, and [check] runs the native profile checks.
 * `jena-ontapi` is an optional dependency: a program that calls [model] adds
 * `org.apache.jena:jena-ontapi` of its Jena version.
 */
public object SparklesOnt {
    /**
     * An `OntModel` that Jena's `OntModelFactory` builds over the graph `graphIri` (the
     * default graph when `null`) of `dataset`, with the profile behaviour of `spec`. The
     * model reads an in-memory copy of the graph that one native call fills, from the
     * thread's transaction when it has one and from the head otherwise. Its writes go to
     * the copy and to the Sparkles graph, in the thread's write transaction when there is
     * one, so the additions of a transaction make one commit. Without a transaction each
     * write commits on its own when the dataset allows autocommit. A transaction that
     * aborts undoes its writes in the copy too. A commit made through another handle after
     * the model is built is not seen by it.
     */
    @JvmStatic
    @JvmOverloads
    public fun model(
        dataset: DatasetGraphSparkles,
        graphIri: String? = null,
        spec: OntSpecification = OntSpecification.OWL2_DL_MEM,
    ): OntModel {
        val node = graphNode(graphIri)
        return OntModelFactory.createModel(graph(dataset, node), spec)
    }

    /**
     * The copy that [model] reads: the graph's triples in one native read, with writes sent
     * through to the dataset.
     */
    @JvmStatic
    @JvmOverloads
    public fun graph(dataset: DatasetGraphSparkles, graphIri: String? = null): Graph = graph(dataset, graphNode(graphIri))

    private fun graph(dataset: DatasetGraphSparkles, node: Node): Graph {
        val mem = GraphMemFactory.createDefaultGraph()
        val bytes = dataset.source().findAll(encodePattern(node, null, null, null))
        for (t in decodeTriples(bytes)) mem.add(t)
        return ReadThroughGraph(mem, dataset, dataset.getGraph(node))
    }

    private fun graphNode(iri: String?): Node = if (iri == null || iri == "default") Quad.defaultGraphIRI else NodeFactory.createURI(iri)

    /** The native profile checks of the graph: one report per profile, as [SparklesOntology.check] answers. */
    @JvmStatic
    @JvmOverloads
    public fun check(dataset: DatasetGraphSparkles, graphIri: String? = null, profiles: List<String> = listOf("dl"), assertions: Boolean = true): JsonObject =
        dataset.ontology().check(OntologyRead(graph = graphIri), profiles, assertions)

    /** The summary of the ontology in the graph, as [SparklesOntology.summary] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun summary(dataset: DatasetGraphSparkles, graphIri: String? = null): JsonObject = dataset.ontology().summary(OntologyRead(graph = graphIri))

    /** One page of the axioms of the graph, as [SparklesOntology.axioms] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun axioms(dataset: DatasetGraphSparkles, graphIri: String? = null, query: AxiomQuery = AxiomQuery()): JsonObject =
        dataset.ontology().axioms(OntologyRead(graph = graphIri), query)

    /** Add and remove axioms of the graph in one commit, as [SparklesOntology.edit] does. */
    @JvmStatic
    @JvmOverloads
    public fun edit(dataset: DatasetGraphSparkles, change: AxiomChange, graphIri: String? = null): JsonObject =
        dataset.ontology().edit(change, OntologyRead(graph = graphIri))

    /** The axioms of the graph that differ between two states, as [SparklesOntology.diff] answers. */
    @JvmStatic
    @JvmOverloads
    public fun diff(dataset: DatasetGraphSparkles, from: String, to: String? = null, graphIri: String? = null): JsonObject =
        dataset.ontology().diff(from, to, OntologyRead(graph = graphIri))
}

/**
 * A graph whose reads come from `copy` and whose writes go to `target` and to `copy`. The
 * triples a write transaction writes are noted until the transaction ends, and then set
 * to what the dataset holds, so that the copy keeps the state of the dataset when the
 * transaction aborts. Like Jena's in-memory graphs, the copy needs the caller's locking
 * when threads use it at once, and another thread sees a transaction's writes before it
 * commits.
 */
private class ReadThroughGraph(
    private val copy: Graph,
    private val dataset: DatasetGraphSparkles,
    private val target: Graph,
) : GraphWrapper(copy) {
    // the triples the transaction `pending` wrote
    private val journal = LinkedHashSet<Triple>()
    private var pending: TxnState? = null

    /**
     * Set the triples of an ended transaction to what the dataset holds in the caller's
     * view. It waits for nothing: a transaction that is still open, or a read whose own
     * transaction reads an older state, leaves them for later. After a write of the
     * caller's, `written`, the transaction that held the writer lock before it has ended,
     * and the caller notes the triples it wrote itself, so that its own abort sets them
     * back.
     */
    @Synchronized
    private fun settle(written: Boolean) {
        val t = pending ?: return
        val own = dataset.txn()
        if (t === own) return
        if (!written && (t.active || (own != null && own.mode != ReadWrite.WRITE))) return
        for (triple in journal) {
            if (target.contains(triple)) copy.add(triple) else copy.delete(triple)
        }
        journal.clear()
        pending = null
    }

    /** Note triples the caller wrote; a write outside a transaction has committed already. */
    private fun record(written: Collection<Triple>) {
        val txn = dataset.txn() ?: return
        pending = txn
        journal.addAll(written)
    }

    override fun get(): Graph {
        settle(false)
        return copy
    }

    // the native write runs outside the monitor, since it may wait for the writer lock
    override fun add(t: Triple) {
        target.add(t)
        synchronized(this) {
            settle(true)
            copy.add(t)
            record(listOf(t))
        }
    }

    override fun delete(t: Triple) {
        target.delete(t)
        synchronized(this) {
            settle(true)
            copy.delete(t)
            record(listOf(t))
        }
    }

    override fun remove(s: Node?, p: Node?, o: Node?) {
        val ms = s ?: Node.ANY
        val mp = p ?: Node.ANY
        val mo = o ?: Node.ANY
        // the triples the removal may write, when a transaction will need to set them back
        val touched: List<Triple> = if (dataset.txn() == null) {
            emptyList()
        } else {
            synchronized(this) {
                copy.find(ms, mp, mo).toList() + journal.filter { it.matches(ms, mp, mo) }
            }
        }
        target.remove(s, p, o)
        synchronized(this) {
            settle(true)
            copy.remove(ms, mp, mo)
            record(touched)
        }
    }

    override fun clear() = remove(Node.ANY, Node.ANY, Node.ANY)
}

/**
 * What ontology to read: the graphs (`"default"` or IRIs; the default graph when empty),
 * whether to follow `owl:imports` to graphs of the dataset, the mode of the read, the state
 * (`commit:N`, a snapshot or a time; the head when `null`) and whether to include the
 * materialized inferences.
 */
public data class OntologyRead @JvmOverloads public constructor(
    public val graphs: List<String> = emptyList(),
    public val imports: Boolean = true,
    public val mode: String = "lenient",
    public val at: String? = null,
    public val inferences: Boolean = false,
) {
    public constructor(graph: String?) : this(graphs = listOfNotNull(graph))

    internal fun value(): JsonObject = JsonObject().also { j ->
        if (graphs.isNotEmpty()) j.put("graph", JsonArray().also { a -> graphs.forEach { a.add(it) } })
        j.put("imports", imports)
        j.put("mode", mode)
        at?.let { j.put("at", it) }
        if (inferences) j.put("inferences", true)
    }

    /** The members that name the ontology, without the state and the inferences, as edits and diffs take them. */
    internal fun selection(): JsonObject {
        require(at == null) { "an edit or a diff names its states itself: leave out at" }
        require(!inferences) { "an edit or a diff reads the asserted axioms: leave out inferences" }
        return value()
    }
}

/** Which axioms a listing keeps: axiom types such as `SubClassOf`, an IRI they mention, the assertions, and the page. */
public data class AxiomQuery @JvmOverloads public constructor(
    public val types: List<String> = emptyList(),
    public val about: String? = null,
    public val assertions: Boolean = false,
    public val limit: Int = 100,
    public val offset: Long = 0,
) {
    internal fun into(j: JsonObject) {
        require(limit > 0 && offset >= 0)
        if (types.isNotEmpty()) j.put("type", types.joinToString(","))
        about?.let { j.put("about", it) }
        j.put("assertions", assertions)
        j.put("limit", limit.toLong())
        j.put("offset", offset)
    }
}

/**
 * The axioms an edit adds and removes, each in functional-style syntax with the dataset's
 * prefixes or as a JSON object in the axioms' JSON form, with the graph that takes the
 * additions, whether to declare the entities they use, and the commit message.
 */
public data class AxiomChange @JvmOverloads public constructor(
    public val add: List<Any> = emptyList(),
    public val remove: List<Any> = emptyList(),
    public val declare: Boolean? = null,
    public val target: String? = null,
    public val message: String? = null,
) {
    internal fun into(j: JsonObject) {
        fun list(v: List<Any>): JsonArray = JsonArray().also { a ->
            for (x in v) {
                when (x) {
                    is String -> a.add(x)
                    is JsonValue -> a.add(x)
                    else -> throw IllegalArgumentException("an axiom is a String in functional-style syntax or a JsonObject")
                }
            }
        }
        j.put("add", list(add))
        j.put("remove", list(remove))
        declare?.let { j.put("declare", it) }
        target?.let { j.put("target", it) }
        message?.let { j.put("message", it) }
    }
}

/**
 * The OWL 2 ontology handle of a dataset (spec F21): its summary, axioms, profile checks,
 * edits and diffs, in the JSON forms of the HTTP routes. Reads run on the head or on the
 * state the request names, and edits commit on their own outside a transaction.
 */
public class SparklesOntology internal constructor(private val owner: DatasetGraphSparkles) {
    private fun call(op: String, args: JsonObject, operation: SparklesOperation, write: Boolean): JsonObject {
        if (write) owner.checkNoTxn("ontology.$op") else owner.checkOpen()
        return document(ffi { owner.handle.ffi.ontologyCall(op, args.bytes(), operation.native) }).asObject
    }

    /** The header, the counts of entities by kind and of axioms by type, the graphs read and the diagnostics. */
    @JvmOverloads public fun summary(read: OntologyRead = OntologyRead()): JsonObject = SparklesOperation().use { summary(read, it) }
    public fun summary(read: OntologyRead, operation: SparklesOperation): JsonObject = call("summary", read.value(), operation, false)

    /** One page of the axioms `query` keeps, each with its type, its functional-style form, its JSON form and its graph. */
    @JvmOverloads public fun axioms(read: OntologyRead = OntologyRead(), query: AxiomQuery = AxiomQuery()): JsonObject = SparklesOperation().use { axioms(read, query, it) }
    public fun axioms(read: OntologyRead, query: AxiomQuery, operation: SparklesOperation): JsonObject =
        call("axioms", read.value().also { query.into(it) }, operation, false)

    /** One report per profile (`el`, `ql`, `rl`, `dl`), over the terminology and, unless `assertions` is false, the assertions. */
    @JvmOverloads public fun check(read: OntologyRead = OntologyRead(), profiles: List<String> = listOf("dl"), assertions: Boolean = true): JsonObject =
        SparklesOperation().use { check(read, profiles, assertions, it) }
    public fun check(read: OntologyRead, profiles: List<String>, assertions: Boolean, operation: SparklesOperation): JsonObject {
        val args = read.value()
        args.put("profiles", JsonArray().also { a -> profiles.forEach { a.add(it) } })
        args.put("assertions", assertions)
        return call("check", args, operation, false)
    }

    /** Add and remove axioms in one commit. */
    @JvmOverloads public fun edit(change: AxiomChange, read: OntologyRead = OntologyRead()): JsonObject = SparklesOperation().use { edit(change, read, it) }
    public fun edit(change: AxiomChange, read: OntologyRead, operation: SparklesOperation): JsonObject =
        call("edit", read.selection().also { change.into(it) }, operation, true)

    /** Remove an entity: its declarations, the axioms that mention it and the assertions about it, in one commit. */
    @JvmOverloads public fun removeEntity(iri: String, read: OntologyRead = OntologyRead(), message: String? = null): JsonObject =
        SparklesOperation().use { removeEntity(iri, read, message, it) }
    public fun removeEntity(iri: String, read: OntologyRead, message: String?, operation: SparklesOperation): JsonObject {
        val args = read.selection()
        args.put("iri", iri)
        message?.let { args.put("message", it) }
        return call("removeEntity", args, operation, true)
    }

    /** The axioms that differ between the states `from` and `to` (the head when `null`). */
    @JvmOverloads public fun diff(from: String, to: String? = null, read: OntologyRead = OntologyRead(), assertions: Boolean = false): JsonObject =
        SparklesOperation().use { diff(from, to, read, assertions, it) }
    public fun diff(from: String, to: String?, read: OntologyRead, assertions: Boolean, operation: SparklesOperation): JsonObject {
        val args = read.selection()
        args.put("from", from)
        to?.let { args.put("to", it) }
        args.put("assertions", assertions)
        return call("diff", args, operation, false)
    }
}
