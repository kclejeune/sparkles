// Client and starter presets for pattern-based OWL 2 axiom generation (ROADMAP item 623,
// docs/specs/U12-ontology-workbench/SPEC.md): `POST /$/ontology/{ds}/patterns/preview`
// and `POST /$/ontology/{ds}/patterns/apply`.
import { receiptOf, request, type Receipt } from './api';
import type { AxiomJson, EntityKind, Selection } from './ontology';
import type { WritePreview } from './validation-overlay';

const enc = encodeURIComponent;

export type TableFormat = 'auto' | 'csv' | 'tsv';
export type PatternMode = 'robot' | 'pattern';
export type PatternSyntax = 'manchester' | 'functional';
export type PatternOnError = 'abort' | 'skip';

export type RobotColumnDirective = {
  column: string;
  directive: string;
  split?: string | null;
};

export type RuleAnnotationSpec = {
  property: string;
  value: string;
  lang?: string | null;
  datatype?: string | null;
  iri?: boolean;
};

export type PatternRuleSpec = {
  name?: string | null;
  template: string;
  syntax?: PatternSyntax;
  skipIfEmpty?: boolean;
  requiredColumns?: string[];
  split?: Record<string, string>;
  annotations?: RuleAnnotationSpec[];
};

export type PatternDeclarationSpec = {
  column: string;
  kind: EntityKind;
  split?: string | null;
};

export type PatternSpec =
  | {
      mode: 'robot';
      baseIri?: string | null;
      prefixes?: Record<string, string>;
      headerTemplateRow?: boolean;
      columns?: RobotColumnDirective[];
    }
  | {
      mode: 'pattern';
      baseIri?: string | null;
      prefixes?: Record<string, string>;
      rules?: PatternRuleSpec[];
      declarations?: PatternDeclarationSpec[];
    };

export type PatternTableInput = {
  data: string;
  format?: TableFormat;
};

export type PatternPreviewRequest = {
  graph?: string;
  imports?: boolean;
  table: PatternTableInput;
  spec: PatternSpec;
  declare?: boolean;
  target?: string;
  limit?: number;
};

export type PatternApplyRequest = {
  graph?: string;
  imports?: boolean;
  table: PatternTableInput;
  spec: PatternSpec;
  declare?: boolean;
  target?: string;
  message?: string;
  onError?: PatternOnError;
};

export type RowDiagnostic = {
  row: number;
  column: string | null;
  severity: 'error' | 'warning';
  code: string;
  message: string;
};

export type GeneratedAxiomPreview = {
  type: string;
  ofn: string;
  manchester: string | null;
  axiom: AxiomJson;
  rows: number[];
  status: 'new' | 'existing';
};

export type AutoDeclaration = {
  iri: string;
  kind: string;
};

export type PatternPreviewResponse = {
  dataset: string;
  commit: number;
  graphs: string[];
  target: string;
  delimiter: 'csv' | 'tsv';
  headers: string[];
  valid: boolean;
  rowsProcessed: number;
  rowsSkipped: number;
  rowsWithErrors: number;
  generated: number;
  newAxioms: number;
  existingAxioms: number;
  autoDeclarations: AutoDeclaration[];
  estimatedTriples: number;
  axioms: GeneratedAxiomPreview[];
  axiomsTruncated: boolean;
  diagnostics: RowDiagnostic[];
  diagnosticsTruncated: boolean;
  /** Entity tag from the response header, reusable as `If-Match` on apply. */
  etag: string | null;
};

export type PatternApplySummary = {
  delimiter: string;
  headers: string[];
  rowsProcessed: number;
  rowsSkipped: number;
  rowsWithErrors: number;
  generated: number;
  valid: boolean;
  diagnostics: RowDiagnostic[];
  diagnosticsTruncated: boolean;
};

export type PatternApplyResponse = {
  added: number;
  present: number;
  removed: number;
  absent: string[];
  declared: string[];
  addedAxioms: { type: string; ofn: string; axiom: AxiomJson }[];
  removedAxioms: { type: string; ofn: string; axiom: AxiomJson }[];
  axiomsTruncated: boolean;
  inserted: number;
  deleted: number;
  kept: number;
  committed: boolean;
  pattern: PatternApplySummary;
  receipt?: Receipt;
};

export type PatternPreset = {
  id: string;
  label: string;
  description: string;
  format: TableFormat;
  data: string;
  spec: PatternSpec;
  commitMessage: string;
};

