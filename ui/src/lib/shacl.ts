// The SHACL validation panel's shapes syntax: Turtle, or the SHACL Compact Syntax
// (SHACLC, `text/shaclc`).

/** The syntax of the shapes in the editor. */
export type ShaclSyntax = 'turtle' | 'shaclc';

/** localStorage key of a dataset's shapes syntax. */
export const shaclSyntaxKey = (ds: string) => `sparkles.shacl.syntax.${ds}`;

/** The stored syntax, Turtle unless it is SHACLC. */
export function readShaclSyntax(stored: unknown): ShaclSyntax {
  return stored === 'shaclc' ? 'shaclc' : 'turtle';
}

/** The media type a shapes body is sent with. */
export function shapesMediaType(syntax: ShaclSyntax): string {
  return syntax === 'shaclc' ? 'text/shaclc' : 'text/turtle';
}

/** The editor's label for a syntax. */
export function shapesLabel(syntax: ShaclSyntax): string {
  return syntax === 'shaclc' ? 'Shapes graph (SHACLC)' : 'Shapes graph (Turtle)';
}

/**
 * The prefixes a shapes document declares, for showing IRIs in the results: Turtle's
 * `@prefix` and SPARQL-style `PREFIX` (Turtle 1.1 and SHACLC). SHACLC binds `rdf`,
 * `rdfs`, `sh` and `xsd` without declaring them.
 */
export function shapesPrefixes(text: string, syntax: ShaclSyntax): Record<string, string> {
  const out: Record<string, string> = {};
  if (syntax === 'shaclc') {
    Object.assign(out, {
      rdf: 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
      rdfs: 'http://www.w3.org/2000/01/rdf-schema#',
      sh: 'http://www.w3.org/ns/shacl#',
      xsd: 'http://www.w3.org/2001/XMLSchema#',
    });
  }
  for (const m of text.matchAll(/(?:@prefix|\bPREFIX)\s+([A-Za-z][\w.-]*|):\s*<([^>\s]*)>/gi))
    out[m[1]] = m[2];
  return out;
}

/** The example shapes of a new SHACLC editor (the Turtle example, in SHACLC). */
export const DEFAULT_SHACLC = `PREFIX foaf: <http://xmlns.com/foaf/0.1/>
PREFIX ex:   <http://example.org/>

# Every ex:Person (subclasses included) has exactly one string name
# and exactly one integer age between 0 and 150.
shape ex:PersonShape -> ex:Person {
  foaf:name xsd:string [1..1] .
  foaf:age xsd:integer [1..1] minInclusive=0 maxInclusive=150 .
}
`;
