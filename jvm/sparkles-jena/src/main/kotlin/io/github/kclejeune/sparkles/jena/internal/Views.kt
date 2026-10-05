package io.github.kclejeune.sparkles.jena.internal

import io.github.kclejeune.sparkles.jena.DatasetGraphSparkles
import org.apache.jena.graph.Node
import org.apache.jena.graph.Triple
import org.apache.jena.graph.GraphEvents
import org.apache.jena.graph.GraphUtil
import org.apache.jena.riot.system.PrefixMapBase
import org.apache.jena.shared.AddDeniedException
import org.apache.jena.shared.DeleteDeniedException
import org.apache.jena.sparql.core.GraphView
import org.apache.jena.sparql.core.Quad
import org.apache.jena.util.iterator.ExtendedIterator
import org.apache.jena.util.iterator.WrappedIterator
import java.util.Collections

/**
 * A graph of a Sparkles dataset. Jena's [GraphView] keeps the graph semantics and the
 * event manager; size, contains, find and clear are single native calls.
 */
internal class GraphViewSparkles(private val dsg: DatasetGraphSparkles, private val gn: Node) : GraphView(dsg, gn) {
    private val node: Node get() = gn

    override fun graphBaseFind(s: Node?, p: Node?, o: Node?): ExtendedIterator<Triple> =
        WrappedIterator.createNoRemove(TripleIter(dsg.findQuads(node, s, p, o, namedOnly = false)))

    override fun graphBaseFind(m: Triple?): ExtendedIterator<Triple> {
        val t = m ?: Triple.ANY
        return graphBaseFind(t.subject, t.predicate, t.`object`)
    }

    override fun graphBaseSize(): Int = dsg.countQuads(node, null, null, null).coerceAtMost(Int.MAX_VALUE.toLong()).toInt()

    override fun graphBaseContains(t: Triple): Boolean =
        dsg.contains(node, t.subject, t.predicate, t.`object`)

    override fun isEmpty(): Boolean = graphBaseSize() == 0

    override fun performAdd(t: Triple) {
        if (Quad.isUnionGraph(gn)) throw AddDeniedException("Can't update the union graph of a dataset")
        dsg.add(node, t.subject, t.predicate, t.`object`)
    }

    override fun performDelete(t: Triple) {
        if (Quad.isUnionGraph(gn)) throw DeleteDeniedException("Can't update the union graph of a dataset")
        dsg.delete(node, t.subject, t.predicate, t.`object`)
    }

    override fun remove(s: Node?, p: Node?, o: Node?) {
        if (eventManager.listening()) {
            // match by match, so that listeners see each one, as GraphBase does
            GraphUtil.remove(this, s, p, o)
            eventManager.notifyEvent(this, GraphEvents.remove(s, p, o))
            return
        }
        if (Quad.isUnionGraph(gn)) throw DeleteDeniedException("Can't update the union graph of a dataset")
        dsg.deleteAny(node, s ?: Node.ANY, p ?: Node.ANY, o ?: Node.ANY)
    }

    override fun clear() {
        if (Quad.isUnionGraph(gn)) throw DeleteDeniedException("Can't update the union graph of a dataset")
        dsg.deleteAny(node, Node.ANY, Node.ANY, Node.ANY)
        eventManager.notifyEvent(this, GraphEvents.removeAll)
    }
}

/**
 * The dataset's prefixes, which every graph view shares. Sparkles saves a change at once
 * and outside any transaction (P04 §3.2).
 */
internal class PrefixMapSparkles(private val handle: Handle) : PrefixMapBase() {
    private val map get() = handle.prefixes

    override fun get(prefix: String): String? = map[prefix]

    override fun getMapping(): Map<String, String> = Collections.unmodifiableMap(map)

    override fun add(prefix: String, iriString: String) {
        handle.checkOpen()
        if (map[prefix] == iriString) return
        ffi { handle.ffi.setPrefix(prefix, iriString) }
        map[prefix] = iriString
    }

    override fun delete(prefix: String) {
        handle.checkOpen()
        if (!map.containsKey(prefix)) return
        ffi { handle.ffi.removePrefix(prefix) }
        map.remove(prefix)
    }

    override fun clear() {
        for (p in map.keys.toList()) delete(p)
    }

    override fun containsPrefix(prefix: String): Boolean = map.containsKey(prefix)

    override fun isEmpty(): Boolean = map.isEmpty()

    override fun size(): Int = map.size
}
