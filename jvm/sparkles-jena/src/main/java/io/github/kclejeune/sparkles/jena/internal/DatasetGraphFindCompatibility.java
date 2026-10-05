package io.github.kclejeune.sparkles.jena.internal;

import java.util.Iterator;
import java.util.Spliterator;
import java.util.Spliterators;
import java.util.stream.Stream;
import java.util.stream.StreamSupport;
import org.apache.jena.graph.Node;
import org.apache.jena.sparql.core.DatasetGraphBaseFind;
import org.apache.jena.sparql.core.Quad;

/** Jena 6's added stream hooks, compiled against the shared Jena 5 API. */
public abstract class DatasetGraphFindCompatibility extends DatasetGraphBaseFind {
    private static Stream<Quad> streamOf(Iterator<Quad> iterator) {
        return StreamSupport.stream(Spliterators.spliteratorUnknownSize(iterator, Spliterator.ORDERED), false)
                .onClose(() -> org.apache.jena.atlas.iterator.Iter.close(iterator));
    }
    // These deliberately have no @Override: Jena 5 has no corresponding hooks.
    protected Stream<Quad> streamInDftGraph(Node s, Node p, Node o) {
        return streamOf(findInDftGraph(s, p, o));
    }
    protected Stream<Quad> streamInSpecificNamedGraph(Node g, Node s, Node p, Node o) {
        return streamOf(findInSpecificNamedGraph(g, s, p, o));
    }
    protected Stream<Quad> streamInAnyNamedGraphs(Node s, Node p, Node o) {
        return streamOf(findInAnyNamedGraphs(s, p, o));
    }
}
