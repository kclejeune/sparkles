// The reference answers of tests/hybrid_rules.rs: runs hybrid.rules over data.nt with
// Apache Jena's GenericRuleReasoner in HYBRID mode and writes, as sorted N-Triples, the
// forward deductions to jena-deductions.nt and every triple the model answers for each
// predicate named on the command line to jena-answers.nt.
//
// Usage: java -cp "$JENA/lib/*:." JenaHybrid data.nt hybrid.rules OUTDIR PREDICATE...
import java.io.*;
import java.nio.file.*;
import java.util.*;
import org.apache.jena.rdf.model.*;
import org.apache.jena.reasoner.rulesys.*;
import org.apache.jena.riot.RDFDataMgr;
import org.apache.jena.riot.Lang;

public class JenaHybrid {
    static List<String> lines(Model m) {
        StringWriter w = new StringWriter();
        RDFDataMgr.write(w, m, Lang.NTRIPLES);
        TreeSet<String> out = new TreeSet<>(Arrays.asList(w.toString().split("\n")));
        out.remove("");
        return new ArrayList<>(out);
    }

    public static void main(String[] args) throws Exception {
        Model data = RDFDataMgr.loadModel(args[0]);
        List<Rule> rules = Rule.rulesFromURL(args[1]);
        GenericRuleReasoner reasoner = new GenericRuleReasoner(rules);
        reasoner.setMode(GenericRuleReasoner.HYBRID);
        InfModel inf = ModelFactory.createInfModel(reasoner, data);
        Files.write(Paths.get(args[2], "jena-deductions.nt"), lines(inf.getDeductionsModel()));
        Model answers = ModelFactory.createDefaultModel();
        for (int i = 3; i < args.length; i++) {
            answers.add(inf.listStatements(null, inf.createProperty(args[i]), (RDFNode) null));
        }
        Files.write(Paths.get(args[2], "jena-answers.nt"), lines(answers));
    }
}
