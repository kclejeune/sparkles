PREFIX ex: <http://example.org/>
PREFIX old: <http://example.org/old/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>

INSERT DATA {
  ex:a rdfs:label "a" ;
    ex:n 1 .
};

CLEAR ALL;

DELETE WHERE {
  ?s skos:altLabel ?o .
};

LOAD ex:data INTO GRAPH old:g
