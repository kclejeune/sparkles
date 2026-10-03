// Writes jena-1.rdfp and jena-1.trp, the same patch in the text and binary forms, with
// the writers of Apache Jena (jena-rdfpatch). From this directory:
//   java -cp "$JENA_HOME/lib/*" GenPatch.java jena-1
// The files in this directory were written by Jena 6.2.0. jena-syntax-1.rdfp is
// jena-rdfpatch's testing/files/syntax-1.rdfp (Apache-2.0).
import java.io.*;
import org.apache.jena.graph.*;
import org.apache.jena.rdfpatch.*;
import org.apache.jena.rdfpatch.binary.RDFChangesWriterBinary;
import org.apache.jena.rdfpatch.changes.RDFChangesCollector;
import org.apache.jena.sparql.sse.SSE;

public class GenPatch {
    public static void main(String[] a) throws Exception {
        RDFChangesCollector c = new RDFChangesCollector();
        c.start();
        c.header("id", NodeFactory.createURI("uuid:bbe2edae-325e-11ec-abcc-a70bbba0dfb1"));
        c.txnBegin();
        c.addPrefix(null, "ex", "http://example/");
        c.addPrefix(NodeFactory.createURI("http://example/g"), "foaf", "http://xmlns.com/foaf/0.1/");
        Node g = NodeFactory.createURI("http://example/g");
        Node s = NodeFactory.createURI("http://example/s");
        Node p = NodeFactory.createURI("http://example/p");
        c.add(g, s, p, NodeFactory.createURI("http://example/o1"));
        c.add(null, s, p, SSE.parseNode("123"));
        c.add(null, s, p, SSE.parseNode("12.5"));
        c.add(null, s, p, SSE.parseNode("1.0e0"));
        c.add(null, s, p, SSE.parseNode("true"));
        c.add(null, s, p, SSE.parseNode("'abc\\ndef'"));
        c.add(null, s, p, SSE.parseNode("'chat'@fr"));
        c.add(null, s, p, SSE.parseNode("'2026-10-02'^^<http://www.w3.org/2001/XMLSchema#date>"));
        c.add(null, NodeFactory.createBlankNode("b1"), p, NodeFactory.createBlankNode("b2"));
        c.add(g, s, p, SSE.parseNode("<<( _:b1 <http://example/q> 'x' )>>"));
        c.delete(g, s, p, NodeFactory.createURI("http://example/o1"));
        c.deletePrefix(null, "ex");
        c.txnCommit();
        c.finish();
        RDFPatch patch = c.getRDFPatch();
        try (OutputStream out = new FileOutputStream(a[0] + ".rdfp")) { RDFPatchOps.write(out, patch); }
        try (OutputStream out = new FileOutputStream(a[0] + ".trp")) { RDFChangesWriterBinary.write(patch, out); }
    }
}
