//! A LUBM-style data generator: the Univ-Bench ontology and universities generated with
//! the distributions of the LUBM data generator (Guo, Pan and Heflin, "LUBM: A benchmark
//! for OWL knowledge base systems", 2005). It is not the official UBA generator, and its
//! triple counts are close to, not equal to, LUBM's (about 125,000 per university).
//! Each university comes from its own seed, so the data of `n` universities is the same
//! however it is produced.

use std::fmt::Write as _;

pub const UB: &str = "http://swat.cse.lehigh.edu/onto/univ-bench.owl#";

/// The Univ-Bench ontology in Turtle.
pub fn ontology() -> String {
    let mut s = String::from(
        "@prefix ub: <http://swat.cse.lehigh.edu/onto/univ-bench.owl#> .
@prefix owl: <http://www.w3.org/2002/07/owl#> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .
<http://swat.cse.lehigh.edu/onto/univ-bench.owl> a owl:Ontology .
",
    );
    let sub = [
        ("AdministrativeStaff", "Employee"),
        ("Article", "Publication"),
        ("AssistantProfessor", "Professor"),
        ("AssociateProfessor", "Professor"),
        ("Book", "Publication"),
        ("Chair", "Professor"),
        ("ClericalStaff", "AdministrativeStaff"),
        ("College", "Organization"),
        ("ConferencePaper", "Article"),
        ("Course", "Work"),
        ("Dean", "Professor"),
        ("Department", "Organization"),
        ("Faculty", "Employee"),
        ("FullProfessor", "Professor"),
        ("GraduateCourse", "Course"),
        ("GraduateStudent", "Person"),
        ("Institute", "Organization"),
        ("JournalArticle", "Article"),
        ("Lecturer", "Faculty"),
        ("Manual", "Publication"),
        ("PostDoc", "Faculty"),
        ("Professor", "Faculty"),
        ("Program", "Organization"),
        ("Research", "Work"),
        ("ResearchAssistant", "Person"),
        ("ResearchGroup", "Organization"),
        ("Software", "Publication"),
        ("Specification", "Publication"),
        ("SystemsStaff", "AdministrativeStaff"),
        ("TechnicalReport", "Article"),
        ("UndergraduateStudent", "Student"),
        ("University", "Organization"),
        ("UnofficialPublication", "Publication"),
        ("VisitingProfessor", "Professor"),
    ];
    for (c, d) in sub {
        let _ = writeln!(s, "ub:{c} a owl:Class ; rdfs:subClassOf ub:{d} .");
    }
    for c in [
        "Organization",
        "Person",
        "Publication",
        "Schedule",
        "Work",
        "Employee",
        "Student",
    ] {
        let _ = writeln!(s, "ub:{c} a owl:Class .");
    }
    // defined classes: C ≡ Person ⊓ ∃p.D (Dean has no Person conjunct in Univ-Bench)
    let defined = [
        ("Chair", Some("Person"), "headOf", "Department"),
        ("Dean", None, "headOf", "College"),
        ("Director", Some("Person"), "headOf", "Program"),
        ("Employee", Some("Person"), "worksFor", "Organization"),
        ("Student", Some("Person"), "takesCourse", "Course"),
        (
            "TeachingAssistant",
            Some("Person"),
            "teachingAssistantOf",
            "Course",
        ),
    ];
    for (c, conj, p, d) in defined {
        let r =
            format!("[ a owl:Restriction ; owl:onProperty ub:{p} ; owl:someValuesFrom ub:{d} ]");
        match conj {
            Some(k) => {
                let _ = writeln!(
                    s,
                    "ub:{c} a owl:Class ; owl:equivalentClass [ a owl:Class ; owl:intersectionOf ( ub:{k} {r} ) ] ."
                );
            }
            None => {
                let _ = writeln!(s, "ub:{c} a owl:Class ; owl:equivalentClass {r} .");
            }
        }
    }
    // restrictions that are only superclasses
    let _ = writeln!(
        s,
        "ub:GraduateStudent rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ub:takesCourse ; owl:someValuesFrom ub:GraduateCourse ] ."
    );
    let _ = writeln!(
        s,
        "ub:ResearchAssistant rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ub:worksFor ; owl:someValuesFrom ub:ResearchGroup ] ."
    );
    let object = [
        ("advisor", Some("Person"), Some("Professor")),
        (
            "affiliatedOrganizationOf",
            Some("Organization"),
            Some("Organization"),
        ),
        ("affiliateOf", Some("Organization"), Some("Person")),
        ("degreeFrom", Some("Person"), Some("University")),
        ("doctoralDegreeFrom", Some("Person"), Some("University")),
        ("hasAlumnus", Some("University"), Some("Person")),
        ("headOf", None, None),
        ("listedCourse", Some("Schedule"), Some("Course")),
        ("mastersDegreeFrom", Some("Person"), Some("University")),
        ("member", Some("Organization"), Some("Person")),
        ("memberOf", None, None),
        ("orgPublication", Some("Organization"), Some("Publication")),
        ("publicationAuthor", Some("Publication"), Some("Person")),
        ("publicationResearch", Some("Publication"), Some("Research")),
        ("researchProject", Some("ResearchGroup"), Some("Research")),
        (
            "softwareDocumentation",
            Some("Software"),
            Some("Publication"),
        ),
        (
            "subOrganizationOf",
            Some("Organization"),
            Some("Organization"),
        ),
        ("takesCourse", None, None),
        ("teacherOf", Some("Faculty"), Some("Course")),
        (
            "teachingAssistantOf",
            Some("TeachingAssistant"),
            Some("Course"),
        ),
        (
            "undergraduateDegreeFrom",
            Some("Person"),
            Some("University"),
        ),
        ("worksFor", None, None),
    ];
    for (p, d, r) in object {
        let _ = writeln!(s, "ub:{p} a owl:ObjectProperty .");
        if let Some(d) = d {
            let _ = writeln!(s, "ub:{p} rdfs:domain ub:{d} .");
        }
        if let Some(r) = r {
            let _ = writeln!(s, "ub:{p} rdfs:range ub:{r} .");
        }
    }
    for p in [
        "age",
        "emailAddress",
        "name",
        "officeNumber",
        "researchInterest",
        "softwareVersion",
        "telephone",
        "title",
        "publicationDate",
        "tenured",
    ] {
        let _ = writeln!(s, "ub:{p} a owl:DatatypeProperty .");
    }
    for (p, d) in [
        ("age", "Person"),
        ("emailAddress", "Person"),
        ("tenured", "Professor"),
    ] {
        let _ = writeln!(s, "ub:{p} rdfs:domain ub:{d} .");
    }
    for (p, q) in [
        ("doctoralDegreeFrom", "degreeFrom"),
        ("mastersDegreeFrom", "degreeFrom"),
        ("undergraduateDegreeFrom", "degreeFrom"),
        ("headOf", "worksFor"),
        ("worksFor", "memberOf"),
    ] {
        let _ = writeln!(s, "ub:{p} rdfs:subPropertyOf ub:{q} .");
    }
    s.push_str("ub:hasAlumnus owl:inverseOf ub:degreeFrom .\n");
    s.push_str("ub:member owl:inverseOf ub:memberOf .\n");
    s.push_str("ub:subOrganizationOf a owl:TransitiveProperty .\n");
    s
}

