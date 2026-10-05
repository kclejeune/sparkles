import type { ValidationConfig, ValidationSource, WriteValidation } from './api';

export type GuardForm = {
  language: 'shacl' | 'shex';
  mode: 'warn' | 'reject';
  baseline: 'strict' | 'grandfather';
  threshold: 'violation' | 'warning' | 'info';
  includeInferences: boolean;
  dataSelection: 'default' | 'union' | 'graphs';
  dataGraphs: string;
  source: 'keep' | 'inline' | 'graphs';
  text: string;
  syntax: string;
  sourceGraphs: string;
  shapeMap: string;
  base: string;
  timeoutSeconds: number;
  reportLimit: number;
};
const lines = (s: string) => s.split(/\s+/).filter(Boolean);

export function guardForm(v: WriteValidation | null): GuardForm {
  const c = v?.config;
  const src = v?.language === 'shex' ? c?.schema : c?.shapes;
  return {
    language: v?.language ?? 'shacl',
    mode: c?.mode === 'reject' ? 'reject' : 'warn',
    baseline: c?.baseline ?? 'strict',
    threshold: c?.threshold ?? 'violation',
    includeInferences: c?.includeInferences ?? false,
    dataSelection: Array.isArray(c?.dataGraph)
      ? 'graphs'
      : c?.dataGraph === 'union'
        ? 'union'
        : 'default',
    dataGraphs: Array.isArray(c?.dataGraph) ? c.dataGraph.join('\n') : '',
    source:
      src?.inline !== undefined
        ? 'inline'
        : src?.graphs && !src.file
          ? 'graphs'
          : src
            ? 'keep'
            : 'inline',
    text: src?.inline ?? '',
    syntax: src?.format ?? (v?.language === 'shex' ? 'shexc' : 'text/turtle'),
    sourceGraphs: src?.graphs?.join('\n') ?? '',
    shapeMap:
      typeof c?.shapeMap === 'string'
        ? c.shapeMap
        : c?.shapeMap
          ? JSON.stringify(c.shapeMap, null, 2)
          : '',
    base: src?.base ?? '',
    timeoutSeconds: c?.timeoutSeconds ?? 10,
    reportLimit: c?.reportLimit ?? 100,
  };
}

/** Preserve source metadata when reusing it, and replace it when changing sources. */
export function guardConfig(f: GuardForm, previous: WriteValidation | null): ValidationConfig {
  if (!Number.isFinite(f.timeoutSeconds) || f.timeoutSeconds <= 0)
    throw new Error('Timeout must be positive.');
  if (!Number.isInteger(f.reportLimit) || f.reportLimit < 1 || f.reportLimit > 10000)
    throw new Error('Report limit must be from 1 to 10,000.');
  let source: ValidationSource;
  if (f.source === 'keep') {
    if (previous?.language !== f.language) throw new Error('Choose a source for this language.');
    const old = f.language === 'shex' ? previous.config.schema : previous.config.shapes;
    if (!old) throw new Error('Choose a shapes or schema source.');
    // Loaded configurations may be Svelte reactive proxies.
    source = JSON.parse(JSON.stringify(old)) as ValidationSource;
  } else if (f.source === 'graphs') {
    const graphs = lines(f.sourceGraphs);
    if (!graphs.length) throw new Error('Name at least one source graph.');
    source = { graphs };
  } else {
    if (!f.text.trim()) throw new Error('Enter the shapes or schema.');
    source = { inline: f.text, format: f.syntax };
    if (
      f.language === 'shex' &&
      previous?.language === 'shex' &&
      previous.config.schema?.inline === f.text
    ) {
      source.prefixes = previous.config.schema.prefixes;
    }
    // SHACL can combine inline shapes with named shape graphs.
    if (f.language === 'shacl' && lines(f.sourceGraphs).length)
      source.graphs = lines(f.sourceGraphs);
  }
  const config: ValidationConfig = {
    language: f.language,
    mode: f.mode,
    baseline: f.baseline,
    includeInferences: f.includeInferences,
    dataGraph: f.dataSelection === 'graphs' ? lines(f.dataGraphs) : f.dataSelection,
    timeoutSeconds: f.timeoutSeconds,
    reportLimit: f.reportLimit,
  };
  if (f.language === 'shacl') {
    config.shapes = source;
    config.threshold = f.threshold;
  } else {
    if (!f.shapeMap.trim()) throw new Error('Enter a shape map.');
    config.schema = source;
    if (f.base.trim()) config.schema.base = f.base.trim();
    else delete config.schema.base;
    config.shapeMap = f.shapeMap.trim().startsWith('[') ? JSON.parse(f.shapeMap) : f.shapeMap;
    if (typeof config.shapeMap !== 'string' && !Array.isArray(config.shapeMap))
      throw new Error('Shape map must be compact text or a JSON array.');
  }
  return config;
}
