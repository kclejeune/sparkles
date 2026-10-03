// Mock of a dataset's DESCRIBE setting for UI development (docs/API.md, "DESCRIBE
// modes"): `GET`, `PUT` and `DELETE /$/describe/{ds}`. A `PUT` replaces the setting with
// the options it names, the others taking their defaults, and checks them as the server
// does. `DELETE` goes back to the defaults.

const MODES = ['cbd', 'scbd', 'outgoing'];
const DEFAULTS = { mode: 'cbd', labels: false, reifiers: false, maxTriples: null, maxDepth: null };

/** The setting as `GET /$/describe/{ds}` reports it. */
export function describeStatus(own) {
  return { ...DEFAULTS, ...(own ?? {}), source: own ? 'dataset' : 'default', modes: MODES };
}

/** The options of a `PUT` body, or `{ error }`. */
export function parseDescribe(body) {
  if (!body || typeof body !== 'object' || Array.isArray(body))
    return { error: 'DESCRIBE options must be a JSON object' };
  const o = { ...DEFAULTS };
  for (const [k, v] of Object.entries(body)) {
    if (k === 'source' || k === 'modes' || k === 'format') continue;
    if (k === 'mode') {
      if (!MODES.includes(String(v).toLowerCase()))
        return { error: `unknown DESCRIBE mode ${JSON.stringify(v)}: cbd, scbd or outgoing` };
      o.mode = String(v).toLowerCase();
    } else if (k === 'labels' || k === 'reifiers') {
      if (typeof v !== 'boolean') return { error: `${k}: expected true or false, not ${v}` };
      o[k] = v;
    } else if (k === 'maxTriples' || k === 'maxDepth') {
      if (v === null) o[k] = null;
      else if (Number.isSafeInteger(v) && v >= 0) o[k] = v === 0 ? null : v;
      else return { error: `${k}: expected a whole number, not ${JSON.stringify(v)}` };
    } else return { error: `unknown DESCRIBE option ${k}` };
  }
  return { ok: o };
}

export async function handleDescribe(req, res, url, seg, ctx) {
  if (seg[0] !== '$' || seg[1] !== 'describe') return false;
  const name = seg[2];
  const ds = name ? ctx.datasets.get(name) : undefined;
  if (!ds) return (ctx.send(res, 404, { error: `No such dataset: ${name ?? ''}` }), true);
  switch (req.method) {
    case 'GET':
      return (ctx.send(res, 200, describeStatus(ds.describe)), true);
    case 'PUT': {
      let body;
      try {
        body = JSON.parse((await ctx.readBody(req)).toString('utf8'));
      } catch {
        return (ctx.send(res, 400, { error: 'invalid JSON' }), true);
      }
      const r = parseDescribe(body);
      if (r.error) return (ctx.send(res, 400, { error: r.error }), true);
      ds.describe = r.ok;
      return (ctx.send(res, 200, describeStatus(ds.describe)), true);
    }
    case 'DELETE':
      ds.describe = undefined;
      return (ctx.send(res, 200, describeStatus(undefined)), true);
    default:
      return (ctx.send(res, 405, { error: 'method not allowed' }), true);
  }
}
