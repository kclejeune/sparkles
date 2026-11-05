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
     * aborts undoes its writes in the copy too, since the triples a transaction writes are
     * read back from the dataset once it ends. Another commit made through another handle
     * after the model is built is not seen by it, unless it writes those triples.
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

    /** One page of the entities of the graph, as [SparklesOntology.entities] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun entities(dataset: DatasetGraphSparkles, graphIri: String? = null, kinds: List<String> = emptyList(), limit: Int = 100, offset: Long = 0): JsonObject =
        dataset.ontology().entities(OntologyRead(graph = graphIri), kinds, limit, offset)

    /** Search the entities of the graph's ontology with typos tolerated, as [SparklesOntology.search] does. */
    @JvmStatic
    @JvmOverloads
    public fun search(dataset: DatasetGraphSparkles, query: String, graphIri: String? = null, kinds: List<String> = emptyList(), limit: Int = 10): JsonObject =
        dataset.ontology().search(query, OntologyRead(graph = graphIri), kinds, limit = limit)

    /** One level of the class hierarchy of the graph, as [SparklesOntology.hierarchy] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun hierarchy(dataset: DatasetGraphSparkles, iri: String? = null, graphIri: String? = null, kind: String = "class", ancestors: Boolean = false): JsonObject =
        dataset.ontology().hierarchy(iri, OntologyRead(graph = graphIri), kind, ancestors)

    /** A page of asserted named individuals or type buckets, as [SparklesOntology.individuals] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun individuals(dataset: DatasetGraphSparkles, graphIri: String? = null, type: String? = null, untyped: Boolean = false, iri: String? = null, after: String? = null, limit: Int = 100): JsonObject =
        dataset.ontology().individuals(OntologyRead(graph = graphIri), type, untyped, iri, after, limit)

    /** Read kept classification state without starting a classifier. */
    @JvmStatic
    @JvmOverloads
    public fun keptClassification(dataset: DatasetGraphSparkles, graphIri: String? = null, query: KeptClassificationQuery = KeptClassificationQuery()): JsonObject =
        dataset.ontology().keptClassification(OntologyRead(graph = graphIri), query)

    /** What the graph says about one entity, read without the whole ontology, as [SparklesOntology.term] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun term(dataset: DatasetGraphSparkles, iri: String, graphIri: String? = null): JsonObject =
        dataset.ontology().term(iri, OntologyRead(graph = graphIri))

    /** What the ontology of the graph says about one entity, as [SparklesOntology.entity] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun entity(dataset: DatasetGraphSparkles, iri: String, graphIri: String? = null, kind: String? = null, assertions: Boolean = false): JsonObject =
        dataset.ontology().entity(iri, OntologyRead(graph = graphIri), kind, assertions)

    /** Remove an entity of the graph in one commit, as [SparklesOntology.removeEntity] does. */
    @JvmStatic
    @JvmOverloads
    public fun removeEntity(dataset: DatasetGraphSparkles, iri: String, graphIri: String? = null): JsonObject =
        dataset.ontology().removeEntity(iri, OntologyRead(graph = graphIri))

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

    /** The OWL 2 EL classification of the graph, as [SparklesOntology.classify] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun classify(dataset: DatasetGraphSparkles, graphIri: String? = null, query: ClassifyQuery = ClassifyQuery()): JsonObject =
        dataset.ontology().classify(OntologyRead(graph = graphIri), query)

    /** Manchester text read against the graph's names and labels, as [SparklesOntology.parse] reads it. */
    @JvmStatic
    @JvmOverloads
    public fun parse(dataset: DatasetGraphSparkles, text: String, `as`: String = "classExpression", graphIri: String? = null): JsonObject =
        dataset.ontology().parse(text, `as`, OntologyRead(graph = graphIri))

    /** A DL query of the graph, as [SparklesOntology.dlQuery] answers it. */
    @JvmStatic
    @JvmOverloads
    public fun dlQuery(dataset: DatasetGraphSparkles, query: DlQuery, graphIri: String? = null): JsonObject =
        dataset.ontology().dlQuery(query, OntologyRead(graph = graphIri))
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
                val found = copy.find(ms, mp, mo).toList()
                // a predecessor's noted triples may be missing from the copy until settled;
                // the caller's own stay noted
                if (pending === dataset.txn()) found else found + journal.filter { it.matches(ms, mp, mo) }
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

    /** One page of the entities of the kinds `kinds` (all kinds when empty), each with its kind and IRI. */
    @JvmOverloads public fun entities(read: OntologyRead = OntologyRead(), kinds: List<String> = emptyList(), limit: Int = 100, offset: Long = 0): JsonObject =
        SparklesOperation().use { entities(read, kinds, limit, offset, it) }
    public fun entities(read: OntologyRead, kinds: List<String>, limit: Int, offset: Long, operation: SparklesOperation): JsonObject {
        require(limit > 0 && offset >= 0)
        val args = read.value()
        if (kinds.isNotEmpty()) args.put("kind", kinds.joinToString(","))
        args.put("limit", limit.toLong())
        args.put("offset", offset)
        return call("entities", args, operation, false)
    }

    /**
     * Search the entities by label, synonym, short form and definition, with typos
     * tolerated, ranked as `GET /$/ontology/{ds}/search` ranks them: one page of `limit`
     * hits from `offset`. `kinds` keeps some kinds, such as `class` or `concept`, and
     * `languages` lists the preferred languages, most preferred first. The search reads
     * the head, so `read` names neither `at` nor `inferences`.
     */
    @JvmOverloads public fun search(
        query: String,
        read: OntologyRead = OntologyRead(),
        kinds: List<String> = emptyList(),
        languages: List<String> = emptyList(),
        limit: Int = 10,
        offset: Long = 0,
        fuzzy: Boolean = true,
        definitions: Boolean = true,
    ): JsonObject = SparklesOperation().use { search(query, read, kinds, languages, limit, offset, fuzzy, definitions, it) }
    public fun search(
        query: String,
        read: OntologyRead,
        kinds: List<String>,
        languages: List<String>,
        limit: Int,
        offset: Long,
        fuzzy: Boolean,
        definitions: Boolean,
        operation: SparklesOperation,
    ): JsonObject {
        require(limit > 0 && offset >= 0)
        val args = read.value()
        args.put("q", query)
        if (kinds.isNotEmpty()) args.put("kind", kinds.joinToString(","))
        if (languages.isNotEmpty()) args.put("lang", languages.joinToString(","))
        args.put("limit", limit.toLong())
        args.put("offset", offset)
        args.put("fuzzy", fuzzy)
        args.put("definitions", definitions)
        return call("search", args, operation, false)
    }

    /**
     * One level of the class, property or concept hierarchy, read from the store's indexes
     * without reading the whole ontology: the children of `iri` (the roots when it is
     * null), a page of `limit` from `offset`, each with the numbers of its own children
     * and parents, and the ancestors of `iri` with `ancestors`. `kind` is `class`,
     * `objectProperty`, `dataProperty`, `annotationProperty` or `concept`.
     */
    @JvmOverloads public fun hierarchy(
        iri: String? = null,
        read: OntologyRead = OntologyRead(),
        kind: String = "class",
        ancestors: Boolean = false,
        scheme: String? = null,
        deprecated: Boolean = true,
        languages: List<String> = emptyList(),
        limit: Int = 100,
        offset: Long = 0,
    ): JsonObject = SparklesOperation().use { hierarchy(iri, read, kind, ancestors, scheme, deprecated, languages, limit, offset, it) }
    public fun hierarchy(
        iri: String?,
        read: OntologyRead,
        kind: String,
        ancestors: Boolean,
        scheme: String?,
        deprecated: Boolean,
        languages: List<String>,
        limit: Int,
        offset: Long,
        operation: SparklesOperation,
    ): JsonObject {
        require(limit > 0 && offset >= 0)
        val args = read.value()
        if (iri != null) args.put("iri", iri)
        args.put("kind", kind)
        args.put("ancestors", ancestors)
        if (scheme != null) args.put("scheme", scheme)
        args.put("deprecated", deprecated)
        if (languages.isNotEmpty()) args.put("lang", languages.joinToString(","))
        args.put("limit", limit.toLong())
        args.put("offset", offset)
        return call("hierarchy", args, operation, false)
    }

    /**
     * A lexical page of asserted named individuals or their type buckets. A type,
     * Untyped selector or individual IRI is optional and mutually exclusive. Continue
     * with the returned cursor and the returned commit in [OntologyRead.at].
     */
    @JvmOverloads public fun individuals(
        read: OntologyRead = OntologyRead(),
        type: String? = null,
        untyped: Boolean = false,
        iri: String? = null,
        after: String? = null,
        limit: Int = 100,
        languages: List<String> = emptyList(),
    ): JsonObject = SparklesOperation().use { individuals(read, type, untyped, iri, after, limit, languages, it) }
    public fun individuals(
        read: OntologyRead,
        type: String?,
        untyped: Boolean,
        iri: String?,
        after: String?,
        limit: Int,
        languages: List<String>,
        operation: SparklesOperation,
    ): JsonObject {
        require(limit in 1..1000)
        val args = read.value()
        type?.let { args.put("type", it) }
        args.put("untyped", untyped)
        iri?.let { args.put("iri", it) }
        after?.let { args.put("after", it) }
        args.put("limit", limit.toLong())
        if (languages.isNotEmpty()) args.put("lang", languages.joinToString(","))
        return call("individuals", args, operation, false)
    }

    /** Read existing immutable classification state only. Missing state never computes.
     * A continuation needs [KeptClassificationQuery.resultId] and the returned commit
     * in [OntologyRead.at]. Restricted views report unavailable. Use a live dataset
     * outside a transaction; pinned dataset wrappers are refused rather than routed
     * to head. Retained history is selected with [OntologyRead.at] on that live dataset. */
    @JvmOverloads public fun keptClassification(read: OntologyRead = OntologyRead(), query: KeptClassificationQuery = KeptClassificationQuery()): JsonObject =
        SparklesOperation().use { keptClassification(read, query, it) }
    public fun keptClassification(read: OntologyRead, query: KeptClassificationQuery, operation: SparklesOperation): JsonObject {
        owner.checkCapture("ontology.keptClassification (use OntologyRead.at for retained history)")
        require(!read.inferences) { "kept classification reads asserted inputs" }
        val args = read.value()
        query.into(args)
        return call("keptClassification", args, operation, false)
    }

    /**
     * What the graphs say about the entity `iri`, read without reading the whole ontology:
     * its statements with their annotations, the axioms they map to, the axioms of other
     * entities that mention it (at most `mentions`), the statements that point at it with
     * `incoming` subjects each, and the label, short form and kinds of every IRI used.
     */
    @JvmOverloads public fun term(
        iri: String,
        read: OntologyRead = OntologyRead(),
        mentions: Int = 200,
        incoming: Int = 20,
        languages: List<String> = emptyList(),
    ): JsonObject = SparklesOperation().use { term(iri, read, mentions, incoming, languages, it) }
    public fun term(
        iri: String,
        read: OntologyRead,
        mentions: Int,
        incoming: Int,
        languages: List<String>,
        operation: SparklesOperation,
    ): JsonObject {
        require(mentions >= 0 && incoming >= 0)
        val args = read.value()
        args.put("iri", iri)
        args.put("mentions", mentions.toLong())
        args.put("incoming", incoming.toLong())
        if (languages.isNotEmpty()) args.put("lang", languages.joinToString(","))
        return call("term", args, operation, false)
    }

    /**
     * What the ontology says about the entity `iri`: its kinds, labels and annotations, and
     * for each kind its superclasses, domains, ranges, types and the other accessors. With
     * `assertions`, the instances, types and property assertions the store holds too, at
     * most `limit` of each.
     */
    @JvmOverloads public fun entity(iri: String, read: OntologyRead = OntologyRead(), kind: String? = null, assertions: Boolean = false, limit: Int = 100): JsonObject =
        SparklesOperation().use { entity(iri, read, kind, assertions, limit, it) }
    public fun entity(iri: String, read: OntologyRead, kind: String?, assertions: Boolean, limit: Int, operation: SparklesOperation): JsonObject {
        require(limit > 0)
        val args = read.value()
        args.put("iri", iri)
        kind?.let { args.put("kind", it) }
        args.put("assertions", assertions)
        args.put("limit", limit.toLong())
        return call("entity", args, operation, false)
    }

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

    /**
     * Classify by OWL 2 EL (spec F24): consistency, the unsatisfiable classes, the axioms
     * left out or used in part, and with [ClassifyQuery] one class and a page of the
     * taxonomy's nodes. A query with a `target` writes the taxonomy to that named graph in
     * one commit.
     */
    @JvmOverloads public fun classify(read: OntologyRead = OntologyRead(), query: ClassifyQuery = ClassifyQuery()): JsonObject =
        SparklesOperation().use { classify(read, query, it) }
    public fun classify(read: OntologyRead, query: ClassifyQuery, operation: SparklesOperation): JsonObject {
        if (query.projection != null) owner.checkCapture("ontology frame projection (use OntologyRead.at for retained history)")
        val args = read.value()
        query.into(args)
        return call("classify", args, operation, query.target != null)
    }

    /**
     * Manchester text read against the ontology's names and labels as `as`
     * (`classExpression`, `dataRange`, `individual`, `literal` or `axiom`): the value in its
     * JSON form, `ofn` and `manchester`. Text that does not read throws, with the span and,
     * for an ambiguous name, the candidates in the message.
     */
    @JvmOverloads public fun parse(text: String, `as`: String = "classExpression", read: OntologyRead = OntologyRead()): JsonObject =
        SparklesOperation().use { parse(text, `as`, read, it) }
    public fun parse(text: String, `as`: String, read: OntologyRead, operation: SparklesOperation): JsonObject {
        val args = read.value()
        args.put("text", text)
        args.put("as", `as`)
        return call("parse", args, operation, false)
    }

    /**
     * A DL query (spec U12): the equivalents, superclasses, subclasses and instances of a
     * class expression. The EL classifier answers inside OWL 2 EL and the RL rules outside
     * it, and the answer's `reasoning` names the engine and whether the answers are complete.
     */
    @JvmOverloads public fun dlQuery(query: DlQuery, read: OntologyRead = OntologyRead()): JsonObject =
        SparklesOperation().use { dlQuery(query, read, it) }
    public fun dlQuery(query: DlQuery, read: OntologyRead, operation: SparklesOperation): JsonObject {
        val args = read.value()
        query.into(args)
        return call("dlQuery", args, operation, false)
    }
}

