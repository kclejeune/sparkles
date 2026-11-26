// Client and starter presets for pattern-based OWL 2 axiom generation (ROADMAP item 623,
// docs/specs/U12-ontology-workbench/SPEC.md): `POST /$/ontology/{ds}/patterns/preview`
// and `POST /$/ontology/{ds}/patterns/apply`.
import { receiptOf, request, type Receipt } from './api';
import { fmtInt } from './format';
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
 * Extract the header column names from the first non-blank logical RFC 4180 record of a
 * CSV/TSV string (handling leading blank lines and quoted multiline header cells) for UI
 * column pickers and placeholder chips.
 */
export function parsePreviewHeaders(data: string, format: TableFormat = 'auto'): string[] {
  const text = data.replace(/^\uFEFF/, '');
  let pos = 0;
  while (pos < text.length) {
    const start = pos;
    let inQuotes = false;
    let tabs = 0;
    let commas = 0;
    let hasContent = false;
    let recordEnd = text.length;
    while (pos < text.length) {
      const ch = text[pos];
      if (ch === '"') {
        if (inQuotes && text[pos + 1] === '"') {
          hasContent = true;
          pos += 2;
          continue;
        }
        inQuotes = !inQuotes;
        pos++;
      } else if (!inQuotes && ch === '\r') {
        recordEnd = pos;
        pos += text[pos + 1] === '\n' ? 2 : 1;
        break;
      } else if (!inQuotes && ch === '\n') {
        recordEnd = pos;
        pos++;
        break;
      } else if (!inQuotes && ch === '\t') {
        tabs++;
        pos++;
      } else if (!inQuotes && ch === ',') {
        commas++;
        pos++;
      } else {
        if (ch.trim().length > 0) hasContent = true;
        pos++;
      }
    }
    if (!hasContent) continue;
    const record = text.slice(start, recordEnd);
    const sep = format === 'tsv' ? '\t' : format === 'csv' ? ',' : tabs > commas ? '\t' : ',';
    const cols: string[] = [];
    let cur = '';
    let quoted = false;
    for (let i = 0; i < record.length; i++) {
      const ch = record[i];
      if (ch === '"') {
        if (quoted && record[i + 1] === '"') {
          cur += '"';
          i++;
        } else {
          quoted = !quoted;
        }
      } else if (ch === sep && !quoted) {
        cols.push(cur.trim());
        cur = '';
      } else {
        cur += ch;
      }
    }
    cols.push(cur.trim());
    return cols.filter((c) => c.length > 0);
  }
  return [];
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

export type PatternAuthoringState = {
  mode: PatternMode;
  baseIri: string;
  prefixes: Record<string, string>;
  headerTemplateRow: boolean;
  robotColumns: RobotColumnDirective[];
  rules: PatternRuleSpec[];
  declarations: PatternDeclarationSpec[];
};

/** Build a normalized `PatternSpec` payload while preserving optional rule fields. */
export function buildPatternSpec(state: PatternAuthoringState): PatternSpec {
  const base = state.baseIri.trim() || null;
  if (state.mode === 'robot') {
    const cols = state.robotColumns
      .filter((c) => c.column.trim() && c.directive.trim())
      .map((c) => ({
        column: c.column.trim(),
        directive: c.directive.trim(),
        ...(c.split?.trim() ? { split: c.split.trim() } : {}),
      }));
    return {
      mode: 'robot',
      baseIri: base,
      prefixes: state.prefixes,
      headerTemplateRow: state.headerTemplateRow,
      ...(cols.length > 0 ? { columns: cols } : {}),
    };
  }
  return {
    mode: 'pattern',
    baseIri: base,
    prefixes: state.prefixes,
    rules: state.rules
      .filter((r) => r.template.trim().length > 0)
      .map((r) => {
        const reqCols = (r.requiredColumns ?? []).map((c) => c.trim()).filter(Boolean);
        const splitEntries = Object.entries(r.split ?? {}).filter(
          ([k, v]) => k.trim().length > 0 && v.length > 0,
        );
        return {
          ...(r.name?.trim() ? { name: r.name.trim() } : {}),
          template: r.template.trim(),
          syntax: r.syntax ?? 'manchester',
          skipIfEmpty: r.skipIfEmpty ?? true,
          ...(reqCols.length > 0 ? { requiredColumns: reqCols } : {}),
          ...(splitEntries.length > 0 ? { split: Object.fromEntries(splitEntries) } : {}),
          ...(r.annotations?.length ? { annotations: r.annotations } : {}),
        };
      }),
    declarations: state.declarations
      .filter((d) => d.column.trim().length > 0)
      .map((d) => ({
        column: d.column.trim(),
        kind: d.kind,
        ...(d.split?.trim() ? { split: d.split.trim() } : {}),
      })),
  };
}

/** Stable signature of every input that affects preview output. */
export function patternInputSignature(args: {
  target: string;
  sel: Selection;
  mode: PatternMode;
  format: TableFormat;
  tableData: string;
  baseIri: string;
  prefixText: string;
  headerTemplateRow: boolean;
  robotColumns: RobotColumnDirective[];
  rules: PatternRuleSpec[];
  declarations: PatternDeclarationSpec[];
  declareEntities: boolean;
}): string {
  return JSON.stringify({
    target: args.target,
    graph: args.sel.graph,
    imports: args.sel.imports,
    mode: args.mode,
    format: args.format,
    tableData: args.tableData,
    baseIri: args.baseIri,
    prefixText: args.prefixText,
    headerTemplateRow: args.headerTemplateRow,
    robotColumns: args.robotColumns,
    rules: args.rules,
    declarations: args.declarations,
    declareEntities: args.declareEntities,
  });
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

/** `POST /$/ontology/{ds}/patterns/apply?dryRun=true&changes=50`: server dry-run triple diff parity. */
export async function dryRunPatterns(
  target: string,
  sel: Selection,
  req: PatternApplyRequest,
  opts: { etag?: string | null; changes?: number; signal?: AbortSignal } = {},
): Promise<WritePreview> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'application/json',
  };
  if (opts.etag) headers['If-Match'] = opts.etag;
  const changes = opts.changes ?? 50;
  const res = await request(
    `/$/ontology/${enc(target)}/patterns/apply?dryRun=true&changes=${changes}`,
    {
      method: 'POST',
      headers,
      body: JSON.stringify(buildBody(sel, req)),
      signal: opts.signal,
    },
  );
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

/** Format a concise human-readable summary of a completed pattern apply response. */
export function formatPatternCommitNotice(res: PatternApplyResponse): string {
  return res.committed
    ? `Committed ${fmtInt(res.added)} new axiom${res.added === 1 ? '' : 's'} (${fmtInt(res.inserted)} triple${res.inserted === 1 ? '' : 's'}) from ${fmtInt(res.pattern.rowsProcessed)} row${res.pattern.rowsProcessed === 1 ? '' : 's'}.`
    : `No new axioms to commit (${fmtInt(res.present)} already present).`;
}

export type PatternCommitToast = {
  kind: 'success' | 'info';
  title: string;
  detail: string;
};

/**
 * Decide whether the owning workbench page should display a toast for a completed
 * pattern commit. When the originating Patterns component is still mounted
 * (`handledInTab === true`), it displays the notice inline; otherwise (including when
 * the user switched away and reopened Patterns before settlement) the page presents a
 * toast so neither a committed summary nor a no-change outcome disappears.
 */
export function patternCommitToast(
  res: PatternApplyResponse,
  notice: string,
  handledInTab: boolean,
): PatternCommitToast | null {
  if (handledInTab) return null;
  return {
    kind: res.committed ? 'success' : 'info',
    title: res.committed ? 'Pattern axioms committed' : 'Pattern apply finished',
    detail: notice,
  };
}

export type PatternCommitHooks = {
  /**
   * Called by the owning workbench whenever a commit finishes, even after the Patterns
   * tab unmounts. `handledInTab` is true only when the originating Patterns component
   * instance remained mounted and rendered the inline notice.
   */
  onCommitted: (
    target: string,
    seq: number | null,
    notice: string,
    res: PatternApplyResponse,
    handledInTab: boolean,
  ) => void;
  /** Called by the owning workbench when a commit fails after the Patterns tab unmounted. */
  onDetachedError?: (target: string, error: unknown) => void;
  /** Called whenever the set of targets with an in-flight commit changes. */
  onPendingChange?: (pendingTargets: ReadonlySet<string>) => void;
};

export type PatternTabCommitCallbacks = {
  isMounted: () => boolean;
  onSuccess: (notice: string, res: PatternApplyResponse) => void;
  onError: (error: unknown) => void;
  onSettled: () => void;
};

/**
 * Workbench-scoped coordinator for in-flight pattern commits keyed by `target`.
 * Keeps commits reconciled with the mounted workbench page (refreshing views and surfacing
 * completion notices or errors) even when the user switches away from the Patterns tab
 * before the HTTP request finishes.
 */
export class PatternCommitCoordinator {
  private pending = new Set<string>();
  private hooks: PatternCommitHooks;

  constructor(hooks: PatternCommitHooks) {
    this.hooks = hooks;
  }

  isPending(target: string): boolean {
    return this.pending.has(target);
  }

  async submit(
    target: string,
    sel: Selection,
    req: PatternApplyRequest,
    opts: { etag?: string | null } = {},
    tab?: PatternTabCommitCallbacks,
  ): Promise<boolean> {
    if (this.pending.has(target)) return false;
    this.pending.add(target);
    this.hooks.onPendingChange?.(new Set(this.pending));
    try {
      const res = await applyPatterns(target, sel, req, { etag: opts.etag });
      const notice = formatPatternCommitNotice(res);
      const seq = res.receipt?.commit?.seq ?? null;
      const handledInTab = Boolean(tab?.isMounted());
      if (handledInTab) {
        tab!.onSuccess(notice, res);
      }
      this.hooks.onCommitted(target, seq, notice, res, handledInTab);
      return true;
    } catch (e) {
      if (tab?.isMounted()) {
        tab.onError(e);
      } else {
        this.hooks.onDetachedError?.(target, e);
      }
      return false;
    } finally {
      this.pending.delete(target);
      this.hooks.onPendingChange?.(new Set(this.pending));
      if (tab?.isMounted()) {
        tab.onSettled();
      }
    }
  }
}
