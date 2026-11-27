import test from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import {
  Dataset,
  BudgetExceededError,
  ConflictError,
  InvalidInputError,
  NotFoundError,
  PermissionDeniedError,
  UnsupportedError,
  evidenceResult,
} from '../dist/index.js';

const SOURCES_G = 'https://example.org/g/sources';
const PROPOSALS_G = 'https://example.org/g/proposals';
const DECISIONS_G = 'https://example.org/g/decisions';
const SUPPORTS_G = 'https://example.org/g/supports';
const IDENTITY_G = 'https://example.org/g/identity';
const ACCEPTED_G = 'https://example.org/g/accepted';
const MAP_OUT_G = 'https://example.org/g/entities';

function sha256(text) {
  return 'sha256:' + crypto.createHash('sha256').update(text, 'utf8').digest('hex');
}

function makeSettings(enabled = true) {
  return {
    format: 1,
    profile: 1,
    enabled,
    sourceRecords: [SOURCES_G],
    proposals: [PROPOSALS_G],
    decisions: [DECISIONS_G],
    supports: [SUPPORTS_G],
    identity: [IDENTITY_G],
    projections: [ACCEPTED_G, MAP_OUT_G],
    policies: {
      'review-v1': {
        version: 1,
        admittedKinds: ['reported'],
        requiredScope: [],
        conflicts: 'refuse',
        boundary: [SOURCES_G, PROPOSALS_G],
        decisions: DECISIONS_G,
        supports: SUPPORTS_G,
        output: ACCEPTED_G,
      },
    },
    matchingPolicies: {
      'match-v1': {
        version: 1,
        boundary: [IDENTITY_G, SOURCES_G, PROPOSALS_G],
        decisions: DECISIONS_G,
        maps: IDENTITY_G,
        output: MAP_OUT_G,
      },
    },
  };
}

function makeProposal({ text = 'Release 1 is affected by CVE-1.', idempotencyKey } = {}) {
  const quote = 'Release 1 is affected';
  const body = {
    revisions: [
      {
        key: 'rev1',
        source: 'https://example.org/source/advisory',
        sourceKey: 'adv-1',
        content: { text },
      },
    ],
    renditions: [
      {
        key: 'ren1',
        revision: { key: 'rev1' },
        occurrenceKey: 'text-v1',
        conversion: { method: 'https://example.org/methods/norm', version: '1' },
        mediaType: 'text/plain',
        content: { text },
      },
    ],
    passages: [
      {
        key: 'p1',
        revision: { key: 'rev1' },
        rendition: { key: 'ren1' },
        occurrenceKey: 'span-1',
        start: 0,
        end: quote.length,
        textDigest: sha256(quote),
        quote,
      },
    ],
    references: [
      {
        key: 'ref1',
        occurrenceKey: 'ref-1',
        passage: {
          revision: { key: 'rev1' },
          rendition: { key: 'ren1' },
          passage: { key: 'p1' },
        },
      },
    ],
    claims: [
      {
        key: 'c1',
        occurrenceKey: 'claim-1',
        revision: { key: 'rev1' },
        proposition:
          '<https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1>',
        kind: 'reported',
        polarity: 'positive',
        author: 'https://example.org/org/vendor',
      },
    ],
    links: [
      {
        key: 'l1',
        occurrenceKey: 'link-1',
        target: { key: 'c1' },
        evidence: { key: 'ref1' },
        relation: 'supports',
        author: 'https://example.org/org/importer',
      },
    ],
    assessments: [
      {
        key: 'a1',
        occurrenceKey: 'assess-1',
        target: { key: 'c1' },
        conclusion: '<https://example.org/status/high>',
        method: { method: 'https://example.org/methods/triage', version: '1' },
        configurationDigest: sha256('{}'),
        evaluatedAt: '2026-01-01T12:00:00Z',
        evaluator: 'https://example.org/org/analyst',
        inputs: [{ key: 'c1' }],
      },
    ],
  };
  if (idempotencyKey !== undefined) body.idempotencyKey = idempotencyKey;
  return body;
}

