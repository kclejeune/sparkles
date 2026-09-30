// Fake dataset for the Sparkles mock server: a small FOAF-flavoured org graph
// with an OWL ontology on top. Deterministic (seeded) so screenshots are stable.

export const PREFIXES = {
  rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
  rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
  owl: 'http://www.w3.org/2002/07/owl#',
  xsd: 'http://www.w3.org/2001/XMLSchema#',
  foaf: 'http://xmlns.com/foaf/0.1/',
  dcterms: 'http://purl.org/dc/terms/',
  skos: 'http://www.w3.org/2004/02/skos/core#',
  schema: 'http://schema.org/',
  ex: 'http://example.org/ontology#',
  res: 'http://example.org/resource/',
};

const header = Object.entries(PREFIXES)
  .map(([p, iri]) => `@prefix ${p}: <${iri}> .`)
  .join('\n');

const ontology = `
ex: a owl:Ontology ; rdfs:label "Sparkles example organisation ontology"@en ;
  dcterms:creator "Sparkles mock" ; owl:versionInfo "0.3" .

ex:Agent a owl:Class ; rdfs:label "Agent"@en ; rdfs:comment "Something that can act."@en .
ex:Person a owl:Class ; rdfs:subClassOf ex:Agent, foaf:Person ; rdfs:label "Person"@en, "Personne"@fr .
ex:Employee a owl:Class ; rdfs:subClassOf ex:Person ; rdfs:label "Employee"@en .
ex:Manager a owl:Class ; rdfs:subClassOf ex:Employee ; rdfs:label "Manager"@en .
ex:Engineer a owl:Class ; rdfs:subClassOf ex:Employee ; rdfs:label "Engineer"@en .
ex:Researcher a owl:Class ; rdfs:subClassOf ex:Person ; rdfs:label "Researcher"@en .
ex:Organization a owl:Class ; rdfs:subClassOf ex:Agent, foaf:Organization ; rdfs:label "Organization"@en .
ex:Company a owl:Class ; rdfs:subClassOf ex:Organization ; rdfs:label "Company"@en .
ex:University a owl:Class ; rdfs:subClassOf ex:Organization ; rdfs:label "University"@en .
ex:Team a owl:Class ; rdfs:subClassOf ex:Organization ; rdfs:label "Team"@en .
ex:Project a owl:Class ; rdfs:label "Project"@en .
ex:Publication a owl:Class ; rdfs:label "Publication"@en .
ex:Place a owl:Class ; rdfs:label "Place"@en .
ex:City a owl:Class ; rdfs:subClassOf ex:Place ; rdfs:label "City"@en .
ex:Skill a owl:Class ; rdfs:subClassOf skos:Concept ; rdfs:label "Skill"@en .

ex:worksFor a owl:ObjectProperty ; rdfs:label "works for"@en ; rdfs:domain ex:Employee ; rdfs:range ex:Company .
ex:memberOf a owl:ObjectProperty ; rdfs:label "member of"@en ; rdfs:domain ex:Person ; rdfs:range ex:Team .
ex:manages a owl:ObjectProperty ; rdfs:label "manages"@en ; rdfs:domain ex:Manager ; rdfs:range ex:Team .
ex:partOf a owl:ObjectProperty, owl:TransitiveProperty ; rdfs:label "part of"@en ; rdfs:domain ex:Team ; rdfs:range ex:Organization .
ex:contributesTo a owl:ObjectProperty ; rdfs:label "contributes to"@en ; rdfs:domain ex:Person ; rdfs:range ex:Project .
ex:authored a owl:ObjectProperty ; rdfs:label "authored"@en ; rdfs:domain ex:Person ; rdfs:range ex:Publication ; owl:inverseOf ex:author .
ex:author a owl:ObjectProperty ; rdfs:label "author"@en ; rdfs:domain ex:Publication ; rdfs:range ex:Person .
ex:basedIn a owl:ObjectProperty ; rdfs:label "based in"@en ; rdfs:domain ex:Agent ; rdfs:range ex:City .
ex:hasSkill a owl:ObjectProperty ; rdfs:label "has skill"@en ; rdfs:domain ex:Person ; rdfs:range ex:Skill .
ex:alumnusOf a owl:ObjectProperty ; rdfs:label "alumnus of"@en ; rdfs:domain ex:Person ; rdfs:range ex:University .
ex:salary a owl:DatatypeProperty ; rdfs:label "salary"@en ; rdfs:domain ex:Employee ; rdfs:range xsd:decimal .
ex:startDate a owl:DatatypeProperty ; rdfs:label "start date"@en ; rdfs:domain ex:Employee ; rdfs:range xsd:date .
ex:founded a owl:DatatypeProperty ; rdfs:label "founded"@en ; rdfs:domain ex:Organization ; rdfs:range xsd:gYear .
ex:citations a owl:DatatypeProperty ; rdfs:label "citations"@en ; rdfs:domain ex:Publication ; rdfs:range xsd:integer .
foaf:knows a owl:ObjectProperty, owl:SymmetricProperty ; rdfs:label "knows"@en ; rdfs:domain foaf:Person ; rdfs:range foaf:Person .
foaf:name a owl:DatatypeProperty ; rdfs:label "name"@en ; rdfs:range xsd:string .
foaf:age a owl:DatatypeProperty ; rdfs:label "age"@en ; rdfs:range xsd:integer .
foaf:mbox a owl:ObjectProperty ; rdfs:label "mailbox"@en .
`;

