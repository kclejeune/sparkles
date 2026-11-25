import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  applyPatterns,
  dryRunPatterns,
  formatPrefixLines,
  parsePrefixLines,
  parsePreviewHeaders,
  PATTERN_PRESETS,
  previewPatterns,
} from './ontology-patterns';

describe('ontology-patterns helpers', () => {
  it('parses CSV and TSV headers including BOM and quoted columns', () => {
    expect(parsePreviewHeaders('\uFEFFID,"Display, Label",Parent\n1,2,3')).toEqual([
      'ID',
      'Display, Label',
      'Parent',
    ]);
    expect(parsePreviewHeaders('ID\tLabel\tParent\nex:A\tA\towl:Thing', 'auto')).toEqual([
      'ID',
      'Label',
      'Parent',
    ]);
    expect(parsePreviewHeaders('\n   \n')).toEqual([]);
  });

  it('parses and formats prefix binding lines', () => {
    const parsed = parsePrefixLines(
      '# comment\nex=http://example.org/\nskos: http://www.w3.org/2004/02/skos/core#\n',
    );
    expect(parsed.error).toBeNull();
    expect(parsed.prefixes).toEqual({
      ex: 'http://example.org/',
      skos: 'http://www.w3.org/2004/02/skos/core#',
    });
    expect(formatPrefixLines(parsed.prefixes)).toContain('ex=http://example.org/');

    const invalid = parsePrefixLines('not-a-valid-line');
    expect(invalid.error).toContain('Line 1');
  });

  it('ships valid starter presets with non-empty headers and specs', () => {
    expect(PATTERN_PRESETS.length).toBeGreaterThanOrEqual(4);
    for (const p of PATTERN_PRESETS) {
      expect(parsePreviewHeaders(p.data, p.format).length).toBeGreaterThan(0);
      expect(['robot', 'pattern']).toContain(p.spec.mode);
    }
  });
});

describe('ontology-patterns HTTP client', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('posts preview request with graph selection and returns ETag', async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          dataset: 'bio',
          commit: 5,
          graphs: ['http://ex.org/g'],
          target: 'http://ex.org/g',
          delimiter: 'csv',
          headers: ['ID', 'Label'],
          valid: true,
          rowsProcessed: 1,
          rowsSkipped: 0,
          rowsWithErrors: 0,
          generated: 2,
          newAxioms: 2,
          existingAxioms: 0,
          autoDeclarations: [],
          estimatedTriples: 2,
          axioms: [],
          axiomsTruncated: false,
          diagnostics: [],
          diagnosticsTruncated: false,
        }),
        { status: 200, headers: { 'Content-Type': 'application/json', ETag: 'W/"bio:5:owl"' } },
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const res = await previewPatterns(
      'bio@dev',
      { graph: 'http://ex.org/g', imports: false },
      {
        table: { data: 'ID,Label\nID,LABEL\nex:A,A\n', format: 'csv' },
        spec: { mode: 'robot', headerTemplateRow: true },
        declare: true,
      },
      { at: 'commit:5' },
    );

    expect(res.etag).toBe('W/"bio:5:owl"');
    expect(res.newAxioms).toBe(2);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe('/$/ontology/bio/patterns/preview?at=commit%3A5&branch=dev');
    expect(init.method).toBe('POST');
    const sent = JSON.parse(String(init.body));
    expect(sent.graph).toBe('http://ex.org/g');
    expect(sent.target).toBe('http://ex.org/g');
    expect(sent.imports).toBe(false);
  });

  it('sends dryRun and If-Match for dry-run and apply calls', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            dryRun: true,
            wouldCommit: true,
            outcome: 'ok',
            head: 5,
            commit: { inserted: 3, deleted: 0 },
            changes: { total: 3, limit: 20, truncated: false, quads: [] },
          }),
          {
            status: 200,
            headers: { 'Content-Type': 'application/json' },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            added: 2,
            present: 0,
            removed: 0,
            absent: [],
            declared: [],
            addedAxioms: [],
            removedAxioms: [],
            axiomsTruncated: false,
            inserted: 3,
            deleted: 0,
            kept: 0,
            committed: true,
            pattern: {
              delimiter: 'csv',
              headers: ['ID'],
              rowsProcessed: 1,
              rowsSkipped: 0,
              rowsWithErrors: 0,
              generated: 2,
              valid: true,
              diagnostics: [],
              diagnosticsTruncated: false,
            },
          }),
          {
            status: 200,
            headers: {
              'Content-Type': 'application/json',
              'X-Sparkles-Commit': '6',
            },
          },
        ),
      );
    vi.stubGlobal('fetch', fetchMock);

    const req = {
      table: { data: 'ID\nID\nex:A\n' },
      spec: { mode: 'robot' as const, headerTemplateRow: true },
      message: 'Load A',
    };

    const dry = await dryRunPatterns('bio', { graph: null, imports: true }, req, {
      etag: 'W/"bio:5:owl"',
    });
    expect(dry.commit?.inserted).toBe(3);
    expect(fetchMock.mock.calls[0][0]).toBe('/$/ontology/bio/patterns/apply?dryRun=true');

    const applied = await applyPatterns('bio', { graph: null, imports: true }, req, {
      etag: 'W/"bio:5:owl"',
    });
    expect(applied.committed).toBe(true);
    const [applyUrl, applyInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(applyUrl).toBe('/$/ontology/bio/patterns/apply?receipt=true');
    expect((applyInit.headers as Record<string, string>)['If-Match']).toBe('W/"bio:5:owl"');
  });
});
