//! Prints a stable projection of the parse of the built-in rule sets and of fixtures in
//! every Jena form, to compare two builds of the parser (item 839, Phase 5).

#[test]
#[ignore]
fn print_parse_projection() {
    use sparkles_reasoner::{OWL_RL_RULES, RDFS_RULES, RDFS_SIMPLE_RULES, parse_rules};
    let fixtures = [
        "@prefix ex: <http://ex.org/> .\n[r: (?a ex:p ?b) notEqual(?a, ?b) -> (?a ex:r ?b)]",
        "@prefix ex: <http://ex.org/> .\n(?a ex:p ?b) -> (?a ex:s ?b) .\n-> (ex:A rdf:type rdfs:Class) .",
        "@prefix ex: <http://ex.org/> .\n[r: (?x ex:p ?y) -> [(?y ex:q ?x) <- (?x ex:r ?y)]]",
        "@prefix ex: <http://ex.org/> .\n[b: (?a ex:q ?b) <- (?b ex:p ?a)]",
        "@prefix ex: <http://ex.org/> .\n@hybrid jena .\n[h: (?p rdf:type ex:Sym) -> [(?x ?p ?y) <- (?y ?p ?x)]]",
        "@include <rdfs> .\n@include <owl-rl> .",
        "@prefix ex: <http://ex.org/> .\n[f: (?x ex:p f(?y, 'a'@en, 1.5, _:b)) (?x ?_ *) -> (?x ex:q 2)]",
        "@prefix ex: <http://ex.org/> .\n[n: (?x ex:p ?y), not(?x ?y ?x), noValue(?x ex:q) -> (?x ex:r ?y)]",
    ];
    let texts = [RDFS_RULES, RDFS_SIMPLE_RULES, OWL_RL_RULES]
        .into_iter()
        .chain(fixtures);
    for (i, text) in texts.enumerate() {
        match parse_rules(text) {
            Ok(rules) => {
                for r in rules {
                    println!("{i}\t{}\t{:?}\t{r}", r.line, r.direction);
                }
            }
            Err(e) => println!("{i}\terror {e}"),
        }
    }
}
