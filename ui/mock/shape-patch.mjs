// RDF Patch stand-in for visual shape authoring. Real-server tests cover the actual
// patch parser and stored blank identities; this mock preserves the same request flow.
import { randomUUID } from 'node:crypto';
import ox from 'oxigraph';

export function handleShapePatch(
  req,
  res,
  ds,
  p,
  { send, fail, headCommit, commitWrite, quadSet, overlaySummary, commitMessageOf },
) {
  const body = String(p.get('__body') ?? '');
  const prev = /^H prev <urn:uuid:([^#>]+)#commit:(\d+)> \.$/m.exec(body);
  const head = headCommit(ds).seq;
  const dry = p.get('dryRun') === 'true';
  const expected = `urn:uuid:${ds.branchId ?? ds.id}#commit:${head}`;
  if (
    p.get('expect') !== expected ||
    !prev ||
    prev[1] !== (ds.branchId ?? ds.id) ||
    Number(prev[2]) !== head
  ) {
    return send(
      res,
      412,
      dry
        ? {
            dryRun: true,
            committed: false,
            wouldCommit: false,
            outcome: 'precondition-failed',
            head,
          }
        : { error: 'The patch expects an older head.', code: 'prev-mismatch' },
    );
  }
  const candidate = new ox.Store(ds.store.match());
  const local = randomUUID().replaceAll('-', '');
  try {
    for (const line of body.split('\n')) {
      const row = /^([AD]) (.*) \.$/.exec(line);
      if (!row) continue;
      const stored = new Map();
      const text = row[2].replace(
        /("(?:[^"\\]|\\.)*")|<_:([A-Za-z0-9_.-]+)>|_:([A-Za-z0-9_.-]+)/g,
        (all, literal, old, fresh) => {
          if (literal) return literal;
          if (old) {
            const key = `known_${stored.size}`;
            stored.set(key, old);
            return '_:' + key;
          }
          return `_:new_${local}_${fresh}`;
        },
      );
      const parsed = new ox.Store();
      parsed.load(text + ' .', { format: 'application/n-quads' });
      if (parsed.size !== 1) throw new Error('A patch row must contain one quad.');
      const t = parsed.match()[0];
      const term = (v) =>
        v.termType === 'BlankNode' && stored.has(v.value) ? ox.blankNode(stored.get(v.value)) : v;
      const quad = ox.quad(term(t.subject), t.predicate, term(t.object), term(t.graph));
      if (row[1] === 'A') candidate.add(quad);
      else candidate.delete(quad);
    }
  } catch (e) {
    return fail(res, 400, `Invalid RDF Patch: ${e.message}`);
  }
  const before = ds.store.match(),
    after = candidate.match();
  const had = new Set(before.map(String)),
    has = new Set(after.map(String));
  const removed = before.filter((q) => !has.has(String(q))),
    added = after.filter((q) => !had.has(String(q)));
  const validation = overlaySummary(ds, candidate);
  const rejected = !!validation && ds.guard?.mode === 'reject' && validation.blocking > 0;
  if (dry) {
    const quad = (op) => (q) => ({
      op,
      subject: String(q.subject),
      predicate: String(q.predicate),
      object: String(q.object),
      graph: q.graph.termType === 'DefaultGraph' ? null : String(q.graph),
    });
    const changes = [...removed.map(quad('-')), ...added.map(quad('+'))];
    const limit = Number(p.get('changes') ?? 50);
    return send(res, rejected ? 422 : 200, {
      dryRun: true,
      committed: false,
      wouldCommit: !rejected && !!changes.length,
      outcome: rejected ? 'rejected' : changes.length ? 'commit' : 'no-change',
      head,
      commit: { seq: head + 1, inserted: added.length, deleted: removed.length },
      changes: {
        total: changes.length,
        limit,
        truncated: changes.length > limit,
        quads: changes.slice(0, limit),
      },
      ...(validation ? { validation } : {}),
    });
  }
  if (rejected) return fail(res, 422, 'The write-time guard rejected the patch.');
  const old = quadSet(ds);
  ds.store = candidate;
  const receipt = commitWrite(ds, 'patch', old, { message: commitMessageOf(req) });
  return send(res, 200, {
    inserted: added.length,
    deleted: removed.length,
    prevChecked: true,
    ...receipt,
  });
}
