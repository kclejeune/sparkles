// Classifies an ontology with an OWL API reasoner, for the comparison in
// docs/benchmarks/el-classification. nix/reasoners/default.nix compiles this file against
// each reasoner's pinned jars, and the packages' `elk-classify`, `hermit-classify` and
// `whelk-classify` wrappers run it. scripts/elk-compare/elk.sh runs those wrappers with
// explicit heap settings.
//
//   ElkClassify [--reasoner elk|hermit|whelk] [--threads N] [--repeat N] [--taxonomy FILE] INPUT
//
// --reasoner hermit runs HermiT, a complete OWL 2 DL reasoner, and --reasoner whelk runs
// Whelk, an OWL 2 EL reasoner in Scala, in place of ELK. Both run with their default
// configuration and on one thread, which the JSON line reports as their threads. The
// reasoners are loaded by name, so the file compiles against the OWL API alone, version
// 5 for ELK and HermiT and version 4.5 for Whelk, and it calls only the methods that both
// versions have.
//
// Each of the N runs loads INPUT with a fresh OWL API manager, creates a reasoner, with N
// worker threads for ELK, and precomputes the class hierarchy. loadMs is the OWL API's
// parse of the file, and classifyMs runs from the creation of the reasoner to the end of
// precomputeInferences(CLASS_HIERARCHY), which includes the reasoner's own loading of the
// axioms from the OWL API ontology and the construction of its taxonomy. After each run a
// full GC with the reasoner and the ontology still alive gives retainedHeapMib, the live
// heap they hold. A JSON line on standard output gives the times of every run, the counts,
// the peak resident memory of the process and the heap settings.
//
// --taxonomy writes the direct hierarchy of the last run in the form that
// scripts/el-oracle.py reads: EquivalentClasses for each node of several classes, with the
// unsatisfiable classes in the node of owl:Nothing, Declaration(Class) for each class of a
// node of its own and SubClassOf from each node's first class to the first class of each of
// its direct superclass nodes. An inconsistent ontology gets one node of every class,
// owl:Thing and owl:Nothing, as the classifier of sparkles-owl writes it.
//
// el_classify reads the axioms of its input file only, so an input that declares imports is
// refused here rather than classified with a different import closure. Missing imports do
// not stop the load before that check.

import java.io.File;
import java.lang.management.ManagementFactory;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.Collections;
import java.util.Comparator;
import java.util.HashSet;
import java.util.List;
import java.util.Set;
import java.util.TreeSet;
import java.util.stream.Collectors;

import org.semanticweb.owlapi.apibinding.OWLManager;
import org.semanticweb.owlapi.formats.FunctionalSyntaxDocumentFormat;
import org.semanticweb.owlapi.io.FileDocumentSource;
import org.semanticweb.owlapi.io.OWLOntologyDocumentSource;
import org.semanticweb.owlapi.model.MissingImportHandlingStrategy;
import org.semanticweb.owlapi.model.OWLClass;
import org.semanticweb.owlapi.model.OWLOntology;
import org.semanticweb.owlapi.model.OWLOntologyManager;
import org.semanticweb.owlapi.model.parameters.Imports;
import org.semanticweb.owlapi.reasoner.InconsistentOntologyException;
import org.semanticweb.owlapi.reasoner.InferenceType;
import org.semanticweb.owlapi.reasoner.Node;
import org.semanticweb.owlapi.reasoner.OWLReasoner;
import org.semanticweb.owlapi.reasoner.OWLReasonerConfiguration;
import org.semanticweb.owlapi.reasoner.OWLReasonerFactory;

public class ElkClassify {
    static final String THING = "http://www.w3.org/2002/07/owl#Thing";
    static final String NOTHING = "http://www.w3.org/2002/07/owl#Nothing";