test('evidence settings, configure, detach and holds lifecycle', async (t) => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'sparkles-ev-node-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const ds = await Dataset.open(path.join(dir, 'db'));
  t.after(() => ds.close());

  const initial = await ds.evidence.settings();
  assert.equal(initial.settings.enabled, false);
  assert.equal(initial.digest, null);
  assert.deepEqual(initial.managed, []);
  assert.deepEqual(initial.detached, []);

  const configured = await ds.evidence.configure(makeSettings());
  assert.equal(configured.settings.enabled, true);
  assert.equal(typeof configured.revision, 'bigint');
  assert.equal(configured.revision, 1n);
  assert.equal(typeof configured.settings.policies['review-v1'].version, 'number');
  assert.ok(configured.managed.includes(ACCEPTED_G));
  assert.deepEqual(configured.detached, []);

  // Direct SPARQL update on a managed graph refuses with profile-managed-graph.
  await assert.rejects(
    ds.update(
      `INSERT DATA { GRAPH <${ACCEPTED_G}> { <https://example.org/s> <https://example.org/p> <https://example.org/o> } }`,
    ),
    (e) => e instanceof ConflictError && e.details.code === 'profile-managed-graph',
  );

  // Detach opens the graph, re-configure re-protects it.
  const detached = await ds.evidence.detach(ACCEPTED_G);
  assert.ok(detached.detached.includes(ACCEPTED_G));
  await ds.update(
    `INSERT DATA { GRAPH <${ACCEPTED_G}> { <https://example.org/s> <https://example.org/p> <https://example.org/o> } }`,
  );
  const reconfigured = await ds.evidence.configure(configured.settings);
  assert.ok(reconfigured.managed.includes(ACCEPTED_G));

  // Unsupported profile version refuses cleanly.
  await assert.rejects(
    ds.evidence.configure({ ...makeSettings(), profile: 99 }),
    (e) => e instanceof UnsupportedError && e.details.code === 'unsupported-profile',
  );

  // Holds lifecycle.
  await ds.update("INSERT DATA { <https://example.org/a> <https://example.org/p> '1' }");
  const c1 = (await ds.info()).head.seq;
  await ds.update("INSERT DATA { <https://example.org/a> <https://example.org/p> '2' }");
  const c2 = (await ds.info()).head.seq;

  const holdId = crypto.randomUUID();
  const acq = await ds.evidence.acquireHold([{ commit: c1 }], { id: holdId });
  assert.equal(acq.created, true);
  assert.equal(acq.hold.id, holdId);
  assert.equal(acq.hold.commits[0].commit, c1);

  const again = await ds.evidence.acquireHold([{ commit: c1 }], { id: holdId });
  assert.equal(again.created, false);

  await assert.rejects(
    ds.evidence.acquireHold([{ commit: c2 }], { id: holdId }),
    (e) => e instanceof ConflictError && e.details.code === 'hold-conflict',
  );

  const page = await ds.evidence.holds({ onBranch: 'main', limit: 10 });
  assert.equal(page.heldCommits, 1);
  assert.equal(page.holds.length, 1);
  assert.equal(page.holds[0].id, holdId);

  assert.equal(await ds.evidence.releaseHold(holdId), true);
  assert.equal(await ds.evidence.releaseHold(holdId), false);
});