/// xorshift64*
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// uniform in `[lo, hi]`
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next() % (hi - lo + 1) as u64) as usize
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Universities referenced by degrees, as LUBM draws them from a fixed range.
const UNIV_RANGE: usize = 1000;

/// The N-Triples of university `u`.
pub fn university(u: usize) -> String {
    let mut r = Rng::new(u as u64 + 1);
    let mut out = String::with_capacity(16 << 20);
    let ty = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let univ = |i: usize| format!("http://www.University{i}.edu");
    let mut t = |s: &str, p: &str, o: &str| {
        let _ = writeln!(out, "<{s}> <{p}> {o} .");
    };
    let ub = |l: &str| format!("{UB}{l}");
    let iri = |s: &str| format!("<{s}>");
    let lit = |s: &str| format!("\"{s}\"");
    let u_iri = univ(u);
    t(&u_iri, ty, &iri(&ub("University")));
    t(&u_iri, &ub("name"), &lit(&format!("University{u}")));
    let depts = r.range(15, 25);
    for d in 0..depts {
        let dept = format!("http://www.Department{d}.University{u}.edu");
        let di = |l: &str| format!("{dept}/{l}");
        t(&dept, ty, &iri(&ub("Department")));
        t(&dept, &ub("name"), &lit(&format!("Department{d}")));
        t(&dept, &ub("subOrganizationOf"), &iri(&u_iri));
        let kinds = [
            ("FullProfessor", r.range(7, 10), (15, 20)),
            ("AssociateProfessor", r.range(10, 14), (10, 18)),
            ("AssistantProfessor", r.range(8, 11), (5, 10)),
            ("Lecturer", r.range(5, 7), (0, 5)),
        ];
        let faculty: usize = kinds.iter().map(|k| k.1).sum();
        let n_courses = faculty * 3 / 2;
        let n_grad_courses = faculty * 3 / 2;
        let mut course = 0;
        let mut grad_course = 0;
        let mut profs: Vec<String> = Vec::new();
        for (kind, n, pubs) in kinds {
            for i in 0..n {
                let f = di(&format!("{kind}{i}"));
                t(&f, ty, &iri(&ub(kind)));
                t(&f, &ub("name"), &lit(&format!("{kind}{i}")));
                t(
                    &f,
                    &ub("emailAddress"),
                    &lit(&format!("{kind}{i}@Department{d}.University{u}.edu")),
                );
                t(&f, &ub("telephone"), &lit("xxx-xxx-xxxx"));
                t(
                    &f,
                    &ub("researchInterest"),
                    &lit(&format!("Research{}", r.below(30))),
                );
                t(&f, &ub("worksFor"), &iri(&dept));
                for _ in 0..r.range(1, 2) {
                    if course < n_courses {
                        let c = di(&format!("Course{course}"));
                        t(&f, &ub("teacherOf"), &iri(&c));
                        t(&c, ty, &iri(&ub("Course")));
                        t(&c, &ub("name"), &lit(&format!("Course{course}")));
                        course += 1;
                    }
                }
                for _ in 0..r.range(1, 2) {
                    if grad_course < n_grad_courses {
                        let c = di(&format!("GraduateCourse{grad_course}"));
                        t(&f, &ub("teacherOf"), &iri(&c));
                        t(&c, ty, &iri(&ub("GraduateCourse")));
                        t(
                            &c,
                            &ub("name"),
                            &lit(&format!("GraduateCourse{grad_course}")),
                        );
                        grad_course += 1;
                    }
                }
                for p in [
                    "undergraduateDegreeFrom",
                    "mastersDegreeFrom",
                    "doctoralDegreeFrom",
                ] {
                    t(&f, &ub(p), &iri(&univ(r.below(UNIV_RANGE))));
                }
                for k in 0..r.range(pubs.0, pubs.1) {
                    let p = format!("{f}/Publication{k}");
                    t(&p, ty, &iri(&ub("Publication")));
                    t(&p, &ub("name"), &lit(&format!("Publication{k}")));
                    t(&p, &ub("publicationAuthor"), &iri(&f));
                }
                if kind == "FullProfessor" && i == 0 {
                    t(&f, &ub("headOf"), &iri(&dept));
                }
                if kind != "Lecturer" {
                    profs.push(f);
                }
            }
        }
        let course_count = course.max(1);
        let grad_course_count = grad_course.max(1);
        let under = faculty * r.range(8, 14);
        for i in 0..under {
            let s = di(&format!("UndergraduateStudent{i}"));
            t(&s, ty, &iri(&ub("UndergraduateStudent")));
            t(&s, &ub("name"), &lit(&format!("UndergraduateStudent{i}")));
            t(
                &s,
                &ub("emailAddress"),
                &lit(&format!(
                    "UndergraduateStudent{i}@Department{d}.University{u}.edu"
                )),
            );
            t(&s, &ub("telephone"), &lit("xxx-xxx-xxxx"));
            t(&s, &ub("memberOf"), &iri(&dept));
            for _ in 0..r.range(2, 4) {
                t(
                    &s,
                    &ub("takesCourse"),
                    &iri(&di(&format!("Course{}", r.below(course_count)))),
                );
            }
            if r.below(5) == 0 {
                t(&s, &ub("advisor"), &iri(&profs[r.below(profs.len())]));
            }
        }
        let grads = faculty * r.range(3, 4);
        let mut pub_by: Vec<(String, String)> = Vec::new();
        for i in 0..grads {
            let s = di(&format!("GraduateStudent{i}"));
            t(&s, ty, &iri(&ub("GraduateStudent")));
            t(&s, &ub("name"), &lit(&format!("GraduateStudent{i}")));
            t(
                &s,
                &ub("emailAddress"),
                &lit(&format!(
                    "GraduateStudent{i}@Department{d}.University{u}.edu"
                )),
            );
            t(&s, &ub("telephone"), &lit("xxx-xxx-xxxx"));
            t(&s, &ub("memberOf"), &iri(&dept));
            t(
                &s,
                &ub("undergraduateDegreeFrom"),
                &iri(&univ(r.below(UNIV_RANGE))),
            );
            for _ in 0..r.range(1, 3) {
                t(
                    &s,
                    &ub("takesCourse"),
                    &iri(&di(&format!(
                        "GraduateCourse{}",
                        r.below(grad_course_count)
                    ))),
                );
            }
            let adv = &profs[r.below(profs.len())];
            t(&s, &ub("advisor"), &iri(adv));
            if r.below(4) == 0 {
                t(
                    &s,
                    &ub("teachingAssistantOf"),
                    &iri(&di(&format!("Course{}", r.below(course_count)))),
                );
            }
            if r.below(4) == 0 {
                t(&s, ty, &iri(&ub("ResearchAssistant")));
            }
            if r.below(3) == 0 {
                pub_by.push((s.clone(), adv.clone()));
            }
        }
        // co-authored publications of graduate students with their advisors
        for (k, (s, adv)) in pub_by.iter().enumerate() {
            let p = format!("{adv}/Publication{}", 100 + k);
            t(&p, ty, &iri(&ub("Publication")));
            t(&p, &ub("name"), &lit(&format!("Publication{}", 100 + k)));
            t(&p, &ub("publicationAuthor"), &iri(adv));
            t(&p, &ub("publicationAuthor"), &iri(s));
        }
        for g in 0..r.range(10, 20) {
            let rg = di(&format!("ResearchGroup{g}"));
            t(&rg, ty, &iri(&ub("ResearchGroup")));
            t(&rg, &ub("subOrganizationOf"), &iri(&dept));
        }
    }
    out
}