    public static void main(String[] args) throws Exception {
        int threads = Runtime.getRuntime().availableProcessors();
        int repeat = 1;
        String taxonomy = null;
        String input = null;
        String which = "elk";
        for (int i = 0; i < args.length; i++) {
            switch (args[i]) {
                case "--threads": threads = Integer.parseInt(args[++i]); break;
                case "--repeat": repeat = Integer.parseInt(args[++i]); break;
                case "--taxonomy": taxonomy = args[++i]; break;
                case "--reasoner": which = args[++i]; break;
                case "--self-test": selfTest(); return;
                default: input = args[i];
            }
        }
        if (input == null || threads < 1 || repeat < 1 || !java.util.Arrays.asList("elk", "hermit", "whelk").contains(which)) {
            System.err.println("usage: ElkClassify [--reasoner elk|hermit|whelk] [--threads N] [--repeat N] [--taxonomy FILE] INPUT");
            System.exit(2);
        }
        File file = new File(input);
        boolean functional = input.endsWith(".ofn") || startsFunctional(file);
        List<Double> loadMs = new ArrayList<>();
        List<Double> classifyMs = new ArrayList<>();
        List<Double> cpuMs = new ArrayList<>();
        List<Double> retainedMib = new ArrayList<>();
        int classes = 0;
        int logicalAxioms = 0;
        boolean consistent = true;
        int unsatisfiable = 0;
        com.sun.management.OperatingSystemMXBean os =
            (com.sun.management.OperatingSystemMXBean) ManagementFactory.getOperatingSystemMXBean();
        for (int run = 0; run < repeat; run++) {
            OWLOntologyManager manager = OWLManager.createOWLOntologyManager();
            manager.setOntologyLoaderConfiguration(manager.getOntologyLoaderConfiguration()
                .setMissingImportHandlingStrategy(MissingImportHandlingStrategy.SILENT));
            OWLOntologyDocumentSource source = functional
                ? new FileDocumentSource(file, new FunctionalSyntaxDocumentFormat())
                : new FileDocumentSource(file);
            long t0 = System.nanoTime();
            OWLOntology ontology = manager.loadOntologyFromOntologyDocument(source);
            long t1 = System.nanoTime();
            if (!ontology.getImportsDeclarations().isEmpty()) {
                System.err.println(input + " declares imports, which el_classify does not read");
                System.exit(3);
            }
            long c0 = os.getProcessCpuTime();
            long t2 = System.nanoTime();
            OWLReasoner reasoner = which.equals("elk")
                ? elk(ontology, threads)
                : factory(which).createReasoner(ontology);
            // ELK answers an inconsistent ontology with an exception once it has found the
            // inconsistency, which ends that classification.
            try {
                reasoner.precomputeInferences(InferenceType.CLASS_HIERARCHY);
                consistent = reasoner.isConsistent();
            } catch (InconsistentOntologyException e) {
                consistent = false;
            }
            long t3 = System.nanoTime();
            long c1 = os.getProcessCpuTime();
            loadMs.add((t1 - t0) / 1e6);
            classifyMs.add((t3 - t2) / 1e6);
            cpuMs.add((c1 - c0) / 1e6);
            System.gc();
            Runtime rt = Runtime.getRuntime();
            retainedMib.add((rt.totalMemory() - rt.freeMemory()) / 1048576.0);
            classes = ontology.getClassesInSignature(Imports.INCLUDED).size();
            logicalAxioms = ontology.getLogicalAxiomCount(Imports.INCLUDED);
            // An inconsistent ontology makes every class unsatisfiable, owl:Thing included,
            // as the taxonomy below writes it.
            unsatisfiable = consistent
                ? reasoner.getUnsatisfiableClasses().getEntitiesMinusBottom().size()
                : (int) ontology.getClassesInSignature(Imports.INCLUDED).stream()
                    .filter(c -> !c.isOWLNothing() && !c.isOWLThing()).count() + 1;
            if (run == repeat - 1 && taxonomy != null) {
                writeTaxonomy(ontology, reasoner, consistent, new File(taxonomy));
            }
            reasoner.dispose();
            for (OWLOntology o : new ArrayList<>(manager.getOntologies())) {
                manager.removeOntology(o);
            }
        }
        StringBuilder out = new StringBuilder("{");
        field(out, "input", "\"" + input.replace("\\", "\\\\").replace("\"", "\\\"") + "\"");
        field(out, "reasoner", "\"" + NAMES.get(which) + " "
            + factory(which).getClass().getPackage().getImplementationVersion() + "\"");
        field(out, "java", "\"" + System.getProperty("java.vm.name") + " " + System.getProperty("java.version") + "\"");
        // HermiT and Whelk classify on one thread whatever --threads asks for.
        field(out, "threads", Integer.toString(which.equals("elk") ? threads : 1));
        field(out, "classes", Integer.toString(classes));
        field(out, "logicalAxioms", Integer.toString(logicalAxioms));
        field(out, "consistent", Boolean.toString(consistent));
        field(out, "unsatisfiable", Integer.toString(unsatisfiable));
        field(out, "loadMs", list(loadMs));
        field(out, "classifyMs", list(classifyMs));
        field(out, "classifyCpuMs", list(cpuMs));
        field(out, "medianLoadMs", fmt(median(loadMs)));
        field(out, "medianClassifyMs", fmt(median(classifyMs)));
        List<Double> total = new ArrayList<>();
        for (int i = 0; i < loadMs.size(); i++) {
            total.add(loadMs.get(i) + classifyMs.get(i));
        }
        field(out, "medianLoadAndClassifyMs", fmt(median(total)));
        field(out, "retainedHeapMib", list(retainedMib));
        field(out, "maxHeapMib", Long.toString(Runtime.getRuntime().maxMemory() / 1048576));
        field(out, "jvmArgs", "[" + ManagementFactory.getRuntimeMXBean().getInputArguments().stream()
            .map(a -> "\"" + a.replace("\\", "\\\\").replace("\"", "\\\"") + "\"")
            .collect(Collectors.joining(",")) + "]");
        field(out, "peakRssKib", Long.toString(peakRssKib()));
        out.setLength(out.length() - 1);
        out.append('}');
        System.out.println(out);
    }