/** Built-in starter presets covering ROBOT templates, DOSDP/Cellfie rules, and individuals. */
export const PATTERN_PRESETS: readonly PatternPreset[] = [
  {
    id: 'robot-hierarchy',
    label: 'ROBOT class hierarchy',
    description: 'Inline row-2 ROBOT directives for classes, labels, superclasses, and comments.',
    format: 'csv',
    data: [
      'ID,Label,Type,Parent,Comment',
      'ID,LABEL@en,TYPE,SC %,A rdfs:comment',
      'ex:Heart,Heart,owl:Class,owl:Thing,Muscular organ that pumps blood',
      'ex:CardiacValve,Cardiac valve,owl:Class,ex:Heart,Controls unidirectional blood flow',
      'ex:MitralValve,Mitral valve,owl:Class,ex:CardiacValve,Valve between left atrium and ventricle',
    ].join('\n'),
    spec: {
      mode: 'robot',
      headerTemplateRow: true,
      prefixes: { ex: 'http://example.org/onto#' },
    },
    commitMessage: 'Generate class hierarchy from ROBOT template',
  },
  {
    id: 'robot-restrictions',
    label: 'ROBOT restrictions & SPLIT',
    description:
      'Existential restrictions (`SC ex:partOf some %`) and pipe-delimited synonyms (`SPLIT=|`).',
    format: 'csv',
    data: [
      'ID,Label,Parent,PartOf,Synonyms,Source',
      'ID,LABEL@en,SC %,SC ex:partOf some %,A skos:altLabel SPLIT=|,>A dcterms:source',
      'ex:Organ,Organ,owl:Thing,,Anatomical organ,Ref:01',
      'ex:Heart,Heart,ex:Organ,,Cardiac organ|Pump,Ref:02',
      'ex:Pericardium,Pericardium,ex:Organ,ex:Heart,Heart sac|Pericardial sac,Ref:03',
    ].join('\n'),
    spec: {
      mode: 'robot',
      headerTemplateRow: true,
      prefixes: { ex: 'http://example.org/onto#' },
    },
    commitMessage: 'Generate partonomy restrictions from ROBOT template',
  },
  {
    id: 'dosdp-manchester',
    label: 'DOSDP / Cellfie Manchester pattern',
    description:
      'Parameterized Manchester and Functional-style rules with `${col|iri}` and `${col|lang(en)}` filters.',
    format: 'csv',
    data: [
      'class,label,genus,relation,filler,curator',
      'HeartFailure,Heart failure,ex:Disorder,ex:affects,ex:Heart,curator-1',
      'ValveStenosis,Valve stenosis,ex:Disorder,ex:affects,ex:CardiacValve,curator-2',
    ].join('\n'),
    spec: {
      mode: 'pattern',
      baseIri: 'http://example.org/onto#',
      prefixes: { ex: 'http://example.org/onto#' },
      declarations: [
        { column: 'class', kind: 'Class' },
        { column: 'genus', kind: 'Class' },
        { column: 'relation', kind: 'ObjectProperty' },
        { column: 'filler', kind: 'Class' },
      ],
      rules: [
        {
          name: 'equivalent-definition',
          template: '${class|iri} EquivalentTo ${genus|iri} and ${relation|iri} some ${filler|iri}',
          syntax: 'manchester',
          skipIfEmpty: true,
          annotations: [{ property: 'dcterms:creator', value: '${curator}' }],
        },
        {
          name: 'label',
          template: 'AnnotationAssertion(rdfs:label ${class|iri} ${label|lang(en)})',
          syntax: 'functional',
          skipIfEmpty: true,
        },
      ],
    },
    commitMessage: 'Generate defined classes from DOSDP pattern',
  },
  {
    id: 'robot-individuals',
    label: 'Individuals & assertions (TSV)',
    description: 'Tab-separated named individuals typed by classes with labels.',
    format: 'tsv',
    data: [
      'ID\tLabel\tClass\tDescription',
      'ID\tLABEL@en\tTYPE\tA rdfs:comment',
      'ex:clinic_alpha\tClinic Alpha\tex:Clinic\tPrimary cardiology site',
      'ex:clinic_beta\tClinic Beta\tex:Clinic\tOutpatient cardiology clinic',
    ].join('\n'),
    spec: {
      mode: 'robot',
      headerTemplateRow: true,
      prefixes: { ex: 'http://example.org/onto#' },
    },
    commitMessage: 'Generate named individuals from TSV template',
  },
];

/**
 * Extract the header column names from the first non-empty line of a CSV/TSV string for
 * UI column pickers and placeholder chips.
 */
export function parsePreviewHeaders(data: string, format: TableFormat = 'auto'): string[] {
  const text = data.replace(/^\uFEFF/, '');
  const lines = text.split(/\r?\n/);
  const first = lines.find((l) => l.trim().length > 0);
  if (!first) return [];
  const sep =
    format === 'tsv'
      ? '\t'
      : format === 'csv'
        ? ','
        : countOutsideQuotes(first, '\t') > countOutsideQuotes(first, ',')
          ? '\t'
          : ',';

  const cols: string[] = [];
  let cur = '';
  let inQuotes = false;
  for (let i = 0; i < first.length; i++) {
    const ch = first[i];
    if (ch === '"') {
      if (inQuotes && first[i + 1] === '"') {
        cur += '"';
        i++;
      } else {
        inQuotes = !inQuotes;
      }
    } else if (ch === sep && !inQuotes) {
      cols.push(cur.trim());
      cur = '';
    } else {
      cur += ch;
    }
  }
  cols.push(cur.trim());
  return cols.filter((c) => c.length > 0);
}

