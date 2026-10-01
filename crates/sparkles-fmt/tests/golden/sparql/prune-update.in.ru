PREFIX ex: <http://example.org/>
PREFIX old: <http://example.org/old/>
PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
PREFIX skos: <http://www.w3.org/2004/02/skos/core#>
PREFIX gone: <http://example.org/gone/>
PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
INSERT DATA { ex:a rdfs:label "a" ; ex:n "1"^^xsd:integer } ;
CLEAR ALL ;
DELETE WHERE { ?s <http://www.w3.org/2004/02/skos/core#altLabel> ?o } ;
LOAD <http://example.org/data> INTO GRAPH old:g