test('evidence propose, resolve, review, withdraw, identities and dependents', async (t) => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'sparkles-ev-node-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const ds = await Dataset.open(path.join(dir, 'db'));
  t.after(() => ds.close());

  await ds.update("INSERT DATA { <https://example.org/fact> <https://example.org/p> 'kept' }");
  const factCommit = (await ds.info()).head.seq;

  await assert.rejects(
    ds.evidence.propose(makeProposal()),
    (e) => e instanceof UnsupportedError && e.details.code === 'unsupported-profile',
  );

  await ds.evidence.configure(makeSettings());

  const preview = await ds.evidence.propose({ ...makeProposal(), dryRun: true });
  assert.equal(preview.committed, false);
  assert.equal(preview.dryRun.wouldCommit, true);
  assert.equal(typeof preview.dryRun.inserted, 'bigint');
  assert.equal(typeof preview.dryRun.deleted, 'bigint');

  const proposed = await ds.evidence.propose(makeProposal({ idempotencyKey: 'node-batch-1' }), {
    principal: 'https://example.org/users/alice',
  });
  assert.equal(proposed.committed, true);
  assert.equal(proposed.replayed, false);
  assert.equal(typeof proposed.commit, 'bigint');
  const byKey = Object.fromEntries(proposed.records.map((r) => [r.key, r]));
  const claim = byKey.c1;
  const assessment = byKey.a1;

  const replayed = await ds.evidence.propose(makeProposal({ idempotencyKey: 'node-batch-1' }), {
    principal: 'https://example.org/users/alice',
  });
  assert.equal(replayed.replayed, true);
  assert.equal(replayed.operation, proposed.operation);

  const resolved = await ds.evidence.resolve([
    {
      key: 'f1',
      storedFact: {
        datasetId: ds.id,
        branchId: ds.id,
        commit: factCommit,
        triple: '<https://example.org/fact> <https://example.org/p> "kept"',
      },
    },
    {
      key: 'r1',
      record: { iri: claim.iri, kinds: ['Claim'] },
    },
  ]);
  assert.equal(resolved.resolved.length, 2);
  assert.equal(typeof resolved.resolved[0].storedFact.commit, 'bigint');
  assert.equal(resolved.resolved[0].storedFact.servedBy, ds.id);
  assert.equal(resolved.resolved[0].storedFact.commit, factCommit);
  assert.equal(resolved.resolved[1].record.iri, claim.iri);
  assert.equal(typeof resolved.resolved[1].record.receipt.commit, 'bigint');

  // Claim states: unreviewed -> accepted -> withdrawn, plus null for unknown claims.
  const unrev = await ds.evidence.claimStates('review-v1', [
    claim.iri,
    'https://example.org/missing',
  ]);
  assert.equal(unrev.policy, 'review-v1');
  assert.equal(typeof unrev.commit, 'bigint');
  assert.equal(unrev.claims[0].state, 'unreviewed');
  assert.equal(unrev.claims[0].origin, 'source');
  assert.equal(unrev.claims[1], null);

  await assert.rejects(
    ds.evidence.resolve([
      { key: 'm1', record: { iri: 'https://example.org/missing', kinds: ['Claim'] } },
    ]),
    (e) => e instanceof NotFoundError && e.details.code === 'evidence-unavailable',
  );

  const deps = await ds.evidence.findDependents([claim.iri], { transitive: true });
  assert.ok(deps.dependents.some((d) => d.assessment === assessment.iri));

  const head = (await ds.info()).head.seq;
  const reviewed = await ds.evidence.review(
    {
      policy: 'review-v1',
      policyVersion: 1,
      ifHead: head,
      decisions: [
        {
          key: 'd1',
          target: claim.iri,
          targetDigest: claim.recordDigest,
          disposition: 'accepted',
        },
      ],
    },
    { principal: 'https://example.org/users/reviewer' },
  );
  assert.equal(reviewed.committed, true);
  assert.equal(reviewed.changed.inserted.length, 1);
  assert.equal(
    await ds.ask(
      `ASK { GRAPH <${ACCEPTED_G}> { <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> } }`,
    ),
    true,
  );

  await assert.rejects(
    ds.evidence.review({
      policy: 'review-v1',
      policyVersion: 1,
      ifHead: head,
      decisions: [
        {
          key: 'd2',
          target: claim.iri,
          targetDigest: claim.recordDigest,
          disposition: 'rejected',
        },
      ],
    }),
    (e) => e instanceof ConflictError && e.details.code === 'head-changed',
  );

  const withdrawn = await ds.evidence.withdraw(
    {
      withdrawals: [
        {
          key: 'w1',
          target: claim.iri,
          targetDigest: claim.recordDigest,
        },
      ],
    },
    { principal: 'https://example.org/users/reviewer' },
  );
  assert.equal(withdrawn.committed, true);
  assert.equal(withdrawn.changed.removed.length, 1);
  assert.equal(
    await ds.ask(
      `ASK { GRAPH <${ACCEPTED_G}> { <https://example.org/rel/1> <https://example.org/affectedBy> <https://example.org/cve/1> } }`,
    ),
    false,
  );

  const idProp = await ds.evidence.propose({
    identities: [
      {
        key: 'id1',
        occurrenceKey: 'match-1',
        entities: ['https://example.org/e/a', 'https://example.org/e/b'],
        relation: 'possibleMatch',
        preferredEntity: 'https://example.org/e/a',
        author: 'https://example.org/org/linker',
      },
    ],
  });
  const idRec = idProp.records[0];
  const idRev = await ds.evidence.review({
    policy: 'match-v1',
    policyVersion: 1,
    ifHead: (await ds.info()).head.seq,
    decisions: [
      {
        key: 'md1',
        target: idRec.iri,
        targetDigest: idRec.recordDigest,
        disposition: 'accepted',
      },
    ],
  });
  assert.ok(idRev.mapRevision);
  assert.equal(typeof idRev.changed.map.number, 'bigint');
  assert.equal(idRev.changed.map.number, 1n);
  const ent = await ds.evidence.resolveEntities('match-v1', ['https://example.org/e/b']);
  assert.equal(typeof ent.number, 'bigint');
  assert.equal(ent.number, 1n);
  assert.equal(typeof ent.commit, 'bigint');
  assert.equal(ent.entities[0].representative, 'https://example.org/e/a');
  assert.deepEqual([...ent.entities[0].members].sort(), [
    'https://example.org/e/a',
    'https://example.org/e/b',
  ]);
});

