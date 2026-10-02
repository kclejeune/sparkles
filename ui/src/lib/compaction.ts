// What the dataset page says about automatic compaction (`stats.compaction`).

import type { CompactionStatus } from './api';
import { fmtInt } from './format';

/** Why a due compaction waits, in words. */
const DEFERRED: Record<string, string> = {
  'min-interval': 'waiting for the minimum interval since the last compaction',
  backoff: 'waiting after a failed compaction',
  'bulk-load': 'waiting for a bulk load',
  reasoning: 'waiting for a reasoning run',
  backup: 'waiting for a backup to finish reading the index',
  restore: 'waiting for a restore',
  clone: 'waiting for a clone',
  history: 'waiting: compacting now would cut the retention window short',
  disk: 'waiting for free disk space',
  'running-limit': 'waiting for another dataset’s compaction',
  slots: 'waiting for a free task slot',
};

/** A short state label and one line of detail. */
export function compactionSummary(c: CompactionStatus): { label: string; detail: string } {
  const m = c.measures;
  const progress = `${fmtInt(m.deltaQuads)} of ${fmtInt(m.threshold)} delta quads`;
  switch (c.state) {
    case 'off':
      return {
        label: 'off',
        detail: c.serverEnabled ? 'turned off for this dataset' : 'turned off on the server',
      };
    case 'running':
      return { label: 'running', detail: c.trigger ?? 'compacting now' };
    case 'due':
      return { label: 'due', detail: c.trigger ?? progress };
    case 'deferred':
      return {
        label: 'waiting',
        detail: DEFERRED[c.deferred ?? ''] ?? c.deferredDetail ?? c.deferred ?? '',
      };
    default:
      return { label: 'on', detail: progress };
  }
}

/** The last compaction in one line, or null when there was none since the server started. */
export function lastCompaction(c: CompactionStatus): string | null {
  const l = c.last;
  if (!l) return null;
  const who = l.automatic ? 'automatic' : 'manual';
  if (l.outcome !== 'done')
    return `last ${who} compaction ${l.outcome}${l.error ? `: ${l.error}` : ''}`;
  const lock = l.lockMs == null ? '' : `, writer lock ${l.lockMs.toFixed(1)} ms`;
  return `last ${who} compaction took ${l.seconds.toFixed(2)} s${lock}`;
}
