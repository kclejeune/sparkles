// The upload form's CSV and TSV options. `POST /{ds}/upload` maps a file named `.csv`,
// `.tsv` or `.tab` (before a compression extension) to triples: with the default mapping,
// which needs a `base` IRI and may take a `key` column, or with a CSVW metadata part named
// `mapping` or a CONSTRUCT template part named `template` (API.md, "CSV and TSV uploads").

/** File extensions the upload's file picker offers. */
export const UPLOAD_ACCEPT = [
  '.ttl',
  '.nt',
  '.nq',
  '.trig',
  '.rdf',
  '.owl',
  '.xml',
  '.jsonld',
  '.n3',
  '.trix',
  '.rt',
  '.trdf',
  '.rpb',
  '.pbrdf',
  '.rj',
  '.csv',
  '.tsv',
  '.tab',
  '.gz',
  '.zst',
  '.br',
  '.lz4',
].join(',');

const COMPRESSED = /\.(gz|zst|br|lz4)$/i;

/** Whether a file name is a table the server maps to triples, and which kind. */
export function tableKind(name: string): 'csv' | 'tsv' | null {
  const plain = name.replace(COMPRESSED, '');
  if (/\.csv$/i.test(plain)) return 'csv';
  if (/\.(tsv|tab)$/i.test(plain)) return 'tsv';
  return null;
}

/** The part a mapping file goes in: a SPARQL file is a template, anything else CSVW. */
export function mappingPart(name: string): 'template' | 'mapping' {
  return /\.(rq|sparql)$/i.test(name) ? 'template' : 'mapping';
}

export type TableOptions = {
  /** The default mapping's namespace, and the URL of a mapped table without one. */
  base?: string;
  /** The column that names each row (default mapping only). */
  key?: string;
  /** A CSVW metadata document or a CONSTRUCT template. */
  mapping?: { name: string } | null;
};

/** The query parameters of the table options (`base` and `key`), without a leading `&`. */
export function tableParams(o: TableOptions): string {
  const q = new URLSearchParams();
  const base = o.base?.trim();
  const key = o.key?.trim();
  if (base) q.set('base', base);
  if (key) q.set('key', key);
  return q.toString();
}

/** Why the table options cannot be sent with these files, or null when they can. */
export function tableProblem(files: { name: string }[], o: TableOptions): string | null {
  if (!files.some((f) => tableKind(f.name))) return null;
  const base = o.base?.trim();
  if (base && !/^[a-z][a-z0-9+.-]*:\S*$/i.test(base))
    return 'The base must be an absolute IRI, such as http://example.org/people/.';
  if (o.key?.trim() && o.mapping)
    return 'A key column applies to the default mapping only, not with a mapping or template.';
  if (!o.mapping && !base) return 'The default mapping of CSV and TSV files needs a base IRI.';
  return null;
}
