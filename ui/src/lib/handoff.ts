// Handoff links of MCP `share_query` (C18 §9.1): `/ui/query?ds=X#ask=<payload>`, where
// the payload is base64url, without padding, of compact JSON. The payload sits in the
// fragment so that it never reaches the server's logs.

/** What a handoff link carries. */
export type Handoff = {
  dataset: string;
  query: string;
  question?: string;
  explanation?: string;
  assumptions?: string[];
  branch?: string;
  atCommit?: number;
};

export const MAX_QUERY = 65_536;
export const MAX_QUESTION = 2000;
export const MAX_EXPLANATION = 400;
export const MAX_ASSUMPTIONS = 5;
/** The tool refuses payloads over 32 KiB; a link may still carry a little more. */
export const MAX_PAYLOAD = 64 * 1024;

/** The payload of a location fragment `#ask=…`, or null when it has none. */
export function askPayload(hash: string): string | null {
  const h = hash.startsWith('#') ? hash.slice(1) : hash;
  for (const part of h.split('&')) {
    if (part.startsWith('ask=')) return part.slice(4);
  }
  return null;
}

function fromBase64Url(s: string): string {
  if (!/^[A-Za-z0-9_-]*$/.test(s)) throw new Error('The link is not base64url.');
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/');
  const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4);
  const bin = atob(padded);
  const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
  return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
}

/** base64url of the UTF-8 of a string, without padding (to build links in tests). */
export function toBase64Url(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

export const encodeHandoff = (h: Handoff) => toBase64Url(JSON.stringify(h));

const str = (v: unknown, name: string, max: number): string | undefined => {
  if (v == null) return undefined;
  if (typeof v !== 'string') throw new Error(`The link's ${name} is not text.`);
  if (v.length > max) throw new Error(`The link's ${name} is longer than ${max} characters.`);
  return v;
};

/** Decode and validate a payload; throws an Error whose message is for the person. */
export function decodeHandoff(payload: string): Handoff {
  if (payload.length > MAX_PAYLOAD) throw new Error('The link is too long.');
  let raw: unknown;
  try {
    raw = JSON.parse(fromBase64Url(payload));
  } catch {
    throw new Error('The link is damaged and could not be read.');
  }
  if (!raw || typeof raw !== 'object' || Array.isArray(raw))
    throw new Error('The link is damaged and could not be read.');
  const o = raw as Record<string, unknown>;
  const dataset = str(o.dataset, 'dataset', 256);
  if (!dataset) throw new Error('The link names no dataset.');
  const query = str(o.query, 'query', MAX_QUERY);
  if (!query?.trim()) throw new Error('The link holds no query.');
  const out: Handoff = { dataset, query };
  const question = str(o.question, 'question', MAX_QUESTION);
  if (question?.trim()) out.question = question.trim();
  const explanation = str(o.explanation, 'explanation', MAX_EXPLANATION);
  if (explanation?.trim()) out.explanation = explanation.trim();
  if (o.assumptions != null) {
    if (!Array.isArray(o.assumptions) || o.assumptions.some((a) => typeof a !== 'string'))
      throw new Error("The link's assumptions are not a list of text.");
    if (o.assumptions.length > MAX_ASSUMPTIONS)
      throw new Error(`The link has more than ${MAX_ASSUMPTIONS} assumptions.`);
    const list = (o.assumptions as string[]).map((a) => a.trim()).filter(Boolean);
    if (list.length) out.assumptions = list;
  }
  const branch = str(o.branch, 'branch', 256);
  if (branch?.trim()) out.branch = branch.trim();
  if (o.atCommit != null) {
    const n = typeof o.atCommit === 'string' ? Number(o.atCommit) : o.atCommit;
    if (typeof n !== 'number' || !Number.isInteger(n) || n < 0)
      throw new Error("The link's commit is not a commit number.");
    out.atCommit = n;
  }
  return out;
}