    static final java.util.Map<String, String> NAMES = java.util.Map.of(
        "elk", "ELK", "hermit", "HermiT", "whelk", "Whelk");
    static final java.util.Map<String, String> FACTORIES = java.util.Map.of(
        "elk", "org.semanticweb.elk.owlapi.ElkReasonerFactory",
        "hermit", "org.semanticweb.HermiT.ReasonerFactory",
        "whelk", "org.geneontology.whelk.owlapi.WhelkOWLReasonerFactory");

    static OWLReasonerFactory factory(String which) throws Exception {
        return (OWLReasonerFactory) Class.forName(FACTORIES.get(which)).getDeclaredConstructor().newInstance();
    }

    // ELK with N worker threads, through its configuration classes by name:
    // new ElkReasonerConfiguration(getDefaultOwlReasonerConfiguration(), elk) where elk is
    // ReasonerConfiguration.getConfiguration() with NUM_OF_WORKING_THREADS set.
    static OWLReasoner elk(OWLOntology ontology, int threads) throws Exception {
        Class<?> rc = Class.forName("org.semanticweb.elk.reasoner.config.ReasonerConfiguration");
        Object elk = rc.getMethod("getConfiguration").invoke(null);
        rc.getMethod("setParameter", String.class, String.class)
            .invoke(elk, rc.getField("NUM_OF_WORKING_THREADS").get(null), Integer.toString(threads));
        Class<?> erc = Class.forName("org.semanticweb.elk.owlapi.ElkReasonerConfiguration");
        Object owl = erc.getMethod("getDefaultOwlReasonerConfiguration").invoke(null);
        Object config = erc.getConstructor(OWLReasonerConfiguration.class, rc).newInstance(owl, elk);
        return factory("elk").createReasoner(ontology, (OWLReasonerConfiguration) config);
    }

    static boolean startsFunctional(File file) throws Exception {
        byte[] head = new byte[4096];
        int n;
        try (java.io.InputStream in = Files.newInputStream(file.toPath())) {
            n = in.read(head);
        }
        String s = new String(head, 0, Math.max(n, 0), StandardCharsets.UTF_8).trim();
        return s.startsWith("Prefix(") || s.startsWith("Ontology(");
    }

    static void writeTaxonomy(OWLOntology ontology, OWLReasoner reasoner, boolean consistent, File path)
            throws Exception {
        Set<OWLClass> all = new HashSet<>(ontology.getClassesInSignature(Imports.INCLUDED));
        TreeSet<String> lines = new TreeSet<>();
        if (!consistent) {
            TreeSet<String> every = new TreeSet<>();
            every.add(THING);
            every.add(NOTHING);
            for (OWLClass c : all) {
                every.add(c.getIRI().toString());
            }
            lines.add(equivalent(every));
        } else {
            OWLClass top = ontology.getOWLOntologyManager().getOWLDataFactory().getOWLThing();
            OWLClass bottom = ontology.getOWLOntologyManager().getOWLDataFactory().getOWLNothing();
            all.add(top);
            all.add(bottom);
            // A reasoner may answer a class's equivalents or its superclasses with an empty
            // node, as Whelk does for owl:Nothing when no class is unsatisfiable, so every
            // node holds its own class, the unsatisfiable classes join owl:Nothing's node and
            // a class without a named superclass gets owl:Thing.
            Set<OWLClass> unsatisfiable = new HashSet<>(reasoner.getUnsatisfiableClasses().getEntities());
            unsatisfiable.add(bottom);
            Set<OWLClass> done = new HashSet<>();
            for (OWLClass c : all) {
                if (done.contains(c)) {
                    continue;
                }
                boolean isBottom = unsatisfiable.contains(c);
                Set<OWLClass> node = new HashSet<>(reasoner.getEquivalentClasses(c).getEntities());
                node.add(c);
                if (isBottom) {
                    node.addAll(unsatisfiable);
                }
                TreeSet<String> members = new TreeSet<>();
                for (OWLClass m : node) {
                    members.add(m.getIRI().toString());
                    done.add(m);
                }
                if (members.size() > 1) {
                    lines.add(equivalent(members));
                } else {
                    lines.add("Declaration(Class(<" + members.first() + ">))");
                }
                if (isBottom || node.contains(top)) {
                    continue;
                }
                String first = members.first();
                TreeSet<String> parents = new TreeSet<>();
                for (Node<OWLClass> parent : reasoner.getSuperClasses(c, true)) {
                    parent.getEntities().stream().map(x -> x.getIRI().toString())
                        .min(Comparator.naturalOrder()).ifPresent(parents::add);
                }
                if (parents.isEmpty()) {
                    parents.add(THING);
                }
                for (String p : parents) {
                    lines.add("SubClassOf(<" + first + "> <" + p + ">)");
                }
            }
        }
        // A buffered writer, unlike PrintWriter, reports a failed write.
        try (java.io.BufferedWriter w = Files.newBufferedWriter(path.toPath(), StandardCharsets.UTF_8)) {
            for (String l : lines) {
                w.write(l);
                w.newLine();
            }
        }
    }

