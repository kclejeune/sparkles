// Global UI state (Svelte 5 runes in a module).
import * as api from './api';
import { WELL_KNOWN, type PrefixMap } from './rdf';
import { loadRaw, saveRaw } from './storage';

type Theme = 'system' | 'light' | 'dark';

class AppState {
  datasets = $state<api.DatasetInfo[]>([]);
  datasetsError = $state<string | null>(null);
  datasetsLoaded = $state(false);
  current = $state<string | null>(loadRaw('sparkles.dataset'));
  online = $state<boolean | null>(null);
  lastPingMs = $state<number | null>(null);
  theme = $state<Theme>((loadRaw('sparkles.theme') as Theme) ?? 'system');
  /** Prefix maps per dataset (well-known ∪ server-provided). */
  prefixMaps = $state<Record<string, PrefixMap>>({});
  /** Local names of predicates and classes per dataset, for autocomplete. */
  vocab = $state<Record<string, string[]>>({});

  /** A query another page wants opened in a new Query tab. */
  pendingQuery = $state<{ query: string; title?: string } | null>(null);

  setDataset(name: string | null) {
    this.current = name;
    saveRaw('sparkles.dataset', name);
    if (name) void this.loadPrefixes(name);
  }

  setTheme(t: Theme) {
    this.theme = t;
    saveRaw('sparkles.theme', t === 'system' ? null : t);
    if (t === 'system') delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = t;
  }

  async refreshDatasets() {
    try {
      this.datasets = await api.listDatasets();
      this.datasetsError = null;
      if (!this.current || !this.datasets.some((d) => d.name === this.current)) {
        this.setDataset(this.datasets[0]?.name ?? null);
      } else if (!this.prefixMaps[this.current]) {
        void this.loadPrefixes(this.current);
      }
    } catch (e) {
      this.datasetsError = api.errorMessage(e);
    } finally {
      this.datasetsLoaded = true;
    }
  }

  prefixes(ds: string | null | undefined): PrefixMap {
    return (ds && this.prefixMaps[ds]) || WELL_KNOWN;
  }

  async loadPrefixes(ds: string, force = false) {
    if (!force && this.prefixMaps[ds]) return this.prefixMaps[ds];
    try {
      const p = await api.prefixes(ds);
      this.prefixMaps[ds] = { ...WELL_KNOWN, ...p };
    } catch {
      this.prefixMaps[ds] = { ...WELL_KNOWN };
    }
    return this.prefixMaps[ds];
  }

  async loadVocab(ds: string) {
    if (this.vocab[ds]) return this.vocab[ds];
    try {
      const s = await api.datasetStats(ds);
      this.vocab[ds] = [...s.predicates.map((p) => p.iri), ...s.classes.map((c) => c.iri)];
    } catch {
      this.vocab[ds] = [];
    }
    return this.vocab[ds];
  }

  async ping() {
    const t0 = performance.now();
    const ctrl = new AbortController();
    const timer = setTimeout(() => ctrl.abort(), 4000);
    try {
      await api.ping(ctrl.signal);
      this.online = true;
      this.lastPingMs = performance.now() - t0;
    } catch {
      this.online = false;
      this.lastPingMs = null;
    } finally {
      clearTimeout(timer);
    }
  }
}

export const app = new AppState();

// --- toasts -------------------------------------------------------------------

export type Toast = {
  id: number;
  kind: 'info' | 'success' | 'error';
  text: string;
  detail?: string;
  /** The server's request id of a failed request, to look it up in the server log. */
  requestId?: string;
};

class Toasts {
  items = $state<Toast[]>([]);
  #seq = 0;
  push(
    kind: Toast['kind'],
    text: string,
    detail?: string,
    ttl = kind === 'error' ? 8000 : 3500,
    requestId?: string,
  ) {
    const id = ++this.#seq;
    this.items.push({ id, kind, text, detail, requestId });
    setTimeout(() => this.dismiss(id), ttl);
  }
  dismiss(id: number) {
    this.items = this.items.filter((t) => t.id !== id);
  }
  error(text: string, e?: unknown) {
    const requestId = e instanceof api.ApiError ? e.requestId : undefined;
    this.push('error', text, e ? api.errorMessage(e) : undefined, undefined, requestId);
  }
}

export const toasts = new Toasts();