/**
 * A DL query: the class expression in Manchester syntax, or in functional-style syntax with
 * `syntax = "functional"`, the engine (`auto`, `el` or `rl`), the answers wanted (every one
 * when empty) and the most IRIs each answer lists.
 */
public data class DlQuery @JvmOverloads public constructor(
    public val expression: String,
    public val syntax: String = "manchester",
    public val reasoning: String = "auto",
    public val answers: List<String> = emptyList(),
    public val limit: Int = 1000,
    public val threads: Int = 0,
) {
    init {
        require(limit > 0 && threads >= 0)
    }

    internal fun into(j: JsonObject) {
        j.put("expression", expression)
        j.put("syntax", syntax)
        j.put("reasoning", reasoning)
        if (answers.isNotEmpty()) j.put("answers", JsonArray().also { a -> answers.forEach { a.add(it) } })
        j.put("limit", limit.toLong())
        j.put("threads", threads.toLong())
    }
}

/** What a classification answers besides its counts, and where it writes the taxonomy. */
public data class ClassifyQuery @JvmOverloads public constructor(
    public val assertions: Boolean = false,
    public val about: String? = null,
    public val nodes: Boolean = false,
    public val limit: Int = 100,
    public val offset: Long = 0,
    public val threads: Int = 0,
    public val target: String? = null,
    public val message: String? = null,
    /** answer what a run with the disjunctive rules proved when the operation's deadline or cancellation stops it */
    public val partial: Boolean = false,
    /** the expansion strategy of the disjunctive rules, `cautious` (the default) or `eager` */
    public val expansion: String? = null,
    /** `frame` returns bounded direct-parent/equivalence clauses instead of legacy arrays. */
    public val projection: String? = null,
    public val languages: List<String> = emptyList(),
) {
    init {
        require(limit > 0 && offset >= 0 && threads >= 0)
        require(expansion == null || expansion == "cautious" || expansion == "eager")
    }

    internal fun into(j: JsonObject) {
        j.put("assertions", assertions)
        about?.let { j.put("about", it) }
        j.put("nodes", nodes)
        j.put("limit", limit.toLong())
        j.put("offset", offset)
        j.put("threads", threads.toLong())
        target?.let { j.put("target", it) }
        message?.let { j.put("message", it) }
        if (partial) j.put("partial", true)
        expansion?.let { j.put("expansion", it) }
        projection?.let { j.put("projection", it) }
        if (languages.isNotEmpty()) j.put("lang", languages.joinToString(","))
    }
}

/** A bounded read from one kept classification. Nonzero offsets require an exact pin. */
public data class KeptClassificationQuery @JvmOverloads public constructor(
    public val part: String = "status",
    public val about: String? = null,
    public val offset: Long = 0,
    public val limit: Int = 100,
    public val resultId: String? = null,
    public val languages: List<String> = emptyList(),
) {
    internal fun into(j: JsonObject) {
        require(limit in 1..1000 && offset >= 0)
        j.put("part", part)
        about?.let { j.put("about", it) }
        j.put("offset", offset)
        j.put("limit", limit.toLong())
        resultId?.let { j.put("resultId", it) }
        if (languages.isNotEmpty()) j.put("lang", languages.joinToString(","))
    }
}