// --- deterministic PRNG ----------------------------------------------------
let seed = 7;
const rnd = () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
const pick = (arr) => arr[Math.floor(rnd() * arr.length)];
const pickN = (arr, n) => {
  const copy = [...arr];
  const out = [];
  while (out.length < n && copy.length)
    out.push(copy.splice(Math.floor(rnd() * copy.length), 1)[0]);
  return out;
};

const first = [
  'Ada',
  'Grace',
  'Alan',
  'Barbara',
  'Edsger',
  'Margaret',
  'Donald',
  'Frances',
  'Tim',
  'Radia',
  'Ken',
  'Leslie',
  'Niklaus',
  'Karen',
  'John',
  'Shafi',
  'Dennis',
  'Hedy',
  'Linus',
  'Sophie',
  'Guido',
  'Anita',
  'Bjarne',
  'Lynn',
  'Yukihiro',
  'Adele',
  'Robin',
  'Mary',
  'Jim',
  'Carla',
  'Ole',
  'Joan',
  'Tony',
  'Ivan',
  'Jean',
  'Katherine',
  'Whitfield',
  'Evelyn',
  'Rasmus',
  'Fran',
];
const last = [
  'Lovelace',
  'Hopper',
  'Turing',
  'Liskov',
  'Dijkstra',
  'Hamilton',
  'Knuth',
  'Allen',
  'Berners-Lee',
  'Perlman',
  'Thompson',
  'Lamport',
  'Wirth',
  'Spärck Jones',
  'McCarthy',
  'Goldwasser',
  'Ritchie',
  'Lamarr',
  'Torvalds',
  'Wilson',
  'van Rossum',
  'Borg',
  'Stroustrup',
  'Conway',
  'Matsumoto',
  'Goldberg',
  'Milner',
  'Shaw',
  'Gray',
  'Ellis',
  'Dahl',
  'Clarke',
  'Hoare',
  'Sutherland',
  'Sammet',
  'Johnson',
  'Diffie',
  'Boyd',
  'Lerdorf',
  'Bilas',
];

const cities = [
  ['Berlin', 'DE', 52.52, 13.405],
  ['Lisbon', 'PT', 38.72, -9.14],
  ['Montréal', 'CA', 45.5, -73.57],
  ['Kyoto', 'JP', 35.01, 135.77],
  ['Nairobi', 'KE', -1.29, 36.82],
  ['Portland', 'US', 45.52, -122.68],
];
const companies = [
  ['Tessellate', 1998],
  ['Quadrant Labs', 2011],
  ['Northwind Graph', 2004],
  ['Lattice & Co', 2016],
];
const universities = [
  'Institute of Formal Methods',
  'Polytechnic of the North',
  'Open Semantics University',
];
const teams = ['Storage', 'Query Engine', 'Reasoner', 'Web UI', 'Ingest', 'Research'];
const projects = [
  'Sparkles',
  'Permutation Index',
  'Delta Merge',
  'OWL-RL Rules',
  'Cardinality Estimator',
  'Result Cache',
];
const skills = [
  'Rust',
  'SPARQL',
  'OWL',
  'Query optimization',
  'Compression',
  'Svelte',
  'Distributed systems',
  'Datalog',
];
const pubWords = [
  'Efficient',
  'Scalable',
  'Incremental',
  'Compressed',
  'Adaptive',
  'Sorted',
  'Join',
  'Index',
  'Permutations',
  'Reasoning',
  'for',
  'over',
  'Knowledge Graphs',
  'RDF',
  'Triple Stores',
  'Datalog',
];

const slug = (s) =>
  s
    .normalize('NFD')
    .replace(/[̀-ͯ]/g, '')
    .replace(/[^A-Za-z0-9]+/g, '_');
const lit = (s) => JSON.stringify(s);