function countOutsideQuotes(line: string, target: string): number {
  let inQuotes = false;
  let count = 0;
  for (let i = 0; i < line.length; i++) {
    const ch = line[i];
    if (ch === '"') {
      if (inQuotes && line[i + 1] === '"') {
        i++;
      } else {
        inQuotes = !inQuotes;
      }
    } else if (!inQuotes && ch === target) {
      count++;
    }
  }
  return count;
}

/** Parse multiline `prefix=namespace` (or `prefix: namespace`) text into a prefix map. */
export function parsePrefixLines(text: string): {
  prefixes: Record<string, string>;
  error: string | null;
} {
  const prefixes: Record<string, string> = {};
  const lines = text.split(/\r?\n/);
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i].trim();
    if (!line || line.startsWith('#')) continue;
    const eq = line.indexOf('=');
    const colon = line.indexOf(':');
    let splitAt = eq;
    if (splitAt < 0 && colon > 0) splitAt = colon;
    if (splitAt <= 0) {
      return {
        prefixes,
        error: `Line ${i + 1}: expected 'prefix=http://namespace/'`,
      };
    }
    const key = line.slice(0, splitAt).trim().replace(/:$/, '');
    const ns = line.slice(splitAt + 1).trim();
    if (!key || !ns) {
      return {
        prefixes,
        error: `Line ${i + 1}: both prefix and namespace IRI are required`,
      };
    }
    prefixes[key] = ns;
  }
  return { prefixes, error: null };
}

/** Format a prefix map as `prefix=namespace` lines for editing. */
export function formatPrefixLines(prefixes: Record<string, string> | undefined): string {
  if (!prefixes) return '';
  return Object.entries(prefixes)
    .map(([k, v]) => `${k}=${v}`)
    .join('\n');
}

function buildBody(
  sel: Selection,
  req: PatternPreviewRequest | PatternApplyRequest,
): Record<string, unknown> {
  const body: Record<string, unknown> = { ...req };
  if (sel.graph) body.graph = sel.graph;
  body.imports = sel.imports;
  if (sel.graph && !body.target) body.target = sel.graph;
  return body;
}

/** `POST /$/ontology/{ds}/patterns/preview`: preview generated axioms and row diagnostics. */
export async function previewPatterns(
  target: string,
  sel: Selection,
  req: PatternPreviewRequest,
  opts: { at?: string; signal?: AbortSignal } = {},
): Promise<PatternPreviewResponse> {
  const params = new URLSearchParams();
  if (opts.at) params.set('at', opts.at);
  const qs = params.toString();
  const res = await request(`/$/ontology/${enc(target)}/patterns/preview${qs ? `?${qs}` : ''}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify(buildBody(sel, req)),
    signal: opts.signal,
  });
  const data = (await res.json()) as Omit<PatternPreviewResponse, 'etag'>;
  return { ...data, etag: res.headers.get('etag') };
}

/** `POST /$/ontology/{ds}/patterns/apply?dryRun=true`: server dry-run triple diff parity. */
export async function dryRunPatterns(
  target: string,
  sel: Selection,
  req: PatternApplyRequest,
  opts: { etag?: string | null; signal?: AbortSignal } = {},
): Promise<WritePreview> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json',
  };
  if (opts.etag) headers['If-Match'] = opts.etag;
  const res = await request(`/$/ontology/${enc(target)}/patterns/apply?dryRun=true`, {
    method: 'POST',
    headers,
    body: JSON.stringify(buildBody(sel, req)),
    signal: opts.signal,
  });
  return (await res.json()) as WritePreview;
}

/** `POST /$/ontology/{ds}/patterns/apply?receipt=true`: commit all generated axioms atomically. */
export async function applyPatterns(
  target: string,
  sel: Selection,
  req: PatternApplyRequest,
  opts: { etag?: string | null; signal?: AbortSignal } = {},
): Promise<PatternApplyResponse> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json',
  };
  if (opts.etag) headers['If-Match'] = opts.etag;
  const res = await request(`/$/ontology/${enc(target)}/patterns/apply?receipt=true`, {
    method: 'POST',
    headers,
    body: JSON.stringify(buildBody(sel, req)),
    signal: opts.signal,
  });
  const data = (await res.json()) as PatternApplyResponse & Record<string, unknown>;
  const receipt = receiptOf(data);
  return receipt ? { ...data, receipt } : data;
}
