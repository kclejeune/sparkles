export interface BackupPolicy {
  name: string;
  repository: string;
  schedule: string;
  datasets?: string[];
  timezone?: string;
  nameTemplate?: string;
  retention?: { expireAfter?: string; minCount?: number; maxCount?: number };
  skipUnchanged?: boolean;
  gcAfterRetention?: boolean;
  catchUp?: 'one' | 'none';
  enabled?: boolean;
}
export interface PolicyRun {
  id: string;
  policy: string;
  trigger: 'schedule' | 'catch-up' | 'manual';
  scheduledFor: string | null;
  started: string;
  finished: string | null;
  result: 'ok' | 'partial' | 'failed' | 'skipped';
  reason?: string;
  datasets: {
    dataset: string;
    backup: string | null;
    result: 'ok' | 'failed' | 'skipped';
    reason?: string;
    addedBytes?: bigint;
    millis?: bigint;
  }[];
  retention: { deleted: string[]; error?: string } | null;
  gc: { task: string } | null;
}
export interface RetentionBackup {
  name: string;
  repository: string;
  dataset: {
    name: string;
    id: string;
    type?: string;
    branch?: { datasetId: string; id: string; name: string; nextOrdinal: number };
  };
  commit: { seq: bigint; timestamp: string; quads: bigint; ref: string };
  created: string;
  completed: string;
  millis: bigint;
  logicalBytes: bigint;
  addedBytes: bigint;
  policy: string | null;
  run: string | null;
  note: string | null;
  sameLineage?: boolean;
  verified: {
    level: 'exists' | 'data' | 'restore';
    status: 'ok' | 'warning' | 'error';
    at: string;
  } | null;
}
export interface RetentionReport {
  dryRun: boolean;
  delete: RetentionBackup[];
  keep: RetentionBackup[];
  errors?: string[];
}
// Only metadata from typed policy results enters this conversion. Arbitrary
// strings (including digit-only names, notes and identifiers) stay strings.
const integers = new Set([
  'addedBytes',
  'logicalBytes',
  'millis',
  'seq',
  'quads',
  'bytes',
  'totalBytes',
  'storedBytes',
  'files',
  'blobs',
  'newBlobs',
  'reusedBlobs',
  'branchesOmitted',
  'plainBytes',
  'objects',
]);
export function policyResult(value: any, key = ''): any {
  if (typeof value === 'string' && key === 'nextOrdinal' && /^\d+$/.test(value))
    return Number(value);
  if (typeof value === 'string' && integers.has(key) && /^\d+$/.test(value)) return BigInt(value);
  if (Array.isArray(value)) return value.map((item) => policyResult(item, key));
  if (value && typeof value === 'object')
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, policyResult(v, k)]));
  return value;
}
