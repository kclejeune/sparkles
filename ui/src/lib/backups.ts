// Typed client for backup repositories: `/$/repositories`, `/$/backups/{ds}`,
// `/$/backup-policies` and task cancellation (docs/API.md, "Backup repositories").

import { getTask, json, request, type Task } from './api';

const enc = encodeURIComponent;

const send = (method: string, body?: unknown): RequestInit =>
  body === undefined
    ? { method }
    : { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) };

// --- types -----------------------------------------------------------------------

export type RepositoryType = 'fs' | 's3' | 'gcs' | 'azure';

/**
 * Where a repository's credentials come from; the server never stores secrets. Repositories
 * registered through the API only name a source the operator defined in the server's backup
 * config file (`named`); the other forms are for config-file repositories.
 */
export type Credentials =
  | { source: 'default' }
  | { source: 'env'; accessKeyIdVar: string; secretAccessKeyVar: string; sessionTokenVar?: string }
  | { source: 'file'; path: string }
  | { source: 'named'; name: string };

export type RepositoryConfig = {
  name: string;
  type: RepositoryType;
  /** fs: an absolute path outside the data directory. */
  path?: string;
  bucket?: string;
  prefix?: string;
  region?: string;
  /** s3: the endpoint of an S3-compatible service (MinIO, R2, …). */
  endpoint?: string;
  pathStyle?: boolean;
  allowHttp?: boolean;
  credentials?: Credentials;
  sse?: null | 'AES256' | 'aws:kms';
  kmsKeyId?: string;
  /** Default true; false means one writer only. */
  conditionalWrites?: boolean;
  readonly?: boolean;
  maxConcurrency?: number;
  maxUploadBytesPerSec?: number | null;
  maxDownloadBytesPerSec?: number | null;
};

export type RepositoryStats = {
  backups: number;
  datasets: number;
  storedBytes: number;
  logicalBytes: number;
  dedupRatio: number;
  asOf: string;
};

export type TestStep = {
  step: 'create' | 'create-again' | 'read' | 'list' | 'delete';
  ok: boolean;
  millis: number;
  error?: string;
};
export type TestReport = { ok: boolean; conditionalWrites: boolean; steps: TestStep[] };

export type GcReport = {
  dryRun: boolean;
  manifests: number;
  referencedBlobs: number;
  listedBlobs: number;
  /** Unreferenced blobs past the grace period. */
  candidates: number;
  /** A dry run reports what a real run would delete. */
  deleted: number;
  deletedBytes: number;
  keptYoung: number;
  storedBytesAfter: number;
  requests: { list: number; get: number; delete: number };
  millis: number;
  lockWaitMillis: number;
};

/** What a server-admin sees of a repository. */
export type Repository = RepositoryConfig & {
  source: 'api' | 'config';
  /** The repository UUID; null until it was reached once. */
  id: string | null;
  status: {
    reachable: boolean;
    checked: string;
    error?: string;
    conditionalWrites?: boolean;
    singleWriter: boolean;
  };
  stats: RepositoryStats | null;
  lastGc?: null | (GcReport & { finished: string });
  /** Policies that back up into it (it cannot be removed while any does). */
  policies?: string[];
  /** The connection test, in the answer to a registration. */
  test?: TestReport;
};

/** What a dataset admin sees: enough to choose a target. */
export type RepositoryBrief = {
  name: string;
  type: RepositoryType;
  readonly: boolean;
  reachable: boolean;
};

export type BackupSummary = {
  name: string;
  repository: string;
  /** `type` is `mem` for a backup of an in-memory dataset. Older servers leave it out. */
  dataset: { name: string; id: string; type?: 'persistent' | 'mem' };
  commit: { seq: number; timestamp: string; quads: number; ref: string };
  created: string;
  completed: string;
  millis: number;
  /** Sum of the file sizes. */
  logicalBytes: number;
  /** Stored bytes of the blobs this backup uploaded first. */
  addedBytes: number;
  policy: string | null;
  run: string | null;
  note: string | null;
  /** Only under `/$/backups/{ds}`: the backup has the live dataset's id. */
  sameLineage?: boolean;
  /** The last verify of it on this server. */
  verified?: null | { level: VerifyLevel; status: 'ok' | 'warning' | 'error'; at: string };
};

