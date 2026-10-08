import { describe, expect, it } from 'vitest';
import type { WriteValidation } from './api';
import { guardConfig, guardForm } from './validation-config';
const validation = (
  language: 'shacl' | 'shex',
  config: WriteValidation['config'],
): WriteValidation => ({ language, config, status: {} as WriteValidation['status'] });

describe('editable guard configuration', () => {
  it('keeps every mode, so re-saving an off guard leaves it off', () => {
    for (const mode of ['off', 'warn', 'reject'] as const) {
      const v = validation('shacl', { mode, shapes: { inline: '', format: 'text/turtle' } });
      const f = guardForm(v);
      expect(f.mode).toBe(mode);
      expect(guardConfig({ ...f, text: '<urn:s> a <urn:C> .' }, v).mode).toBe(mode);
    }
    expect(guardForm(null).mode).toBe('warn');
  });
  it('retains installed sources and advanced settings', () => {
    const v = validation('shex', {
      mode: 'reject',
      schema: { file: 'validation-schema.shexj', format: 'shexj', prefixes: { ex: 'urn:ex:' } },
      shapeMap: [{ node: 'urn:n', shape: 'urn:S' }],
      dataGraph: ['urn:g'],
      includeInferences: true,
      baseline: 'grandfather',
      timeoutSeconds: 3.5,
      reportLimit: 12,
    });
    const f = guardForm(v);
    expect(f.source).toBe('keep');
    expect(guardConfig(f, v)).toEqual({ ...v.config, language: 'shex' });
    expect(v.config.schema?.file).toBe('validation-schema.shexj');
  });
  it('round trips memory inline schema without a nonexistent copied file', () => {
    const v = validation('shex', {
      mode: 'warn',
      schema: { inline: '<urn:S> {}', format: 'shexc' },
      shapeMap: '<urn:n>@<urn:S>',
    });
    const c = guardConfig(guardForm(v), v);
    expect(c.schema).toEqual({ inline: '<urn:S> {}', format: 'shexc' });
    expect(c.shapeMap).toBe('<urn:n>@<urn:S>');
  });
  it('replaces source metadata and combines SHACL inline and graph shapes', () => {
    const v = validation('shacl', {
      mode: 'warn',
      shapes: { file: 'validation-shapes.ttl', sha256: 'old' },
    });
    const f = {
      ...guardForm(v),
      source: 'inline' as const,
      text: '<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> .',
      sourceGraphs: 'urn:shape:a\nurn:shape:b',
      threshold: 'warning' as const,
    };
    expect(guardConfig(f, v).shapes).toEqual({
      inline: f.text,
      format: 'text/turtle',
      graphs: ['urn:shape:a', 'urn:shape:b'],
    });
    expect(guardConfig(f, v).threshold).toBe('warning');
  });
  it('rejects invalid budgets and missing or stale language sources before dispatch', () => {
    const f = { ...guardForm(null), text: '<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> .' };
    expect(() => guardConfig({ ...f, reportLimit: 0 }, null)).toThrow('Report limit');
    expect(() => guardConfig({ ...f, timeoutSeconds: NaN }, null)).toThrow('Timeout');
    expect(() => guardConfig({ ...f, source: 'keep' }, null)).toThrow('source');
    expect(() => guardConfig({ ...f, source: 'graphs' }, null)).toThrow('graph');
  });
});