export function buildTurtle() {
  const out = [header, ontology];
  for (const [name, cc, lat, lon] of cities) {
    out.push(`res:${slug(name)} a ex:City ; rdfs:label ${lit(name)}@en ; schema:addressCountry "${cc}" ;
      schema:latitude "${lat}"^^xsd:decimal ; schema:longitude "${lon}"^^xsd:decimal .`);
  }
  for (const [name, year] of companies) {
    out.push(`res:${slug(name)} a ex:Company ; rdfs:label ${lit(name)} ; foaf:name ${lit(name)} ;
      ex:founded "${year}"^^xsd:gYear ; ex:basedIn res:${slug(pick(cities)[0])} ;
      foaf:homepage <https://${slug(name).toLowerCase()}.example> .`);
  }
  for (const u of universities) {
    out.push(
      `res:${slug(u)} a ex:University ; rdfs:label ${lit(u)}@en ; ex:basedIn res:${slug(pick(cities)[0])} .`,
    );
  }
  teams.forEach((t, i) => {
    const company = companies[i % companies.length][0];
    out.push(
      `res:team_${slug(t)} a ex:Team ; rdfs:label ${lit(t + ' team')}@en ; ex:partOf res:${slug(company)} .`,
    );
  });
  projects.forEach((p) => {
    out.push(`res:project_${slug(p)} a ex:Project ; rdfs:label ${lit(p)} ;
      dcterms:description ${lit('Work on ' + p.toLowerCase() + ' for the Sparkles engine.')}@en .`);
  });
  skills.forEach((s) =>
    out.push(`res:skill_${slug(s)} a ex:Skill ; skos:prefLabel ${lit(s)}@en .`),
  );

  const people = first.map((f, i) => ({
    id: `res:${slug(f + '_' + last[i])}`,
    name: `${f} ${last[i]}`,
    i,
  }));
  for (const p of people) {
    const role =
      p.i % 9 === 0
        ? 'ex:Manager'
        : p.i % 4 === 0
          ? 'ex:Researcher'
          : p.i % 3 === 0
            ? 'ex:Employee'
            : 'ex:Engineer';
    const company = pick(companies)[0];
    const lines = [
      `${p.id} a ${role}`,
      `foaf:name ${lit(p.name)}`,
      `rdfs:label ${lit(p.name)}`,
      `foaf:givenName ${lit(first[p.i])}`,
      `foaf:familyName ${lit(last[p.i])}`,
      `foaf:age ${20 + Math.floor(rnd() * 45)}`,
      `foaf:mbox <mailto:${slug(first[p.i]).toLowerCase()}@${slug(company).toLowerCase()}.example>`,
      `ex:basedIn res:${slug(pick(cities)[0])}`,
      `ex:memberOf res:team_${slug(pick(teams))}`,
      `ex:hasSkill ${pickN(skills, 1 + Math.floor(rnd() * 3))
        .map((s) => `res:skill_${slug(s)}`)
        .join(', ')}`,
      `ex:contributesTo ${pickN(projects, 1 + Math.floor(rnd() * 2))
        .map((s) => `res:project_${slug(s)}`)
        .join(', ')}`,
      `foaf:knows ${pickN(
        people.filter((q) => q !== p),
        2 + Math.floor(rnd() * 4),
      )
        .map((q) => q.id)
        .join(', ')}`,
    ];
    if (role !== 'ex:Researcher') {
      lines.push(`ex:worksFor res:${slug(company)}`);
      lines.push(`ex:salary "${(60000 + Math.floor(rnd() * 90) * 1000).toFixed(2)}"^^xsd:decimal`);
      lines.push(
        `ex:startDate "${2008 + Math.floor(rnd() * 17)}-0${1 + Math.floor(rnd() * 9)}-1${Math.floor(rnd() * 9)}"^^xsd:date`,
      );
    } else {
      lines.push(`ex:alumnusOf res:${slug(pick(universities))}`);
    }
    if (role === 'ex:Manager') lines.push(`ex:manages res:team_${slug(pick(teams))}`);
    if (p.i % 5 === 0)
      lines.push(
        `rdfs:comment ${lit(`${first[p.i]} likes sorted permutations.`)}@en, ${lit(`${first[p.i]} aime les permutations triées.`)}@fr`,
      );
    out.push(lines.join(' ;\n  ') + ' .');
  }

  for (let k = 0; k < 18; k++) {
    const title = pickN(pubWords, 4).join(' ');
    const authors = pickN(people, 1 + Math.floor(rnd() * 3));
    out.push(`res:pub_${k} a ex:Publication ; dcterms:title ${lit(title)}@en ;
      dcterms:issued "${2012 + Math.floor(rnd() * 13)}"^^xsd:gYear ; ex:citations ${Math.floor(rnd() * 400)} ;
      ex:author ${authors.map((a) => a.id).join(', ')} ;
      ex:about [ a skos:Concept ; skos:prefLabel ${lit(pick(skills))}@en ] .`);
  }
  return out.join('\n\n');
}

export const provenanceTrig = `
${header}
<http://example.org/graph/provenance> {
  <http://example.org/graph/provenance> dcterms:created "2026-09-01T10:00:00Z"^^xsd:dateTime ;
    dcterms:source <https://github.com/kclejeune/sparkles> ; rdfs:label "Load provenance"@en .
  res:Tessellate dcterms:source <https://tessellate.example/about> .
  res:Quadrant_Labs dcterms:source <https://quadrant.example/about> .
}
`;

export const scratchTurtle = `
${header}
res:hello a ex:Project ; rdfs:label "Scratch project"@en ; dcterms:description "A tiny in-memory dataset."@en ;
  ex:contributesTo res:hello .
res:alice a foaf:Person ; foaf:name "Alice" ; foaf:knows res:bob .
res:bob a foaf:Person ; foaf:name "Bob" ; foaf:knows res:alice .
`;
