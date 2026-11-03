// Every address of the pages whose tabs are routes or parameters (ROADMAP item 633): the
// Admin tabs, Similar's Search and Map, and the Browse page's views. The rail, the palette,
// in-app links and the tests build these addresses here, so moving a page or splitting a
// merged one changes this module rather than its callers.

import { base } from '$app/paths';

const q = (params: Record<string, string | null | undefined>) => {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v) p.set(k, v);
  const s = p.toString();
  return s ? `?${s}` : '';
};

export const routes = {
  /** The app's base path, such as `/ui`. */
  get base() {
    return base;
  },
  adminServer: () => `${base}/admin/server`,
  adminBackups: (params: Record<string, string | null | undefined> = {}) =>
    `${base}/admin/backups${q(params)}`,
  adminTokens: () => `${base}/admin/tokens`,
  /** Similar's Search tab. */
  similar: (ds: string | null, params: Record<string, string | null | undefined> = {}) =>
    `${base}/similar${q({ ds, ...params })}`,
  /** Similar's Map tab: the embedding map of a vector index. */
  similarMap: (ds: string | null, params: Record<string, string | null | undefined> = {}) =>
    `${base}/similar/map${q({ ds, ...params })}`,
};