export type BackupFile = {
  path: string;
  kind: 'immutable' | 'append' | 'meta';
  size: number;
  sha256: string;
  blobs: { id: string; size: number }[];
};

/** The manifest view of one backup. */
export type Backup = BackupSummary & {
  format: 1;
  generation: string;
  indexFormat: number;
  parent: string | null;
  server: { version: string };
  files: BackupFile[];
  stats: {
    logicalBytes: number;
    addedBytes: number;
    files: number;
    blobs: number;
    newBlobs: number;
    reusedBlobs: number;
  };
  derived: { text: null | { rebuildOnRestore: true } };
};

/** `restoredFrom` of a dataset made by a restore (in `DatasetInfo`). */
export type RestoredFrom = { repository: string; backup: string; datasetId: string; seq: number };

export type DatasetBackups = {
  dataset: string;
  /** The live dataset's id; null when no dataset of that name is served. */
  datasetId: string | null;
  backups: BackupSummary[];
};

export type Identity = 'auto' | 'new' | 'keep';
export type RestoreCheck = 'quick' | 'full' | 'none';
export type RestoreRequest = {
  target?: string;
  replace?: boolean;
  identity?: Identity;
  check?: RestoreCheck;
  keepReplaced?: boolean;
};

export type VerifyLevel = 'exists' | 'data' | 'restore';
export type VerifyReport = {
  level: VerifyLevel;
  status: 'ok' | 'warning' | 'error';
  backups: {
    name: string;
    status: 'ok' | 'error';
    missing: string[];
    corrupt: string[];
    check?: unknown;
  }[];
  orphans?: { blobs: number; bytes: number };
  requests: { list: number; head: number; get: number };
  millis: number;
};

/** What a verify dialog checks: a whole repository or one backup. */
export type VerifyTarget =
  | { kind: 'repository'; repository: string }
  | { kind: 'backup'; backup: BackupSummary };

export type Retention = { expireAfter: string | null; minCount: number; maxCount: number | null };

export type PolicyRun = {
  id: string;
  policy: string;
  trigger: 'schedule' | 'catch-up' | 'manual';
  scheduledFor: string | null;
  started: string;
  finished: string | null;
  result: 'ok' | 'partial' | 'failed' | 'skipped';
  /** Why a scheduled run was skipped. */
  reason?: string;
  datasets: {
    dataset: string;
    backup: string | null;
    result: 'ok' | 'failed' | 'skipped';
    reason?: string;
    addedBytes?: number;
    millis?: number;
  }[];
  retention: { deleted: string[]; error?: string } | null;
  gc: { task: string } | null;
};

export type PolicyState = {
  nextRun: string | null;
  lastScheduledFor: string | null;
  lastRun: PolicyRun | null;
  lastSuccess: string | null;
  consecutiveFailures: number;
  runningTask: string | null;
};

/** What `POST` and `PUT /$/backup-policies…` take. */
export type PolicyInput = {
  name: string;
  repository: string;
  /** Dataset names or `*` globs. */
  datasets: string[];
  /** Cron (5 or 6 fields) or `every <duration>`. */
  schedule: string;
  timezone: string;
  nameTemplate: string;
  retention: Retention;
  skipUnchanged: boolean;
  gcAfterRetention: boolean;
  catchUp: 'one' | 'none';
  enabled: boolean;
};

export type Policy = PolicyInput & { source: 'api' | 'config'; state: PolicyState };

export type SchedulePreview = {
  next: string[];
  description: string;
  /** The name template rendered for the first run, when one was sent. */
  sample?: string;
};

export type RetentionResult = {
  dryRun: boolean;
  delete: BackupSummary[];
  keep: BackupSummary[];
  errors?: string[];
};

export type LockInfo = {
  id: string;
  kind: 'shared' | 'exclusive';
  operation: 'create' | 'restore' | 'verify' | 'delete' | 'gc';
  holder: { host: string; pid: number; server: string; version: string };
  created: string;
  lastModified: string;
  stale: boolean;
};

