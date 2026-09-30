export type Example = { title: string; description: string; query: string };

export const EXAMPLES: Example[] = [
  {
    title: 'First 100 triples',
    description: 'Any subject, predicate and object',
    query: `SELECT ?s ?p ?o
WHERE {
  ?s ?p ?o
}
LIMIT 100`,
  },
  {
    title: 'Classes by instance count',
    description: 'What kinds of things are in the data',
    query: `PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>

SELECT ?class (COUNT(?s) AS ?instances)
WHERE {
  ?s rdf:type ?class
}
GROUP BY ?class
ORDER BY DESC(?instances)`,
  },
  {
    title: 'Predicates by usage',
    description: 'Which properties are used and how often',
    query: `SELECT ?p (COUNT(*) AS ?uses) (COUNT(DISTINCT ?s) AS ?subjects)
WHERE {
  ?s ?p ?o
}
GROUP BY ?p
ORDER BY DESC(?uses)`,
  },
  {
    title: 'People and who they know',
    description: 'foaf:knows as a graph',
    query: `PREFIX foaf: <http://xmlns.com/foaf/0.1/>

CONSTRUCT {
  ?a foaf:knows ?b .
  ?a foaf:name ?an .
}
WHERE {
  ?a foaf:knows ?b ;
     foaf:name ?an .
}
LIMIT 300`,
  },
  {
    title: 'Labels in a language',
    description: 'rdfs:label filtered by language tag',
    query: `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?s ?label
WHERE {
  ?s rdfs:label ?label .
  FILTER(LANGMATCHES(LANG(?label), "en"))
}
ORDER BY ?label
LIMIT 200`,
  },
  {
    title: 'Class hierarchy',
    description: 'rdfs:subClassOf pairs with labels',
    query: `PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>

SELECT ?class ?super ?label
WHERE {
  ?class rdfs:subClassOf ?super .
  OPTIONAL { ?class rdfs:label ?label FILTER(LANG(?label) = "" || LANGMATCHES(LANG(?label), "en")) }
}
ORDER BY ?super ?class`,
  },
  {
    title: 'Describe a resource',
    description: 'All triples about one IRI',
    query: `DESCRIBE <http://example.org/resource/Ada_Lovelace>`,
  },
  {
    title: 'Named graphs',
    description: 'Quads per graph',
    query: `SELECT ?g (COUNT(*) AS ?quads)
WHERE {
  GRAPH ?g { ?s ?p ?o }
}
GROUP BY ?g
ORDER BY DESC(?quads)`,
  },
  {
    title: 'Insert data',
    description: 'A SPARQL Update, sent to /update',
    query: `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX res: <http://example.org/resource/>

INSERT DATA {
  res:new_person a foaf:Person ;
    foaf:name "New Person" ;
    foaf:knows res:Ada_Lovelace .
}`,
  },
];