    // Checks writeTaxonomy against a reasoner that answers every query with an empty node, as
    // Whelk does for some, on an ontology of three classes with B under A. The nix packages
    // run it when they compile this file.
    static void selfTest() throws Exception {
        OWLOntologyManager m = OWLManager.createOWLOntologyManager();
        org.semanticweb.owlapi.model.OWLDataFactory f = m.getOWLDataFactory();
        OWLOntology o = m.createOntology();
        OWLClass a = f.getOWLClass(org.semanticweb.owlapi.model.IRI.create("urn:t:A"));
        OWLClass b = f.getOWLClass(org.semanticweb.owlapi.model.IRI.create("urn:t:B"));
        OWLClass u = f.getOWLClass(org.semanticweb.owlapi.model.IRI.create("urn:t:U"));
        for (OWLClass c : new OWLClass[] {a, b, u}) {
            m.addAxiom(o, f.getOWLDeclarationAxiom(c));
        }
        OWLReasoner r = (OWLReasoner) java.lang.reflect.Proxy.newProxyInstance(
            OWLReasoner.class.getClassLoader(), new Class<?>[] {OWLReasoner.class}, (proxy, method, margs) -> {
                switch (method.getName()) {
                    case "getEquivalentClasses":
                        return new org.semanticweb.owlapi.reasoner.impl.OWLClassNode();
                    case "getUnsatisfiableClasses":
                        return new org.semanticweb.owlapi.reasoner.impl.OWLClassNode(u);
                    case "getSuperClasses":
                        return margs[0].equals(b)
                            ? new org.semanticweb.owlapi.reasoner.impl.OWLClassNodeSet(
                                new org.semanticweb.owlapi.reasoner.impl.OWLClassNode(a))
                            : new org.semanticweb.owlapi.reasoner.impl.OWLClassNodeSet(
                                new org.semanticweb.owlapi.reasoner.impl.OWLClassNode());
                    default:
                        throw new UnsupportedOperationException(method.getName());
                }
            });
        File out = File.createTempFile("taxonomy", ".ofn");
        out.deleteOnExit();
        writeTaxonomy(o, r, true, out);
        List<String> got = Files.readAllLines(out.toPath());
        List<String> want = java.util.Arrays.asList(
            "Declaration(Class(<" + THING + ">))",
            "Declaration(Class(<urn:t:A>))",
            "Declaration(Class(<urn:t:B>))",
            "EquivalentClasses(<" + NOTHING + "> <urn:t:U>)",
            "SubClassOf(<urn:t:A> <" + THING + ">)",
            "SubClassOf(<urn:t:B> <urn:t:A>)");
        if (!got.equals(want)) {
            System.err.println("self-test: got " + got + ", want " + want);
            System.exit(1);
        }
        System.out.println("self-test passed");
    }

    static String equivalent(TreeSet<String> members) {
        return "EquivalentClasses(" + members.stream().map(m -> "<" + m + ">")
            .collect(Collectors.joining(" ")) + ")";
    }

    // The peak resident set from Linux's procfs, or -1 where it is not available.
    static long peakRssKib() {
        try {
            for (String l : Files.readAllLines(new File("/proc/self/status").toPath())) {
                if (l.startsWith("VmHWM:")) {
                    return Long.parseLong(l.replaceAll("[^0-9]", ""));
                }
            }
        } catch (java.io.IOException | RuntimeException e) {
            // Fall through to the unavailable value.
        }
        return -1;
    }

    static double median(List<Double> xs) {
        List<Double> s = new ArrayList<>(xs);
        Collections.sort(s);
        int n = s.size();
        return n % 2 == 1 ? s.get(n / 2) : (s.get(n / 2 - 1) + s.get(n / 2)) / 2;
    }

    static String fmt(double x) {
        return String.format(java.util.Locale.ROOT, "%.1f", x);
    }

    static String list(List<Double> xs) {
        return "[" + xs.stream().map(ElkClassify::fmt).collect(Collectors.joining(",")) + "]";
    }

    static void field(StringBuilder out, String name, String value) {
        out.append('"').append(name).append("\":").append(value).append(',');
    }
}