export const BACKUP_TASK_KINDS = [
  'backup-create',
  'backup-restore',
  'backup-verify',
  'backup-gc',
  'backup-policy',
] as const;

export const isBackupTask = (t: Task) => (BACKUP_TASK_KINDS as readonly string[]).includes(t.kind);

/** Still waiting or working. */
export const isActive = (t: Task) => t.state === 'queued' || t.state === 'running';

/** A full repository (server-admin view), as opposed to the brief one. */
export const isFull = (r: Repository | RepositoryBrief): r is Repository => 'status' in r;

export const reachable = (r: Repository | RepositoryBrief) =>
  isFull(r) ? r.status.reachable : r.reachable;

export const isReadonly = (r: Repository | RepositoryBrief) => r.readonly === true;

/** Whether a backup was taken from an in-memory dataset. */
export const fromMemory = (s: Pick<BackupSummary, 'dataset'>) => s.dataset.type === 'mem';

// --- repositories ------------------------------------------------------------------

export async function listRepositories(): Promise<(Repository | RepositoryBrief)[]> {
  const body = await json<{ repositories: (Repository | RepositoryBrief)[] }>('/$/repositories');
  return body?.repositories ?? [];
}

export const getRepository = (repo: string) => json<Repository>(`/$/repositories/${enc(repo)}`);

/** Registers (initializing or attaching the location) and tests the connection. */
export const addRepository = (config: RepositoryConfig, verify = true) =>
  json<Repository>(`/$/repositories${verify ? '' : '?verify=false'}`, send('POST', config));

/** Replaces the settings; the location cannot change. */
export const updateRepository = (repo: string, config: RepositoryConfig) =>
  json<Repository>(`/$/repositories/${enc(repo)}`, send('PUT', config));

/** Unregisters; the repository's contents stay. */
export const removeRepository = (repo: string) =>
  json<unknown>(`/$/repositories/${enc(repo)}`, send('DELETE'));

export const testRepository = (repo: string) =>
  json<TestReport>(`/$/repositories/${enc(repo)}/test`, send('POST'));

export const verifyRepository = (repo: string, level: 'exists' | 'data') =>
  json<Task>(`/$/repositories/${enc(repo)}/verify`, send('POST', { level }));

export type BackupFilter = {
  dataset?: string;
  datasetId?: string;
  policy?: string;
  limit?: number;
  /** The `next` cursor of the previous page. */
  before?: string;
};

export function repositoryBackups(repo: string, f: BackupFilter = {}) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(f)) if (v != null && v !== '') q.set(k, String(v));
  const qs = q.size ? `?${q}` : '';
  return json<{ backups: BackupSummary[]; next: string | null }>(
    `/$/repositories/${enc(repo)}/backups${qs}`,
  );
}

/** Every backup of a repository, following `next` for up to `maxPages` pages. */
export async function allRepositoryBackups(
  repo: string,
  f: Omit<BackupFilter, 'before'> = {},
  maxPages = 20,
): Promise<BackupSummary[]> {
  const out: BackupSummary[] = [];
  let before: string | undefined;
  for (let i = 0; i < maxPages; i++) {
    const page = await repositoryBackups(repo, { ...f, before });
    out.push(...(page?.backups ?? []));
    if (!page?.next) break;
    before = page.next;
  }
  return out;
}

export const runGc = (repo: string, opts: { dryRun?: boolean; graceHours?: number } = {}) =>
  json<Task>(`/$/repositories/${enc(repo)}/gc`, send('POST', opts));

export async function listLocks(repo: string): Promise<LockInfo[]> {
  const body = await json<{ locks: LockInfo[] }>(`/$/repositories/${enc(repo)}/locks`);
  return body?.locks ?? [];
}

export const breakLock = (repo: string, id: string) =>
  json<unknown>(`/$/repositories/${enc(repo)}/locks/${enc(id)}`, send('DELETE'));

// --- backups of a dataset -------------------------------------------------------------

export const datasetBackups = (ds: string, repository?: string) =>
  json<DatasetBackups>(
    `/$/backups/${enc(ds)}${repository ? `?repository=${enc(repository)}` : ''}`,
  );