test('evidence transaction-owner, in-txn holds, branch errors, integer precision and controls', async (t) => {
  const dir = await fs.mkdtemp(path.join(os.tmpdir(), 'sparkles-ev-node-'));
  t.after(() => fs.rm(dir, { recursive: true, force: true }));
  const ds = await Dataset.open(path.join(dir, 'db'));
  await ds.evidence.configure(makeSettings());
  await ds.branches.create('feat');

  // In-transaction holds() and claimStates() succeed on a persistent store without deadlocking,
  // while mutating evidence operations are refused with ConflictError.
  await ds.transaction(async () => {
    const page = await ds.evidence.holds();
    assert.equal(page.heldCommits, 0);
    const states = await ds.evidence.claimStates('review-v1', [], { timeout: 200 });
    assert.deepEqual(states.claims, []);
    await assert.rejects(ds.evidence.configure(makeSettings()), ConflictError);
    await assert.rejects(ds.evidence.detach(ACCEPTED_G), ConflictError);
    await assert.rejects(ds.evidence.acquireHold([{ commit: 0 }]), ConflictError);
    await assert.rejects(ds.evidence.releaseHold(crypto.randomUUID()), ConflictError);
    await assert.rejects(ds.evidence.propose(makeProposal()), ConflictError);
    await assert.rejects(
      ds.evidence.review({ policy: 'review-v1', policyVersion: 1, ifHead: 0, decisions: [] }),
      ConflictError,
    );
    await assert.rejects(ds.evidence.withdraw({ withdrawals: [] }), ConflictError);
  });

  // Unknown branch surfaces as NotFoundError with details.code === 'no-such-branch',
  // and history-gone on acquireHold preserves details.code === 'history-gone'.
  await assert.rejects(
    ds.evidence.resolve([], { branch: 'missing' }),
    (e) => e instanceof NotFoundError && e.details.code === 'no-such-branch',
  );
  await assert.rejects(
    ds.evidence.acquireHold([{ commit: 999999n }]),
    (e) => e instanceof NotFoundError && e.details.code === 'history-gone',
  );

  // Branch handles refuse evidence operations uniformly with InvalidInputError, and an open
  // branch transaction still allows main-handle claimStates() to read committed state.
  const br = await ds.branch('feat');
  await br.transaction(async () => {
    const states = await ds.evidence.claimStates('review-v1', [], {
      branch: 'feat',
      timeout: 200,
    });
    assert.deepEqual(states.claims, []);
  });
  await assert.rejects(br.evidence.settings(), InvalidInputError);
  await assert.rejects(br.evidence.holds(), InvalidInputError);
  await assert.rejects(
    br.evidence.configure({ format: 1, profile: 1, enabled: false }),
    InvalidInputError,
  );
  await assert.rejects(br.evidence.claimStates('review-v1', []), InvalidInputError);
  await br.close();

  // Integer precision and runtime-type guards: unsafe JS numbers, u64 overflow, and
  // non-integer `at` values (including booleans) reject with InvalidInputError.
  const unsafeInt = Number.MAX_SAFE_INTEGER + 2;
  const overU64 = 18446744073709551616n;
  await assert.rejects(ds.evidence.acquireHold([{ commit: unsafeInt }]), InvalidInputError);
  await assert.rejects(ds.evidence.acquireHold([{ commit: overU64 }]), InvalidInputError);
  await assert.rejects(
    ds.evidence.propose({ ...makeProposal(), ifHead: unsafeInt }),
    InvalidInputError,
  );
  await assert.rejects(
    ds.evidence.review({ policy: 'review-v1', policyVersion: 1, ifHead: unsafeInt, decisions: [] }),
    InvalidInputError,
  );
  await assert.rejects(
    ds.evidence.withdraw({ ifHead: overU64, withdrawals: [] }),
    InvalidInputError,
  );
  await assert.rejects(ds.evidence.holds({ limit: 1.5 }), InvalidInputError);
  await assert.rejects(ds.evidence.claimStates('review-v1', [], { at: true }), InvalidInputError);
  await assert.rejects(ds.evidence.claimStates('review-v1', [], { at: false }), InvalidInputError);
  await assert.rejects(ds.evidence.claimStates('review-v1', [], { at: null }), InvalidInputError);

  // Exact payload byte bounds (R2-4): raw settings text is measured by UTF-8 bytes without
  // JSON string escaping inflation, nested requests are bounded at the payload boundary
  // without 4 KiB slack, and list endpoints bound admission before cloning.
  const newlinePaddedSettings = '\n'.repeat(9 * 1024 * 1024) + JSON.stringify(makeSettings());
  const paddedResult = await ds.evidence.configure(newlinePaddedSettings);
  assert.equal(paddedResult.settings.enabled, true);

  const overSettings = ' '.repeat(16 * 1024 * 1024) + '{}';
  await assert.rejects(ds.evidence.configure(overSettings), BudgetExceededError);

  const overReview = {
    policy: 'review-v1',
    policyVersion: 1,
    ifHead: 0,
    message: 'a'.repeat(4 * 1024 * 1024 + 64),
    decisions: [],
  };
  await assert.rejects(ds.evidence.review(overReview), BudgetExceededError);

  const overWithdraw = {
    message: 'a'.repeat(4 * 1024 * 1024 + 64),
    withdrawals: [],
  };
  await assert.rejects(ds.evidence.withdraw(overWithdraw), BudgetExceededError);

  await assert.rejects(
    ds.evidence.claimStates(
      'review-v1',
      Array.from({ length: 10_001 }, () => 'https://example.org/c/1'),
    ),
    InvalidInputError,
  );
  await assert.rejects(
    ds.evidence.resolveEntities(
      'match-v1',
      Array.from({ length: 101 }, () => 'https://example.org/e/1'),
    ),
    InvalidInputError,
  );
  await assert.rejects(
    ds.evidence.findDependents(
      Array.from({ length: 1_001 }, () => 'https://example.org/c/1'),
    ),
    InvalidInputError,
  );

  // Synthetic response above JS safe integer range decodes losslessly to BigInt
  // while leaving settings numbers and IRI revision strings unchanged.
  const maxU64 = '18446744073709551615';
  const decoded = evidenceResult({
    revision: maxU64,
    number: maxU64,
    commit: maxU64,
    servedBy: maxU64,
    map: { revision: 'urn:ev:map:1', number: maxU64 },
    span: { rendition: 'urn:ev:rend:1', start: maxU64, end: maxU64, textDigest: 'sha256:00' },
    settings: { policies: { 'review-v1': { version: 1 } } },
  });
  assert.equal(decoded.revision, 18446744073709551615n);
  assert.equal(decoded.number, 18446744073709551615n);
  assert.equal(decoded.commit, 18446744073709551615n);
  assert.equal(decoded.servedBy, 18446744073709551615n);
  assert.equal(decoded.map.revision, 'urn:ev:map:1');
  assert.equal(decoded.map.number, 18446744073709551615n);
  assert.equal(decoded.span.start, 18446744073709551615n);
  assert.equal(decoded.span.end, 18446744073709551615n);
  assert.equal(decoded.settings.policies['review-v1'].version, 1);

  // Pre-aborted AbortSignal refuses controlled evidence operations.
  const ac = new AbortController();
  ac.abort(new Error('stop-now'));
  await assert.rejects(ds.evidence.configure(makeSettings(), { signal: ac.signal }), /stop-now/);
  await assert.rejects(ds.evidence.resolve([], { signal: ac.signal }), /stop-now/);
  await assert.rejects(ds.evidence.claimStates('review-v1', [], { signal: ac.signal }), /stop-now/);

  // Closed dataset refuses all evidence operations.
  await ds.close();
  await assert.rejects(ds.evidence.settings(), InvalidInputError);
  await assert.rejects(ds.evidence.holds(), InvalidInputError);
  await assert.rejects(ds.evidence.resolve([]), InvalidInputError);
  await assert.rejects(ds.evidence.configure(makeSettings()), InvalidInputError);
  await assert.rejects(ds.evidence.claimStates('review-v1', []), InvalidInputError);
  await assert.rejects(
    ds.evidence.resolveEntities('match-v1', ['https://example.org/e/a']),
    InvalidInputError,
  );
  await assert.rejects(ds.evidence.findDependents(['https://example.org/c/1']), InvalidInputError);

  // Read-only dataset refuses mutating evidence operations.
  const ro = await Dataset.open(path.join(dir, 'db'), { readOnly: true });
  t.after(() => ro.close());
  assert.equal((await ro.evidence.settings()).settings.enabled, true);
  await assert.rejects(ro.evidence.configure(makeSettings()), PermissionDeniedError);
  await assert.rejects(ro.evidence.propose(makeProposal()), PermissionDeniedError);
});
