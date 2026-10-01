PREFIX ex2: <http://example.org/2/>
PREFIX ex: <http://example.org/>
LOAD SILENT <http://example.org/data> INTO GRAPH ex:g ;
LOAD <http://example.org/data> ;
CLEAR SILENT GRAPH ex:g ;
CLEAR DEFAULT ;
DROP NAMED ;
DROP ALL ;
CREATE SILENT GRAPH ex:g ;
ADD SILENT DEFAULT TO GRAPH ex:g ;
MOVE ex:a TO DEFAULT ;
COPY GRAPH ex:a TO ex:b ;
INSERT DATA { ex:s ex:p _:b . GRAPH ex:g { ex:s ex:p [ ex:q 1 ] } . ex:t ex:p ( 1 2 ) } ;
DELETE DATA { ex:s ex:p "o" GRAPH ex:g {} } ;
DELETE WHERE { ?s ex2:p ?o . GRAPH ?g { ?s ?p ?o } } ;
WITH ex:g DELETE { ?s ex:p ?o } INSERT { ?s ex:q _:new } USING ex:u USING NAMED ex:v WHERE { ?s ex:p ?o } ;
INSERT { ?s ex:q ?o } WHERE {} ;
DELETE { ?s ex:q ?o } WHERE { ?s ex:q ?o } ;