export const createBackup = (
  ds: string,
  body: { repository: string; name?: string; note?: string },
) => json<Task>(`/$/backups/${enc(ds)}`, send('POST', body));

const backupPath = (ds: string, repo: string, backup: string) =>
  `/$/backups/${enc(ds)}/${enc(repo)}/${enc(backup)}`;

/** `{ds}` is the backup's dataset name (the server checks that the backup belongs to it). */
export const getBackup = (ds: string, repo: string, backup: string) =>
  json<Backup>(backupPath(ds, repo, backup));

export const deleteBackup = (ds: string, repo: string, backup: string) =>
  json<unknown>(backupPath(ds, repo, backup), send('DELETE'));

export const restoreBackup = (ds: string, repo: string, backup: string, req: RestoreRequest) =>
  json<Task>(`${backupPath(ds, repo, backup)}/restore`, send('POST', req));

export const verifyBackup = (ds: string, repo: string, backup: string, level: VerifyLevel) =>
  json<Task>(`${backupPath(ds, repo, backup)}/verify`, send('POST', { level }));

/** The same backup call for a summary. */
export const pathOf = (b: BackupSummary) => [b.dataset.name, b.repository, b.name] as const;

// --- policies --------------------------------------------------------------------------

export async function listPolicies(): Promise<Policy[]> {
  const body = await json<{ policies: Policy[] }>('/$/backup-policies');
  return body?.policies ?? [];
}

export const getPolicy = (name: string) => json<Policy>(`/$/backup-policies/${enc(name)}`);

export const createPolicy = (p: PolicyInput) => json<Policy>('/$/backup-policies', send('POST', p));

export const updatePolicy = (name: string, p: PolicyInput) =>
  json<Policy>(`/$/backup-policies/${enc(name)}`, send('PUT', p));

export const deletePolicy = (name: string) =>
  json<unknown>(`/$/backup-policies/${enc(name)}`, send('DELETE'));

/** Runs now; the schedule does not move. */
export const runPolicy = (name: string) =>
  json<Task>(`/$/backup-policies/${enc(name)}/run`, send('POST'));

export const applyRetention = (name: string, dryRun: boolean) =>
  json<RetentionResult>(
    `/$/backup-policies/${enc(name)}/retention${dryRun ? '?dryRun=true' : ''}`,
    send('POST'),
  );

export async function policyRuns(name: string, limit = 50): Promise<PolicyRun[]> {
  const body = await json<{ runs: PolicyRun[] }>(
    `/$/backup-policies/${enc(name)}/runs?limit=${limit}`,
  );
  return body?.runs ?? [];
}

export type PreviewRequest = {
  schedule: string;
  timezone: string;
  count?: number;
  nameTemplate?: string;
  dataset?: string;
};

/** The next runs of a schedule and a description, so the UI needs no cron library. */
export const previewSchedule = (req: PreviewRequest, signal?: AbortSignal) =>
  json<SchedulePreview>('/$/backup-policies/preview', { ...send('POST', req), signal });

// --- tasks -------------------------------------------------------------------------

/** Asks a task to stop (`202`); `409` when it cannot be cancelled or has finished. */
export async function cancelTask(id: string): Promise<void> {
  await request(`/$/tasks/${enc(id)}`, send('DELETE'));
}

/**
 * Polls a task until it is no longer queued or running, reporting every state seen.
 * Resolves with the final task; rejects when `signal` aborts.
 */
export async function followTask(
  id: string,
  onUpdate: (t: Task) => void,
  opts: { signal?: AbortSignal; intervalMs?: number } = {},
): Promise<Task> {
  const interval = opts.intervalMs ?? 700;
  for (;;) {
    if (opts.signal?.aborted) throw new DOMException('Aborted', 'AbortError');
    const t = await getTask(id);
    onUpdate(t);
    if (!isActive(t)) return t;
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(resolve, interval);
      opts.signal?.addEventListener(
        'abort',
        () => {
          clearTimeout(timer);
          reject(new DOMException('Aborted', 'AbortError'));
        },
        { once: true },
      );
    });
  }
}
