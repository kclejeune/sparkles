import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  applyPatterns,
  buildPatternSpec,
  dryRunPatterns,
  formatPrefixLines,
  parsePrefixLines,
  parsePreviewHeaders,
  PATTERN_PRESETS,
  PatternCommitCoordinator,
  patternCommitToast,
  patternInputSignature,
  previewPatterns,
} from './ontology-patterns';

describe('ontology-patterns helpers', () => {
  it('parses CSV and TSV headers including BOM, leading blanks, quoted multiline and duplicate columns', () => {
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
    // Leading blank lines before TSV header
    expect(parsePreviewHeaders('\n  \nID\tLabel\tParent\nex:A\tA\towl:Thing', 'auto')).toEqual([
      'ID',
      'Label',
      'Parent',
    ]);
    // Quoted header cell with embedded newline
    expect(parsePreviewHeaders('ID,"Display\nLabel",Parent\nex:A,Heart,ex:Organ')).toEqual([
      'ID',
      'Display\nLabel',
      'Parent',
    ]);
    // Duplicate header names preserved in order (chips are keyed by index)
    expect(parsePreviewHeaders('ID,Note,Note\nex:A,a,b')).toEqual(['ID', 'Note', 'Note']);
    expect(parsePreviewHeaders('\n   \n')).toEqual([]);
  });

  it('preserves requiredColumns and split in buildPatternSpec and detects input/imports changes', () => {
    const spec = buildPatternSpec({
      mode: 'pattern',
      baseIri: 'http://ex.org/',
      prefixes: { ex: 'http://ex.org/' },
      headerTemplateRow: false,
      robotColumns: [],
      rules: [
        {
          name: 'parts',
          template: '${cls|iri} SubClassOf ex:partOf some ${parts|iri}',
          syntax: 'manchester',
          skipIfEmpty: true,
          requiredColumns: ['cls', ' '],
          split: { parts: '|', unused: '' },
          annotations: [{ property: 'dcterms:source', value: '${src}' }],
        },
      ],
      declarations: [{ column: 'cls', kind: 'Class', split: '' }],
    });
    expect(spec.mode).toBe('pattern');
    if (spec.mode === 'pattern') {
      expect(spec.rules?.[0].requiredColumns).toEqual(['cls']);
      expect(spec.rules?.[0].split).toEqual({ parts: '|' });
      expect(spec.rules?.[0].annotations).toHaveLength(1);
    }

    const baseArgs = {
      target: 'bio',
      sel: { graph: null, imports: true },
      mode: 'robot' as const,
      format: 'csv' as const,
      tableData: 'ID,Label\nID,LABEL\nex:A,A\n',
      baseIri: '',
      prefixText: 'ex=http://ex.org/',
      headerTemplateRow: true,
      robotColumns: [],
      rules: [],
      declarations: [],
      declareEntities: true,
    };
    const sig1 = patternInputSignature(baseArgs);
    const sigImports = patternInputSignature({
      ...baseArgs,
      sel: { graph: null, imports: false },
    });
    const sigData = patternInputSignature({
      ...baseArgs,
      tableData: 'ID,Label\nID,LABEL\nex:B,B\n',
    });
    const sigDeclare = patternInputSignature({
      ...baseArgs,
      declareEntities: false,
    });
    expect(sigImports).not.toBe(sig1);
    expect(sigData).not.toBe(sig1);
    expect(sigDeclare).not.toBe(sig1);
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
    expect(fetchMock.mock.calls[0][0]).toBe(
      '/$/ontology/bio/patterns/apply?dryRun=true&changes=50',
    );

    const applied = await applyPatterns('bio', { graph: null, imports: true }, req, {
      etag: 'W/"bio:5:owl"',
    });
    expect(applied.committed).toBe(true);
    const [applyUrl, applyInit] = fetchMock.mock.calls[1] as [string, RequestInit];
    expect(applyUrl).toBe('/$/ontology/bio/patterns/apply?receipt=true');
    expect((applyInit.headers as Record<string, string>)['If-Match']).toBe('W/"bio:5:owl"');
  });

  it('reconciles committed writes and surfaces detached feedback across unmount and remount', async () => {
    const makeResponse = (committed: boolean, seq: number) =>
      new Response(
        JSON.stringify({
          added: committed ? 2 : 0,
          present: committed ? 0 : 2,
          removed: 0,
          absent: [],
          declared: [],
          addedAxioms: [],
          removedAxioms: [],
          axiomsTruncated: false,
          inserted: committed ? 3 : 0,
          deleted: 0,
          kept: committed ? 0 : 3,
          committed,
          commit: { seq },
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
        { status: 200, headers: { 'Content-Type': 'application/json' } },
      );

    let resolveFetch!: (res: Response) => void;
    const pendingFetch = new Promise<Response>((resolve) => {
      resolveFetch = resolve;
    });
    const fetchMock = vi.fn().mockReturnValueOnce(pendingFetch);
    vi.stubGlobal('fetch', fetchMock);

    // Mirror the workbench page presentation gate: activeTab can return to 'patterns'
    // after unmount, and toast presentation depends on `handledInTab`, not `activeTab`.
    let activeTab: 'tree' | 'patterns' = 'patterns';
    const committed: {
      target: string;
      seq: number | null;
      notice: string;
      handledInTab: boolean;
    }[] = [];
    const pageToasts: ReturnType<typeof patternCommitToast>[] = [];
    const detachedErrors: { target: string; error: unknown }[] = [];
    const pendingSnapshots: string[][] = [];
    const coordinator = new PatternCommitCoordinator({
      onCommitted: (t, seq, notice, res, handledInTab) => {
        committed.push({ target: t, seq, notice, handledInTab });
        const toast = patternCommitToast(res, notice, handledInTab);
        if (toast) pageToasts.push(toast);
      },
      onDetachedError: (t, error) => detachedErrors.push({ target: t, error }),
      onPendingChange: (set) => pendingSnapshots.push([...set]),
    });

    let origMounted = true;
    const tabSuccess = vi.fn();
    const tabSettled = vi.fn();
    const req = {
      table: { data: 'ID\nID\nex:A\n' },
      spec: { mode: 'robot' as const, headerTemplateRow: true },
      message: 'Load A',
    };

    const submitPromise = coordinator.submit(
      'bio',
      { graph: null, imports: true },
      req,
      { etag: 'W/"bio:5:owl-patterns.abc"' },
      {
        isMounted: () => origMounted,
        onSuccess: tabSuccess,
        onError: vi.fn(),
        onSettled: tabSettled,
      },
    );

    // While pending, duplicate submits on the same target are suppressed.
    expect(coordinator.isPending('bio')).toBe(true);
    const dup = await coordinator.submit('bio', { graph: null, imports: true }, req);
    expect(dup).toBe(false);

    // User switches from Patterns to Tree (destroying the originating Patterns instance)
    // and then reopens Patterns BEFORE settlement (activeTab === 'patterns', origMounted === false).
    activeTab = 'tree';
    origMounted = false;
    activeTab = 'patterns';
    expect(activeTab).toBe('patterns');

    resolveFetch(makeResponse(true, 11));

    expect(await submitPromise).toBe(true);
    expect(coordinator.isPending('bio')).toBe(false);
    expect(committed).toEqual([
      {
        target: 'bio',
        seq: 11,
        notice: 'Committed 2 new axioms (3 triples) from 1 row.',
        handledInTab: false,
      },
    ]);
    // Unmounted originating tab callbacks were NOT invoked, while the page toast WAS emitted
    // even though activeTab had returned to 'patterns'.
    expect(tabSuccess).not.toHaveBeenCalled();
    expect(tabSettled).not.toHaveBeenCalled();
    expect(pageToasts).toEqual([
      {
        kind: 'success',
        title: 'Pattern axioms committed',
        detail: 'Committed 2 new axioms (3 triples) from 1 row.',
      },
    ]);
    expect(pendingSnapshots).toEqual([['bio'], []]);

    // Remounted Patterns before a no-change (`committed: false`) settlement also surfaces
    // an info toast instead of silently clearing the pending indicator.
    let resolveNoChange!: (res: Response) => void;
    fetchMock.mockReturnValueOnce(
      new Promise<Response>((resolve) => {
        resolveNoChange = resolve;
      }),
    );
    let origMountedNoChange = true;
    const noChangePromise = coordinator.submit(
      'bio',
      { graph: null, imports: true },
      req,
      {},
      {
        isMounted: () => origMountedNoChange,
        onSuccess: vi.fn(),
        onError: vi.fn(),
        onSettled: vi.fn(),
      },
    );
    origMountedNoChange = false;
    activeTab = 'patterns';
    resolveNoChange(makeResponse(false, 11));
    expect(await noChangePromise).toBe(true);
    expect(pageToasts[1]).toEqual({
      kind: 'info',
      title: 'Pattern apply finished',
      detail: 'No new axioms to commit (2 already present).',
    });

    // When the originating Patterns instance stays mounted, it handles the notice inline
    // and the page suppresses any duplicate toast.
    fetchMock.mockReturnValueOnce(Promise.resolve(makeResponse(true, 12)));
    const inlineSuccess = vi.fn();
    const inlineSettled = vi.fn();
    expect(
      await coordinator.submit(
        'bio',
        { graph: null, imports: true },
        req,
        {},
        {
          isMounted: () => true,
          onSuccess: inlineSuccess,
          onError: vi.fn(),
          onSettled: inlineSettled,
        },
      ),
    ).toBe(true);
    expect(inlineSuccess).toHaveBeenCalledTimes(1);
    expect(inlineSettled).toHaveBeenCalledTimes(1);
    expect(pageToasts).toHaveLength(2);

    // Detached failure also notifies the parent workbench after unmount.
    let rejectFetch!: (err: unknown) => void;
    const failingFetch = new Promise<Response>((_, reject) => {
      rejectFetch = reject;
    });
    fetchMock.mockReturnValueOnce(failingFetch);
    let mounted2 = true;
    const failPromise = coordinator.submit(
      'bio',
      { graph: null, imports: true },
      req,
      {},
      {
        isMounted: () => mounted2,
        onSuccess: vi.fn(),
        onError: vi.fn(),
        onSettled: vi.fn(),
      },
    );
    mounted2 = false;
    rejectFetch(new Error('network down'));
    expect(await failPromise).toBe(false);
    expect(detachedErrors).toHaveLength(1);
    expect(detachedErrors[0].target).toBe('bio');
  });
});
