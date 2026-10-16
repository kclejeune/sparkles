// The lookup of a mapping draft (spec C05 §3.7): the entries that name the IRI of a
// cell value, and the choice of a candidate for a value the draft left ambiguous.

export const LOOKUP_KEY = 'urn:x-sparkles:lookup';

type Entry = { 'rdf:value': string | string[]; '@id': string };
type Column = Record<string, unknown> & { name?: string };

/** The values an entry names. */
function valuesOf(e: Entry): string[] {
  const v = e['rdf:value'];
  return Array.isArray(v) ? v : [v];
}

/**
 * The mapping text with `iri` as the entity of `value` in the lookup of `column`. An
 * earlier entry for the value is replaced, and an entry that names no other value is
 * dropped. Text that is not a mapping with that column comes back unchanged.
 */
export function chooseCandidate(
  mappingText: string,
  column: string,
  value: string,
  iri: string,
): string {
  let m: { tableSchema?: { columns?: Column[] } };
  try {
    m = JSON.parse(mappingText);
  } catch {
    return mappingText;
  }
  const col = m.tableSchema?.columns?.find((c) => c.name === column);
  if (!col) return mappingText;
  const old = (Array.isArray(col[LOOKUP_KEY]) ? col[LOOKUP_KEY] : []) as Entry[];
  const kept: Entry[] = [];
  for (const e of old) {
    const rest = valuesOf(e).filter((v) => v !== value);
    if (rest.length === 0) continue;
    kept.push({ ...e, 'rdf:value': rest.length === 1 ? rest[0] : rest });
  }
  kept.push({ 'rdf:value': value, '@id': iri });
  col[LOOKUP_KEY] = kept;
  return JSON.stringify(m, null, 2);
}

/** Whether the lookup of `column` names `value`, and the IRI it gives. */
export function chosen(mappingText: string, column: string, value: string): string | undefined {
  try {
    const m = JSON.parse(mappingText);
    const col = (m.tableSchema?.columns as Column[] | undefined)?.find((c) => c.name === column);
    const entries = (Array.isArray(col?.[LOOKUP_KEY]) ? col[LOOKUP_KEY] : []) as Entry[];
    return entries.find((e) => valuesOf(e).includes(value))?.['@id'];
  } catch {
    return undefined;
  }
}
