// Mock of backup repositories for UI development (docs/API.md, "Backup repositories"):
// repositories (CRUD, connection test, verify, backup listing, GC, locks), backups per
// dataset (list, create, show, delete, restore, verify), lifecycle policies (CRUD,
// schedule preview, run now, retention, run history) and cancellable tasks
// (`DELETE /$/tasks/{id}`) that queue for two task slots and progress over a few seconds.
//
// Everything lives in memory and is seeded on the first request: four repositories
// (`local` fs, `s3-main` from the config file, `dr-source` read-only with backups of a
// dataset this server does not have, `minio-lab` unreachable), three policies and their
// run history. Blobs are content-addressed ids with sizes, so dedup ratios, added bytes
// and GC candidates behave plausibly.
//
//   MOCK_ROLE=dataset-admin   answer /$/whoami as a signed-in admin of `foaf` only
//                             (reduced repository list, no policies, no GC)

import { createHash, randomUUID } from 'node:crypto';
import ox from 'oxigraph';

const MAX_SLOTS = 2;
const PIECE = 32 * 1024 * 1024;
const HOUR = 3600_000;
const DAY = 24 * HOUR;
const ROLE = process.env.MOCK_ROLE ?? 'server-admin';

const REPO_NAME = /^[a-z0-9][a-z0-9_-]{0,63}$/;
const BACKUP_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;
const DATASET_NAME = /^[A-Za-z0-9_-][A-Za-z0-9_.-]{0,63}$/;
const PERMS = ['spo', 'sop', 'pso', 'pos', 'osp', 'ops', 'gspo'];

const sha = (s) => createHash('sha256').update(s).digest('hex');
const iso = (t) => new Date(t).toISOString();
const pad = (n, w = 2) => String(n).padStart(w, '0');
const mb = (b) => (b / 1e6).toFixed(b < 1e7 ? 1 : 0);

/** `{time}`: `20260930t140311z`. */
function timeToken(t) {
  const d = new Date(t);
  return (
    `${d.getUTCFullYear()}${pad(d.getUTCMonth() + 1)}${pad(d.getUTCDate())}` +
    `t${pad(d.getUTCHours())}${pad(d.getUTCMinutes())}${pad(d.getUTCSeconds())}z`
  );
}

class HttpError extends Error {
  constructor(status, code, message, extra = {}) {
    super(message);
    this.status = status;
    this.code = code;
    this.extra = extra;
  }
}
const err = (status, code, message, extra) => new HttpError(status, code, message, extra);

// ---------------------------------------------------------------------------
// schedules: 5/6-field cron and `every <duration>`, evaluated in an IANA time zone

const UNIT_SECONDS = { s: 1, m: 60, h: 3600, d: 86400, w: 604800 };

function parseDuration(s) {
  const re =
    /(\d+)\s*(s|sec|secs|seconds?|m|min|mins|minutes?|h|hr|hrs|hours?|d|days?|w|weeks?)\s*/y;
  let at = 0;
  let total = 0;
  const src = s.trim().toLowerCase();
  if (!src) return null;
  while (at < src.length) {
    re.lastIndex = at;
    const m = re.exec(src);
    if (!m) return null;
    total += Number(m[1]) * UNIT_SECONDS[m[2][0]];
    at = re.lastIndex;
  }
  return total;
}

const MONTHS = ['jan', 'feb', 'mar', 'apr', 'may', 'jun', 'jul', 'aug', 'sep', 'oct', 'nov', 'dec'];
const DOWS = ['sun', 'mon', 'tue', 'wed', 'thu', 'fri', 'sat'];
const DAY_NAMES = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];

function parseField(src, lo, hi, names, label) {
  const out = new Set();
  const val = (s) => {
    const i = names ? names.indexOf(s.toLowerCase()) : -1;
    const n = i >= 0 ? i + (label === 'month' ? 1 : 0) : Number(s);
    if (!Number.isInteger(n) || n < lo || n > hi)
      throw new Error(`${label} value “${s}” is out of range ${lo}–${hi}`);
    return n;
  };
  for (const part of src.split(',')) {
    const [range, stepS] = part.split('/');
    const step = stepS === undefined ? 1 : Number(stepS);
    if (!Number.isInteger(step) || step < 1) throw new Error(`bad step in ${label} “${part}”`);
    let a = lo;
    let b = hi;
    if (range !== '*' && range !== '?') {
      const [x, y] = range.split('-');
      a = val(x);
      b = y === undefined ? (stepS === undefined ? a : hi) : val(y);
    }
    for (let v = a; v <= b; v += step) out.add(v);
  }
  return out;
}

function parseSchedule(schedule) {
  const s = String(schedule ?? '').trim();
  const every = /^every\s+(.+)$/i.exec(s);
  if (every) {
    const secs = parseDuration(every[1]);
    if (secs == null) throw new Error(`cannot read the duration “${every[1]}”`);
    if (secs < 60) throw new Error('an interval must be at least 1 minute');
    return { kind: 'every', secs, src: s };
  }
  const f = s.split(/\s+/);
  if (f.length !== 5 && f.length !== 6)
    throw new Error('a cron schedule has 5 fields (or 6 with seconds), or use “every <duration>”');
  const [sec, ...rest] = f.length === 6 ? f : ['0', ...f];
  const [min, hour, dom, mon, dow] = rest;
  const dows = parseField(dow, 0, 7, DOWS, 'weekday');
  if (dows.has(7)) dows.add(0);
  return {
    kind: 'cron',
    src: s,
    fields: f,
    sec: Math.min(...parseField(sec, 0, 59, null, 'second')),
    min: parseField(min, 0, 59, null, 'minute'),
    hour: parseField(hour, 0, 23, null, 'hour'),
    dom: parseField(dom, 1, 31, null, 'day'),
    mon: parseField(mon, 1, 12, MONTHS, 'month'),
    dow: dows,
    domAny: dom === '*' || dom === '?',
    dowAny: dow === '*' || dow === '?',
  };
}

function zoned(timeZone) {
  const f = new Intl.DateTimeFormat('en-US', {
    timeZone,
    hourCycle: 'h23',
    weekday: 'short',
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: 'numeric',
    minute: 'numeric',
  });
  return (t) => {
    const p = {};
    for (const x of f.formatToParts(t)) p[x.type] = x.value;
    return {
      mon: +p.month,
      dom: +p.day,
      hour: +p.hour % 24,
      min: +p.minute,
      dow: DOWS.indexOf(p.weekday.toLowerCase()),
      key: `${p.year}-${p.month}-${p.day} ${p.hour}:${p.minute}`,
    };
  };
}

function checkZone(tz) {
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: tz });
  } catch {
    throw err(400, 'invalid-schedule', `unknown time zone “${tz}”`);
  }
}

/** The next `n` runs strictly after `after` (ms). */
function nextRuns(sched, tz, after, n) {
  const out = [];
  if (sched.kind === 'every') {
    const step = sched.secs * 1000;
    let t = Math.floor(after / step) * step + step;
    while (out.length < n) {
      out.push(t);
      t += step;
    }
    return out;
  }
  const at = zoned(tz);
  let t = Math.floor(after / 60_000) * 60_000;
  if (t + sched.sec * 1000 <= after) t += 60_000;
  let lastKey = '';
  const limit = after + 800 * DAY;
  while (out.length < n && t < limit) {
    const p = at(t);
    const dayOk =
      sched.mon.has(p.mon) &&
      (sched.domAny && sched.dowAny
        ? true
        : sched.domAny
          ? sched.dow.has(p.dow)
          : sched.dowAny
            ? sched.dom.has(p.dom)
            : sched.dom.has(p.dom) || sched.dow.has(p.dow));
    if (!dayOk || !sched.hour.has(p.hour)) {
      t += (60 - p.min) * 60_000; // on to the next hour
      continue;
    }
    // an ambiguous local time (clocks going back) runs once
    if (sched.min.has(p.min) && p.key !== lastKey) {
      out.push(t + sched.sec * 1000);
      lastKey = p.key;
    }
    t += 60_000;
  }
  return out;
}

function describe(sched, tz) {
  if (sched.kind === 'every') {
    const s = sched.secs;
    const [n, unit] =
      s % 86400 === 0
        ? [s / 86400, 'day']
        : s % 3600 === 0
          ? [s / 3600, 'hour']
          : [Math.round(s / 60), 'minute'];
    return `Every ${n === 1 ? '' : `${n} `}${unit}${n === 1 ? '' : 's'}, counted from 00:00 UTC`;
  }
  const [min, hour, dom, mon, dow] =
    sched.fields.length === 6 ? sched.fields.slice(1) : sched.fields;
  const num = (x) => /^\d+$/.test(x);
  const hm = () => `${pad(hour)}:${pad(min)}`;
  const zone = ` (${tz})`;
  if (dom === '*' && mon === '*') {
    if (num(min) && hour === '*' && dow === '*') return `Every hour at minute ${Number(min)}`;
    const everyMin = /^\*\/(\d+)$/.exec(min);
    if (everyMin && hour === '*' && dow === '*') return `Every ${everyMin[1]} minutes`;
    if (num(min) && num(hour)) {
      if (dow === '*') return `Every day at ${hm()}${zone}`;
      if (num(dow)) return `Every ${DAY_NAMES[Number(dow) % 7]} at ${hm()}${zone}`;
      if (dow === '1-5') return `At ${hm()} on weekdays${zone}`;
    }
  }
  return `Cron “${sched.src}”${zone}`;
}

// ---------------------------------------------------------------------------
// retention (per dataset id, newest first by completion)

function retentionPlan(list, policy, now, busy = new Set()) {
  const expire = policy.retention.expireAfter ? parseDuration(policy.retention.expireAfter) : null;
  const byId = new Map();
  for (const b of list.filter((x) => x.policy === policy.name)) {
    if (!byId.has(b.dataset.id)) byId.set(b.dataset.id, []);
    byId.get(b.dataset.id).push(b);
  }
  const del = [];
  const keep = [];
  for (const group of byId.values()) {
    group.sort((a, b) => b.completed.localeCompare(a.completed));
    group.forEach((b, i) => {
      const old = expire != null && Date.parse(b.completed) + expire * 1000 < now;
      const beyond = policy.retention.maxCount != null && i >= policy.retention.maxCount;
      if (i >= policy.retention.minCount && (old || beyond) && !busy.has(b.name)) del.push(b);
      else keep.push(b);
    });
  }
  return { delete: del, keep };
}

// ---------------------------------------------------------------------------
// the mock

let mock = null;

/**
 * Answers backup-repository routes (and `DELETE /$/tasks/{id}`); returns false for
 * everything else. `ctx` hands over the main mock's datasets, tasks and helpers.
 */
export async function handleBackups(req, res, url, seg, ctx) {
  mock ??= createMock(ctx);
  return mock(req, res, url, seg);
}

function createMock({
  datasets,
  tasks,
  nextTaskId,
  makeDataset,
  addCommit,
  headCommit,
  send,
  readBody,
}) {
  const now = Date.now();
  const repos = new Map();
  const policies = new Map();
  /** Policy runs, newest first. */
  const runs = [];
  /** Internal state of the tasks this module runs, by id. */
  const jobs = new Map();

  // --- permissions (MOCK_ROLE) --------------------------------------------------------

  const roleDatasets = { foaf: 'admin', scratch: 'read' };
  const serverAdmin = () => ROLE !== 'dataset-admin';
  const level = (ds) => (serverAdmin() ? 'admin' : (roleDatasets[ds] ?? null));
  const needServerAdmin = () => {
    if (!serverAdmin()) throw err(403, 'forbidden', 'requires the server-admin permission');
  };
  const needDataset = (ds, need) => {
    const l = level(ds);
    if (!l) throw err(404, 'no-such-dataset', `No such dataset: ${ds}`);
    if (need === 'admin' && l !== 'admin') throw err(403, 'forbidden', `requires admin on ${ds}`);
  };

  // --- blobs and manifests --------------------------------------------------------------

  function pieces(key, size) {
    const out = [];
    for (let i = 0, rest = size; rest > 0 || i === 0; i++, rest -= PIECE)
      out.push({ id: sha(`${key}#${i}`), size: Math.max(0, Math.min(PIECE, rest)) });
    return out;
  }

  /** The files of a dataset at a commit, reusing the parent's blobs where they are the same. */
  function planFiles(src, parent) {
    const files = [];
    const genKey = `${src.id}/${src.generation}/${src.genQuads}`;
    const n = src.genQuads * (src.scale ?? 1);
    const immutable = [
      ['vocab.dat', n * 38],
      ['vocab.off', n * 8],
      ...PERMS.flatMap((p) => [
        [`${p}.dat`, n * 11],
        [`${p}.meta`, 64 + Math.ceil(n / 4096) * 48],
      ]),
      ['meta.json', 612],
      ['stats.json', 1800],
      ['commit.json', 310],
    ];
    for (const [name, size] of immutable)
      files.push({
        path: `${src.generation}/${name}`,
        kind: 'immutable',
        size,
        blobs: pieces(`${genKey}/${name}`, size),
      });
    const since = Math.max(0, src.seq - src.genBase) * (src.scale ?? 1);
    const append = [
      [`${src.generation}/wal.log`, 64 + since * 480],
      [`${src.generation}/delta.vocab`, since * 96],
      ['commits.bin', 64 + (src.seq + 1) * 88],
    ];
    for (const [path, size] of append) {
      const prev = parent?.files.find((f) => f.path === path);
      const blobs =
        prev && prev.size <= size
          ? [
              ...prev.blobs,
              ...(size > prev.size
                ? pieces(`${src.id}/${path}@${prev.size}-${size}`, size - prev.size)
                : []),
            ]
          : pieces(`${src.id}/${path}@0-${size}`, size);
      files.push({ path, kind: 'append', size, blobs });
    }
    const meta = [
      ['CURRENT', 9, src.generation],
      ['dataset.json', 380, src.id],
      ['prefixes.json', 720, src.id],
      ...(src.text ? [['text.json', 140, src.id]] : []),
    ];
    for (const [path, size, version] of meta)
      files.push({ path, kind: 'meta', size, blobs: pieces(`${version}/${path}`, size) });
    for (const f of files) f.sha256 = sha(f.blobs.map((b) => b.id).join());
    return files;
  }

  /** Stores a backup: new blobs count as added, the manifest becomes visible. */
  function storeBackup(repo, src, opts) {
    const parent = [...repo.backups.values()]
      .filter((b) => b.dataset.id === src.id)
      .sort((a, b) => b.completed.localeCompare(a.completed))[0];
    const files = planFiles(src, parent);
    let added = 0;
    let newBlobs = 0;
    let reused = 0;
    for (const f of files)
      for (const b of f.blobs) {
        if (repo.blobs.has(b.id)) reused++;
        else {
          repo.blobs.set(b.id, { size: b.size, at: opts.completed });
          added += b.size;
          newBlobs++;
        }
      }
    const logical = files.reduce((a, f) => a + f.size, 0);
    const b = {
      name: opts.name,
      repository: repo.config.name,
      dataset: { name: src.name, id: src.id },
      commit: {
        seq: src.seq,
        timestamp: src.timestamp,
        quads: src.quads,
        ref: `commit:${src.seq}`,
      },
      created: iso(opts.created),
      completed: iso(opts.completed),
      millis: Math.round(opts.completed - opts.created),
      logicalBytes: logical,
      addedBytes: added,
      policy: opts.policy ?? null,
      run: opts.run ?? null,
      note: opts.note ?? null,
      verified: opts.verified ?? null,
      // manifest-only fields
      format: 1,
      generation: src.generation,
      indexFormat: 2,
      parent: parent?.name ?? null,
      server: { version: '0.1.0-mock' },
      files,
      stats: {
        logicalBytes: logical,
        addedBytes: added,
        files: files.length,
        blobs: files.reduce((a, f) => a + f.blobs.length, 0),
        newBlobs,
        reusedBlobs: reused,
      },
      derived: { text: src.text ? { rebuildOnRestore: true } : null },
      // mock-only: the data to restore, and seeded damage
      quads: src.data ?? null,
      corrupt: opts.corrupt ?? false,
    };
    repo.backups.set(b.name, b);
    return { backup: b, newBlobs, reused };
  }

  const summary = (b, live) => {
    const out = {
      name: b.name,
      repository: b.repository,
      dataset: b.dataset,
      commit: b.commit,
      created: b.created,
      completed: b.completed,
      millis: b.millis,
      logicalBytes: b.logicalBytes,
      addedBytes: b.addedBytes,
      policy: b.policy,
      run: b.run,
      note: b.note,
      verified: b.verified,
    };
    if (live !== undefined) out.sameLineage = !!live && live.id === b.dataset.id;
    return out;
  };

  const manifest = (b) => {
    const { quads, corrupt, ...rest } = b;
    void quads;
    void corrupt;
    return rest;
  };

  const referenced = (repo) => {
    const ids = new Set();
    for (const b of repo.backups.values())
      for (const f of b.files) for (const x of f.blobs) ids.add(x.id);
    return ids;
  };

  function stats(repo) {
    if (!repo.status.reachable && !repo.id) return null;
    let stored = 0;
    for (const b of repo.blobs.values()) stored += b.size;
    const list = [...repo.backups.values()];
    const logical = list.reduce((a, b) => a + b.logicalBytes, 0);
    return {
      backups: list.length,
      datasets: new Set(list.map((b) => b.dataset.id)).size,
      storedBytes: stored,
      logicalBytes: logical,
      dedupRatio: stored ? Math.round((logical / stored) * 10) / 10 : 0,
      asOf: iso(Date.now()),
    };
  }

  function repoJson(repo) {
    return {
      ...repo.config,
      source: repo.source,
      id: repo.id,
      status: { ...repo.status },
      stats: stats(repo),
      lastGc: repo.lastGc,
      policies: [...policies.values()]
        .filter((p) => p.repository === repo.config.name)
        .map((p) => p.name),
    };
  }

  const brief = (repo) => ({
    name: repo.config.name,
    type: repo.config.type,
    readonly: !!repo.config.readonly,
    reachable: repo.status.reachable,
  });

  // --- seed data ----------------------------------------------------------------------

  /** `[credentials.<name>]` of the (imaginary) backup config file. */
  const CREDENTIAL_SOURCES = ['minio', 's3-main'];

  function addRepo(config, source, status, extra = {}) {
    const repo = {
      config: {
        conditionalWrites: true,
        readonly: false,
        maxConcurrency: config.type === 'fs' ? 4 : 8,
        maxUploadBytesPerSec: null,
        maxDownloadBytesPerSec: null,
        credentials: config.type === 'fs' ? undefined : { source: 'default' },
        ...config,
      },
      source,
      id: status.reachable || extra.id ? (extra.id ?? randomUUID()) : null,
      status: {
        checked: iso(Date.now()),
        singleWriter: config.conditionalWrites === false,
        ...status,
      },
      backups: new Map(),
      blobs: new Map(),
      locks: [],
      lastGc: null,
    };
    repos.set(config.name, repo);
    return repo;
  }

  /**
   * The dataset as a backup source at its head, or as it was at an earlier time (seeds).
   * File sizes are scaled up so that the small demo datasets look like real ones.
   */
  function sourceOf(ds, at) {
    const head = headCommit(ds);
    const seq = at == null ? head.seq : Math.max(2, head.seq - Math.ceil((now - at) / (8 * HOUR)));
    const c = ds.commits.find((x) => x.seq === seq) ?? head;
    return {
      name: ds.name,
      id: ds.id,
      seq: c.seq,
      timestamp: at == null ? c.timestamp : iso(Math.min(Date.parse(c.timestamp), at - 7 * 60_000)),
      scale: 1500,
      quads: c.quads,
      generation: 'gen-0001',
      genBase: 1,
      genQuads: ds.baseQuads,
      text: !!ds.text,
      data: ds.store.match(),
    };
  }

  function seed() {
    const foaf = datasets.get('foaf');
    const local = addRepo({ name: 'local', type: 'fs', path: '/srv/backups/sparkles' }, 'api', {
      reachable: true,
      conditionalWrites: true,
    });
    const s3 = addRepo(
      {
        name: 's3-main',
        type: 's3',
        bucket: 'kg-backups',
        prefix: 'prod/sparkles',
        region: 'eu-central-1',
        credentials: { source: 'file', path: '/run/secrets/sparkles-s3.json' },
        sse: 'aws:kms',
        kmsKeyId: 'arn:aws:kms:eu-central-1:111122223333:key/7c1e…',
        maxUploadBytesPerSec: 104857600,
      },
      'config',
      { reachable: true, conditionalWrites: true },
    );
    const dr = addRepo(
      {
        name: 'dr-source',
        type: 's3',
        bucket: 'kg-backups-eu',
        prefix: 'prod',
        region: 'eu-west-1',
        readonly: true,
        credentials: {
          source: 'env',
          accessKeyIdVar: 'DR_AWS_ACCESS_KEY_ID',
          secretAccessKeyVar: 'DR_AWS_SECRET_ACCESS_KEY',
        },
      },
      'config',
      { reachable: true, conditionalWrites: true },
    );
    addRepo(
      {
        name: 'minio-lab',
        type: 's3',
        bucket: 'sparkles',
        endpoint: 'http://127.0.0.1:9000',
        pathStyle: true,
        allowHttp: true,
        conditionalWrites: false,
        credentials: { source: 'named', name: 'minio' },
      },
      'api',
      {
        reachable: false,
        error: 'repository-unavailable: error sending request (connection refused)',
        singleWriter: true,
      },
    );

    if (foaf) {
      // an older lineage of foaf (the dataset was deleted and loaded again since)
      const old = { ...sourceOf(foaf), id: randomUUID(), seq: 118, quads: foaf.store.size - 40 };
      old.timestamp = iso(now - 21 * DAY);
      old.genQuads = old.quads;
      storeBackup(local, old, {
        name: 'foaf-before-reload',
        created: now - 21 * DAY,
        completed: now - 21 * DAY + 4200,
        note: 'before the September reload',
      });
      const manual = [
        [
          5 * DAY,
          'before the schema change',
          { level: 'data', status: 'ok', at: iso(now - 4 * DAY) },
        ],
        [2 * DAY + 3 * HOUR, null, { level: 'data', status: 'error', at: iso(now - DAY) }],
      ];
      for (const [ago, note, verified] of manual)
        storeBackup(local, sourceOf(foaf, now - ago), {
          name: `foaf-${timeToken(now - ago)}`,
          created: now - ago,
          completed: now - ago + 2100,
          note,
          verified,
          corrupt: verified?.status === 'error',
        });
      // foaf-6h: the last four scheduled instants
      const step = 6 * HOUR;
      for (let i = 4; i >= 1; i--) {
        const at = Math.floor(now / step) * step - (i - 1) * step;
        const run = randomUUID();
        const { backup } = storeBackup(local, sourceOf(foaf, at), {
          name: `foaf-6h-foaf-${timeToken(at)}`,
          created: at + 500,
          completed: at + 2600,
          policy: 'foaf-6h',
          run,
        });
        runs.push(
          policyRun('foaf-6h', run, 'schedule', at, [
            {
              dataset: 'foaf',
              backup: backup.name,
              result: 'ok',
              addedBytes: backup.addedBytes,
              millis: backup.millis,
            },
          ]),
        );
      }
      // nightly: five nights at 02:30 Berlin
      for (let i = 5; i >= 1; i--) {
        const at = nextRuns(
          parseSchedule('30 2 * * *'),
          'Europe/Berlin',
          now - (i + 1) * DAY,
          1,
        )[0];
        const run = randomUUID();
        const failed = i === 3;
        const items = [];
        if (failed)
          items.push({
            dataset: 'foaf',
            backup: null,
            result: 'failed',
            reason: 'repository-unavailable: 503 Slow Down (after 10 retries)',
            millis: 181_000,
          });
        else {
          const { backup } = storeBackup(s3, sourceOf(foaf, at), {
            name: `nightly-foaf-${new Date(at).toISOString().slice(0, 10).replace(/-/g, '')}`,
            created: at + 1000,
            completed: at + 5400,
            policy: 'nightly',
            run,
          });
          items.push({
            dataset: 'foaf',
            backup: backup.name,
            result: 'ok',
            addedBytes: backup.addedBytes,
            millis: backup.millis,
          });
        }
        items.push({
          dataset: 'scratch',
          backup: null,
          result: 'skipped',
          reason: 'in-memory dataset',
        });
        const r = policyRun('nightly', run, i === 5 ? 'catch-up' : 'schedule', at, items);
        if (!failed) r.retention = { deleted: i === 1 ? ['nightly-foaf-20260829'] : [] };
        runs.push(r);
      }
    }

    // wiki lives on another server; its backups are here for disaster recovery
    const wikiId = randomUUID();
    const wikiBase = 10_512_334;
    for (let i = 6; i >= 1; i--) {
      const at = nextRuns(parseSchedule('30 2 * * *'), 'Europe/Berlin', now - (i + 1) * DAY, 1)[0];
      const compacted = i <= 2;
      storeBackup(
        dr,
        {
          name: 'wiki',
          id: wikiId,
          seq: 1840 + (6 - i) * 37,
          timestamp: iso(at - 20 * 60_000),
          quads: wikiBase + (6 - i) * 1200,
          generation: compacted ? 'gen-0008' : 'gen-0007',
          genBase: compacted ? 1950 : 1700,
          genQuads: compacted ? wikiBase + 4800 : wikiBase,
          text: true,
          data: null,
        },
        {
          name: `nightly-wiki-${new Date(at).toISOString().slice(0, 10).replace(/-/g, '')}`,
          created: at + 2000,
          completed: at + 2000 + (compacted && i === 2 ? 410_000 : 38_000),
          policy: 'nightly',
          run: randomUUID(),
        },
      );
    }

    // an unreferenced leftover of a deleted backup, older than the grace period
    for (let i = 0; i < 12; i++)
      local.blobs.set(sha(`orphan-${i}`), { size: 3_900_000 + i * 17_000, at: iso(now - 3 * DAY) });
    local.lastGc = {
      dryRun: false,
      manifests: 9,
      referencedBlobs: 131,
      listedBlobs: 140,
      candidates: 9,
      deleted: 9,
      deletedBytes: 38_211_000,
      keptYoung: 0,
      storedBytesAfter: 0,
      requests: { list: 4, get: 2, delete: 1 },
      millis: 1840,
      lockWaitMillis: 12,
      finished: iso(now - 9 * DAY),
    };
    local.lastGc.storedBytesAfter = stats(local).storedBytes;
    // a lock left behind by a server that went away
    s3.locks.push({
      id: randomUUID(),
      kind: 'shared',
      operation: 'create',
      holder: {
        host: 'kg-worker-2',
        pid: 48211,
        server: sha('kg-worker-2').slice(0, 16),
        version: '0.1.0',
      },
      created: iso(now - 2 * DAY),
      lastModified: iso(now - 2 * DAY + 5 * 60_000),
      stale: true,
    });

    addPolicy(
      {
        name: 'nightly',
        repository: 's3-main',
        datasets: ['*'],
        schedule: '30 2 * * *',
        timezone: 'Europe/Berlin',
        nameTemplate: '{policy}-{dataset}-{date:%Y%m%d}',
        retention: { expireAfter: '30d', minCount: 7, maxCount: 60 },
        skipUnchanged: false,
        gcAfterRetention: true,
        catchUp: 'one',
        enabled: true,
      },
      'config',
    );
    addPolicy(
      {
        name: 'foaf-6h',
        repository: 'local',
        datasets: ['foaf'],
        schedule: 'every 6h',
        timezone: 'UTC',
        nameTemplate: '{policy}-{dataset}-{time}',
        retention: { expireAfter: '7d', minCount: 4, maxCount: 28 },
        skipUnchanged: true,
        gcAfterRetention: false,
        catchUp: 'one',
        enabled: true,
      },
      'api',
    );
    addPolicy(
      {
        name: 'weekly-archive',
        repository: 'local',
        datasets: ['foaf', 'wiki-*'],
        schedule: '0 3 * * 0',
        timezone: 'UTC',
        nameTemplate: '{dataset}-week{date:%V}-{date:%Y}',
        retention: { expireAfter: null, minCount: 1, maxCount: 12 },
        skipUnchanged: false,
        gcAfterRetention: false,
        catchUp: 'none',
        enabled: false,
      },
      'api',
    );
    runs.sort((a, b) => b.started.localeCompare(a.started));
    for (const p of policies.values()) refreshState(p);
  }

  function policyRun(policy, id, trigger, at, items) {
    const failed = items.filter((d) => d.result === 'failed').length;
    const ok = items.filter((d) => d.result === 'ok').length;
    return {
      id,
      policy,
      trigger,
      scheduledFor: trigger === 'manual' ? null : iso(at),
      started: iso(at + 400),
      finished: iso(at + 400 + items.reduce((a, d) => a + (d.millis ?? 0), 0) + 300),
      result: failed === 0 ? 'ok' : ok > 0 ? 'partial' : 'failed',
      datasets: items,
      retention: failed === items.length ? null : { deleted: [] },
      gc: null,
    };
  }

  // --- policies -------------------------------------------------------------------------

  function validatePolicy(body, existing) {
    const p = {
      name: String(body.name ?? existing?.name ?? ''),
      repository: String(body.repository ?? ''),
      datasets:
        Array.isArray(body.datasets) && body.datasets.length ? body.datasets.map(String) : ['*'],
      schedule: String(body.schedule ?? ''),
      timezone: String(body.timezone ?? 'UTC'),
      nameTemplate: String(body.nameTemplate ?? '{policy}-{dataset}-{time}'),
      retention: {
        expireAfter: body.retention?.expireAfter || null,
        minCount: Number(body.retention?.minCount ?? 1),
        maxCount:
          body.retention?.maxCount == null || body.retention?.maxCount === ''
            ? null
            : Number(body.retention.maxCount),
      },
      skipUnchanged: !!body.skipUnchanged,
      gcAfterRetention: !!body.gcAfterRetention,
      catchUp: body.catchUp === 'none' ? 'none' : 'one',
      enabled: body.enabled !== false,
    };
    if (!REPO_NAME.test(p.name)) throw err(400, 'invalid-name', `invalid policy name “${p.name}”`);
    if (!repos.has(p.repository))
      throw err(404, 'no-such-repository', `no repository named “${p.repository}”`);
    if (repos.get(p.repository).config.readonly)
      throw err(409, 'repository-read-only', `repository “${p.repository}” is read-only`);
    checkZone(p.timezone);
    try {
      parseSchedule(p.schedule);
    } catch (e) {
      throw err(400, 'invalid-schedule', `invalid schedule: ${e.message}`);
    }
    if (p.retention.expireAfter && parseDuration(p.retention.expireAfter) == null)
      throw err(400, 'invalid-config', `cannot read expireAfter “${p.retention.expireAfter}”`);
    if (!Number.isInteger(p.retention.minCount) || p.retention.minCount < 0)
      throw err(400, 'invalid-config', 'minCount must be a whole number ≥ 0');
    if (
      p.retention.maxCount != null &&
      (!Number.isInteger(p.retention.maxCount) || p.retention.maxCount < 1)
    )
      throw err(400, 'invalid-config', 'maxCount must be a whole number ≥ 1, or null');
    const sample = renderName(p, 'dataset', 42, randomUUID(), Date.now());
    if (!BACKUP_NAME.test(sample))
      throw err(
        400,
        'invalid-config',
        `the name template renders “${sample}”, which is not a valid backup name`,
      );
    return p;
  }

  function renderName(p, dataset, seq, run, at) {
    const parts = {};
    for (const x of new Intl.DateTimeFormat('en-US', {
      timeZone: p.timezone,
      hourCycle: 'h23',
      year: 'numeric',
      month: '2-digit',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
    }).formatToParts(at))
      parts[x.type] = x.value;
    const doy =
      Math.round(
        (Date.UTC(+parts.year, +parts.month - 1, +parts.day) - Date.UTC(+parts.year, 0, 1)) / DAY,
      ) + 1;
    const week = (() => {
      const t = new Date(Date.UTC(+parts.year, +parts.month - 1, +parts.day));
      t.setUTCDate(t.getUTCDate() - ((t.getUTCDay() + 6) % 7) + 3);
      const first = new Date(Date.UTC(t.getUTCFullYear(), 0, 4));
      return 1 + Math.round(((t - first) / DAY - 3 + ((first.getUTCDay() + 6) % 7)) / 7);
    })();
    const fmt = {
      Y: parts.year,
      m: parts.month,
      d: parts.day,
      H: pad(+parts.hour % 24),
      M: parts.minute,
      S: parts.second,
      j: pad(doy, 3),
      V: pad(week),
    };
    return p.nameTemplate.replace(/\{([^}]*)\}/g, (_, t) => {
      if (t === 'policy') return p.name;
      if (t === 'dataset') return dataset;
      if (t === 'seq') return String(seq);
      if (t === 'run') return run.replace(/-/g, '').slice(0, 8);
      if (t === 'time') return timeToken(at);
      if (t.startsWith('date:')) return t.slice(5).replace(/%(.)/g, (m, c) => fmt[c] ?? m);
      throw err(400, 'invalid-config', `unknown placeholder {${t}}`);
    });
  }

  function addPolicy(p, source) {
    const policy = { ...p, source, state: null };
    policies.set(p.name, policy);
    refreshState(policy);
    return policy;
  }

  function refreshState(p) {
    const mine = runs.filter((r) => r.policy === p.name);
    const lastRun = mine[0] ?? null;
    let failures = 0;
    for (const r of mine) {
      if (r.result === 'failed' || r.result === 'partial') failures++;
      else if (r.result === 'ok') break;
    }
    const running = p.state?.runningTask;
    p.state = {
      nextRun: p.enabled
        ? iso(nextRuns(parseSchedule(p.schedule), p.timezone, Date.now(), 1)[0])
        : null,
      lastScheduledFor: mine.find((r) => r.scheduledFor)?.scheduledFor ?? null,
      lastRun,
      lastSuccess: mine.find((r) => r.result === 'ok')?.finished ?? null,
      consecutiveFailures: failures,
      runningTask:
        running && jobs.get(running) && isActive(jobs.get(running).task) ? running : null,
    };
  }

  const policyJson = (p) => {
    refreshState(p);
    return structuredClone(p);
  };

  const allBackups = () => [...repos.values()].flatMap((r) => [...r.backups.values()]);
  const busyBackups = () =>
    new Set([...jobs.values()].filter((j) => isActive(j.task) && j.backup).map((j) => j.backup));

  function applyRetention(p, dryRun) {
    const repo = repos.get(p.repository);
    const plan = retentionPlan([...repo.backups.values()], p, Date.now(), busyBackups());
    if (!dryRun) for (const b of plan.delete) deleteBackup(repo, b);
    return {
      dryRun,
      delete: plan.delete.map((b) => summary(b)),
      keep: plan.keep.map((b) => summary(b)),
    };
  }

  function deleteBackup(repo, b) {
    repo.backups.delete(b.name);
    // its blobs stay until GC; they are young, so the grace period keeps them for a day
    const refs = referenced(repo);
    for (const f of b.files)
      for (const x of f.blobs)
        if (!refs.has(x.id) && repo.blobs.has(x.id)) repo.blobs.get(x.id).at = iso(Date.now());
  }

  // --- tasks: two slots, progress, cancellation -------------------------------------------

  const isActive = (t) => t.state === 'queued' || t.state === 'running';

  /**
   * Starts a task. `steps(i, n)` returns the message at step `i` of `n`; `finish()` its
   * message and detail. Tasks past `slots` wait as `queued`.
   */
  function startJob({
    kind,
    dataset = '',
    target,
    backup,
    repo,
    ms = 4000,
    steps,
    finish,
    cancellable = true,
    lock,
  }) {
    const task = {
      id: nextTaskId(),
      kind,
      dataset,
      ...(target ? { target } : {}),
      state: 'queued',
      startedAt: iso(Date.now()),
      message: 'waiting for a free backup task slot',
      progress: 0,
      cancellable,
    };
    tasks.unshift(task);
    jobs.set(task.id, { task, backup, repo, ms, steps, finish, lock, timer: null, i: 0, n: 16 });
    pump();
    return task;
  }

  function pump() {
    const running = [...jobs.values()].filter((j) => j.task.state === 'running').length;
    const waiting = [...jobs.values()].filter((j) => j.task.state === 'queued').reverse();
    for (const job of waiting.slice(0, Math.max(0, MAX_SLOTS - running))) begin(job);
  }

  function begin(job) {
    const { task } = job;
    task.state = 'running';
    task.startedAt = iso(Date.now());
    task.message = 'starting';
    if (job.lock && job.repo && !job.repo.config.readonly) {
      job.lockId = randomUUID();
      job.repo.locks.push({
        id: job.lockId,
        kind: job.lock === 'gc' ? 'exclusive' : 'shared',
        operation: job.lock,
        holder: {
          host: 'localhost',
          pid: process.pid,
          server: sha('mock').slice(0, 16),
          version: '0.1.0-mock',
        },
        created: iso(Date.now()),
        lastModified: iso(Date.now()),
        stale: false,
      });
    }
    const tick = () => {
      job.i++;
      task.progress = Math.min(0.99, job.i / job.n);
      const m = job.steps?.(job.i, job.n, task);
      if (m) task.message = m;
      if (job.i < job.n) {
        job.timer = setTimeout(tick, job.ms / job.n);
        return;
      }
      try {
        const r = job.finish() ?? {};
        task.state = 'done';
        task.message = r.message ?? 'done';
        if (r.detail !== undefined) task.detail = r.detail;
      } catch (e) {
        task.state = 'failed';
        task.message = e instanceof HttpError ? `${e.code}: ${e.message}` : String(e?.message ?? e);
      }
      end(job);
    };
    job.timer = setTimeout(tick, job.ms / job.n);
  }

  function end(job) {
    clearTimeout(job.timer);
    job.task.finishedAt = iso(Date.now());
    delete job.task.progress;
    job.task.cancellable = false;
    if (job.lockId && job.repo) job.repo.locks = job.repo.locks.filter((l) => l.id !== job.lockId);
    job.onEnd?.();
    pump();
  }

  function cancel(id) {
    const job = jobs.get(id);
    const t = tasks.find((x) => x.id === id);
    if (!t) throw err(404, 'no-such-task', `No such task: ${id}`);
    if (!job || !isActive(t) || !t.cancellable)
      throw err(
        409,
        'not-cancellable',
        isActive(t) ? 'this task cannot be cancelled' : `task ${id} has finished`,
      );
    if (t.dataset ? level(t.dataset) !== 'admin' : !serverAdmin())
      throw err(403, 'forbidden', 'cancelling needs admin on the task’s dataset');
    t.state = 'cancelled';
    t.message = `cancelled at ${Math.round((t.progress ?? 0) * 100)}%${job.backup ? '; nothing was published' : ''}`;
    job.onCancel?.();
    end(job);
    return t;
  }

  // --- operations ---------------------------------------------------------------------------

  function getRepo(name, { write = false } = {}) {
    const repo = repos.get(name);
    if (!repo) throw err(404, 'no-such-repository', `no repository named “${name}”`);
    if (write && repo.config.readonly)
      throw err(409, 'repository-read-only', `repository “${name}” is read-only`);
    if (!repo.status.reachable)
      throw err(
        502,
        'repository-unavailable',
        repo.status.error ?? `repository “${name}” is unreachable`,
      );
    return repo;
  }

  /** The backup `{ds}/{repo}/{name}`: 404 unless it belongs to `ds` by name or live id. */
  function findBackup(ds, repoName, name) {
    const repo = getRepo(repoName);
    const b = repo.backups.get(name);
    const live = datasets.get(ds);
    if (!b || (b.dataset.name !== ds && b.dataset.id !== live?.id))
      throw err(404, 'no-such-backup', `no backup “${name}” of ${ds} in ${repoName}`);
    return { repo, b };
  }

  function createBackupJob(ds, repo, name, opts = {}) {
    const running = [...jobs.values()].find(
      (j) =>
        j.task.kind === 'backup-create' &&
        j.task.dataset === ds.name &&
        j.repo === repo &&
        isActive(j.task),
    );
    if (running)
      throw err(
        409,
        'backup-in-progress',
        `a backup of ${ds.name} into ${repo.config.name} is running`,
        {
          task: running.task.id,
        },
      );
    if (repo.backups.has(name))
      throw err(409, 'backup-exists', `backup “${name}” exists in ${repo.config.name}`);
    const src = sourceOf(ds);
    const created = Date.now();
    // an estimate of the upload from the blobs the repository lacks
    const parent = [...repo.backups.values()]
      .filter((b) => b.dataset.id === src.id)
      .sort((a, b) => b.completed.localeCompare(a.completed))[0];
    const plan = planFiles(src, parent);
    const blobs = plan.flatMap((f) => f.blobs);
    const fresh = blobs.filter((b) => !repo.blobs.has(b.id));
    const total = fresh.reduce((a, b) => a + b.size, 0) || 1;
    const task = startJob({
      kind: 'backup-create',
      dataset: ds.name,
      target: name,
      backup: name,
      repo,
      lock: 'create',
      ms: opts.ms ?? 5000,
      steps: (i, n) => {
        if (i <= 1) return `capturing commit ${src.seq}`;
        if (i === 2)
          return `planning: ${fresh.length} new blobs, ${blobs.length - fresh.length} reused`;
        if (i >= n - 1) return 'writing the manifest';
        const done = Math.round(((i - 2) / (n - 4)) * total);
        const k = Math.round(((i - 2) / (n - 4)) * fresh.length);
        return `uploading ${mb(done)}/${mb(total)} MB · ${k} new blobs · ${blobs.length - fresh.length} reused`;
      },
      finish: () => {
        const { backup } = storeBackup(repo, src, {
          name,
          created,
          completed: Date.now(),
          note: opts.note,
          policy: opts.policy,
          run: opts.run,
        });
        opts.onDone?.(backup);
        return {
          message: `backed up ${ds.name} at commit ${src.seq} into ${repo.config.name}/${name}`,
          detail: summary(backup),
        };
      },
    });
    return task;
  }

  function verifyReport(level, list, repo, withOrphans) {
    const out = {
      level,
      status: 'ok',
      backups: list.map((b) => ({
        name: b.name,
        status: b.corrupt && level !== 'exists' ? 'error' : 'ok',
        missing: [],
        corrupt: b.corrupt && level !== 'exists' ? [b.files[3].blobs[0].id] : [],
        ...(level === 'restore'
          ? { check: { status: b.corrupt ? 'error' : 'ok', checks: 14 } }
          : {}),
      })),
      requests: { list: 2, head: 0, get: level === 'exists' ? list.length : list.length * 20 },
      millis: 900 + list.length * (level === 'exists' ? 20 : 400),
    };
    if (out.backups.some((b) => b.status === 'error')) out.status = 'error';
    if (withOrphans) {
      const refs = referenced(repo);
      const orphans = [...repo.blobs].filter(([id]) => !refs.has(id));
      out.orphans = { blobs: orphans.length, bytes: orphans.reduce((a, [, x]) => a + x.size, 0) };
      if (out.status === 'ok' && orphans.length) out.status = 'warning';
    }
    const at = iso(Date.now());
    for (const b of list)
      b.verified = { level, status: out.backups.find((x) => x.name === b.name).status, at };
    return out;
  }

  function gcJob(repo, dryRun, graceHours) {
    const task = startJob({
      kind: 'backup-gc',
      target: repo.config.name,
      repo,
      lock: 'gc',
      ms: 3500,
      steps: (i, n) =>
        i < n / 2
          ? `marking: reading ${repo.backups.size} manifests`
          : dryRun
            ? 'listing candidates (dry run)'
            : 'sweeping under an exclusive lock',
      finish: () => {
        const refs = referenced(repo);
        const cutoff = Date.now() - graceHours * HOUR;
        const unref = [...repo.blobs].filter(([id]) => !refs.has(id));
        const old = unref.filter(([, x]) => Date.parse(x.at) < cutoff);
        const bytes = old.reduce((a, [, x]) => a + x.size, 0);
        let stored = 0;
        for (const x of repo.blobs.values()) stored += x.size;
        if (!dryRun) for (const [id] of old) repo.blobs.delete(id);
        const report = {
          dryRun,
          manifests: repo.backups.size,
          referencedBlobs: refs.size,
          listedBlobs: repo.blobs.size + (dryRun ? 0 : old.length),
          candidates: old.length,
          deleted: old.length,
          deletedBytes: bytes,
          keptYoung: unref.length - old.length,
          storedBytesAfter: stored - bytes,
          requests: {
            list: 4,
            get: repo.backups.size,
            delete: dryRun ? 0 : Math.ceil(old.length / 1000),
          },
          millis: 1200 + repo.backups.size * 10,
          lockWaitMillis: dryRun ? 0 : 8,
        };
        if (!dryRun) repo.lastGc = { ...report, finished: iso(Date.now()) };
        return {
          message: dryRun
            ? `dry run: ${old.length} blobs (${mb(bytes)} MB) can be deleted`
            : `deleted ${old.length} blobs (${mb(bytes)} MB)`,
          detail: report,
        };
      },
    });
    return task;
  }

  function runPolicy(p, trigger) {
    if (p.state?.runningTask)
      throw err(
        409,
        'policy-running',
        `policy ${p.name} is running (task ${p.state.runningTask})`,
        {
          task: p.state.runningTask,
        },
      );
    const repo = getRepo(p.repository, { write: true });
    const runId = randomUUID();
    const at = Date.now();
    const selected = [...datasets.values()].filter((d) => p.datasets.some((g) => glob(g, d.name)));
    const items = [];
    const task = startJob({
      kind: 'backup-policy',
      target: p.name,
      repo,
      ms: 1500 + selected.length * 1500,
      steps: (i, n) => {
        const k = Math.min(selected.length - 1, Math.floor((i / n) * selected.length));
        return selected.length
          ? `dataset ${k + 1}/${selected.length}: ${selected[k].name}`
          : 'no datasets match';
      },
      finish: () => {
        for (const ds of selected) {
          if (ds.type === 'mem') {
            items.push({
              dataset: ds.name,
              backup: null,
              result: 'skipped',
              reason: 'in-memory dataset',
            });
            continue;
          }
          const last = [...repo.backups.values()]
            .filter((b) => b.policy === p.name && b.dataset.id === ds.id)
            .sort((a, b) => b.completed.localeCompare(a.completed))[0];
          if (p.skipUnchanged && last && last.commit.seq === headCommit(ds).seq) {
            items.push({ dataset: ds.name, backup: null, result: 'skipped', reason: 'unchanged' });
            continue;
          }
          let name = renderName(p, ds.name, headCommit(ds).seq, runId, at);
          for (let k = 2; repo.backups.has(name); k++) name = `${name.replace(/-\d+$/, '')}-${k}`;
          const t0 = Date.now();
          const { backup } = storeBackup(repo, sourceOf(ds), {
            name,
            created: t0,
            completed: t0 + 1800,
            policy: p.name,
            run: runId,
          });
          items.push({
            dataset: ds.name,
            backup: name,
            result: 'ok',
            addedBytes: backup.addedBytes,
            millis: 1800,
          });
        }
        const r = policyRun(p.name, runId, trigger, at, items);
        r.scheduledFor = trigger === 'manual' ? null : iso(at);
        r.started = iso(at);
        r.finished = iso(Date.now());
        r.retention = { deleted: applyRetention(p, false).delete.map((b) => b.name) };
        if (p.gcAfterRetention) r.gc = { task: gcJob(repo, false, 24).id };
        runs.unshift(r);
        return {
          message: `${items.filter((d) => d.result === 'ok').length}/${items.length} datasets backed up`,
          detail: r,
        };
      },
    });
    p.state.runningTask = task.id;
    jobs.get(task.id).onEnd = () => {
      p.state.runningTask = null;
    };
    return task;
  }

  function glob(pattern, name) {
    const re = new RegExp(
      `^${pattern
        .split('*')
        .map((s) => s.replace(/[.+?^${}()|[\]\\]/g, '\\$&'))
        .join('.*')}$`,
    );
    return re.test(name);
  }

  function restoreJob(ds, repo, b, body) {
    const target = String(body.target || ds);
    const replace = body.replace === true;
    const identityWanted = ['auto', 'new', 'keep'].includes(body.identity) ? body.identity : 'auto';
    const check = ['quick', 'full', 'none'].includes(body.check) ? body.check : 'quick';
    if (!DATASET_NAME.test(target))
      throw err(400, 'invalid-name', `invalid dataset name “${target}”`);
    if (level(target) !== 'admin') throw err(403, 'forbidden', `requires admin on ${target}`);
    const existing = datasets.get(target);
    if (replace) {
      if (!existing) throw err(404, 'no-such-dataset', `No such dataset: ${target}`);
      if (existing.type === 'mem')
        throw err(409, 'not-managed', `${target} is an in-memory dataset and cannot be replaced`);
      const other = [...jobs.values()].find(
        (j) => isActive(j.task) && (j.task.dataset === target || j.task.target === target),
      );
      if (other)
        throw err(409, 'dataset-busy', `task ${other.task.id} works on ${target}`, {
          task: other.task.id,
        });
    } else if (existing) {
      throw err(409, 'dataset-exists', `dataset /${target} already exists`);
    }
    const idInUse = [...datasets.values()].some(
      (d) => d.id === b.dataset.id && !(replace && d === existing),
    );
    if (identityWanted === 'keep' && idInUse)
      throw err(409, 'duplicate-dataset-id', `another dataset has the id ${b.dataset.id}`);
    if (
      identityWanted === 'keep' &&
      replace &&
      existing.id === b.dataset.id &&
      headCommit(existing).seq > b.commit.seq
    )
      throw err(
        409,
        'duplicate-dataset-id',
        `keeping the id would issue commits ${b.commit.seq + 1}–${headCommit(existing).seq} again; use identity new`,
      );
    const keep =
      identityWanted === 'keep' ||
      (identityWanted === 'auto' && !idInUse && !(replace && existing.id === b.dataset.id));
    const total = b.logicalBytes;
    const nblobs = b.files.reduce((a, f) => a + f.blobs.length, 0);
    const t0 = Date.now();
    const task = startJob({
      kind: 'backup-restore',
      dataset: ds,
      target,
      backup: b.name,
      repo,
      lock: 'restore',
      ms: 6000,
      steps: (i, n, t) => {
        const swapAt = n - 2;
        if (replace && i >= swapAt) t.cancellable = false;
        if (i < n - 5) {
          const f = i / (n - 5);
          return `downloading ${mb(total * f)}/${mb(total)} MB · ${Math.round(nblobs * f)}/${nblobs} blobs`;
        }
        if (i < n - 3) return check === 'none' ? 'fsync' : `checking (${check})`;
        if (i < swapAt) return 'opening the restored store';
        return replace ? `swapping /${target}` : `publishing /${target}`;
      },
      finish: () => {
        if (b.corrupt)
          throw err(
            422,
            'invalid-backup',
            `blob ${b.files[3].blobs[0].id.slice(0, 12)}… failed its SHA-256 check (after 2 retries)`,
          );
        if (!repo.backups.has(b.name))
          throw err(404, 'no-such-backup', 'the backup was deleted during the restore');
        let d = existing;
        if (replace) {
          for (const q of d.store.match()) d.store.delete(q);
        } else {
          d = makeDataset(target, 'persistent');
        }
        if (b.quads) for (const q of b.quads) d.store.add(q);
        else
          d.store.load(
            `@prefix ex: <http://example.org/wiki/> .\nex:Main_Page ex:title "Main Page" ; ex:links ex:Help .\nex:Help ex:title "Help" .\n`,
            { format: 'text/turtle' },
          );
        d.baseQuads = d.store.size;
        d.deltaInserts = 0;
        d.deltaDeletes = 0;
        const forkedFrom = keep ? undefined : { id: b.dataset.id, seq: b.commit.seq };
        d.id = keep ? b.dataset.id : randomUUID();
        d.commits = [];
        addCommit(d, 'load', d.store.size, 0, { quads: d.store.size, bulk: true });
        d.commits[0].seq = b.commit.seq;
        d.commits[0].parent = b.commit.seq ? b.commit.seq - 1 : null;
        d.commits[0].ref = `commit:${b.commit.seq}`;
        d.firstRetained = b.commit.seq;
        d.restoredFrom = {
          repository: repo.config.name,
          backup: b.name,
          datasetId: b.dataset.id,
          seq: b.commit.seq,
        };
        return {
          message: `restored ${b.name} into /${target}${replace ? ' (replaced)' : ''}`,
          detail: {
            backup: summary(b),
            dataset: target,
            datasetId: d.id,
            identity: keep ? 'kept' : 'new',
            ...(forkedFrom ? { forkedFrom } : {}),
            check: check === 'none' ? null : { status: 'ok', mode: check },
            millis: Date.now() - t0,
          },
        };
      },
    });
    return task;
  }

  // --- routing ----------------------------------------------------------------------------

  async function jsonBody(req) {
    const body = await readBody(req);
    if (!body.length) return {};
    try {
      return JSON.parse(body.toString('utf8'));
    } catch {
      throw err(400, 'invalid-request', 'the body is not valid JSON');
    }
  }

  function validateRepo(body, existing) {
    const c = {
      name: String(body.name ?? ''),
      type: body.type,
      ...(body.path != null ? { path: String(body.path) } : {}),
      ...(body.bucket != null ? { bucket: String(body.bucket) } : {}),
      ...(body.prefix ? { prefix: String(body.prefix) } : {}),
      ...(body.region ? { region: String(body.region) } : {}),
      ...(body.endpoint ? { endpoint: String(body.endpoint) } : {}),
      ...(body.pathStyle != null ? { pathStyle: !!body.pathStyle } : {}),
      ...(body.allowHttp != null ? { allowHttp: !!body.allowHttp } : {}),
      credentials: body.credentials ?? undefined,
      sse: body.sse ?? null,
      ...(body.kmsKeyId ? { kmsKeyId: String(body.kmsKeyId) } : {}),
      conditionalWrites: body.conditionalWrites !== false,
      readonly: !!body.readonly,
      maxConcurrency: Number(body.maxConcurrency ?? (body.type === 'fs' ? 4 : 8)),
      maxUploadBytesPerSec: body.maxUploadBytesPerSec ?? null,
      maxDownloadBytesPerSec: body.maxDownloadBytesPerSec ?? null,
    };
    if (!REPO_NAME.test(c.name))
      throw err(400, 'invalid-name', `invalid repository name “${c.name}”`);
    if (!['fs', 's3', 'gcs', 'azure'].includes(c.type))
      throw err(400, 'invalid-config', `unknown type “${c.type}”`);
    if (c.type === 'gcs' || c.type === 'azure')
      throw err(
        400,
        'invalid-config',
        `type: ${c.type} repositories use the server's own credentials and are defined in its backup config file`,
      );
    if (c.type === 'fs' && !/^\//.test(c.path ?? ''))
      throw err(400, 'invalid-config', 'path must be an absolute path');
    if (c.type === 'fs' && /^\/var\/lib\/sparkles(\/|$)/.test(c.path))
      throw err(400, 'invalid-config', 'path must not lie inside the data directory');
    if (c.type !== 'fs' && !c.bucket) throw err(400, 'invalid-config', 'bucket is required');
    if (c.endpoint) {
      let u;
      try {
        u = new URL(c.endpoint);
      } catch {
        throw err(400, 'invalid-config', `endpoint “${c.endpoint}” is not a URL`);
      }
      if (u.username || u.password || /(^|&)(x-amz-|signature|access)/i.test(u.search.slice(1)))
        throw err(400, 'invalid-config', 'endpoint must not carry credentials');
      if (u.protocol === 'http:' && !c.allowHttp)
        throw err(400, 'invalid-config', 'an http:// endpoint needs allowHttp: true');
    }
    // registered through the API: only a credential source the operator named
    const cred = c.credentials;
    if (c.type === 's3' && cred?.source !== 'named')
      throw err(
        400,
        'invalid-config',
        'credentials: repositories registered through the API use a credential source defined in the server\'s backup config file: {"source": "named", "name": …}',
      );
    if (c.type === 's3' && !CREDENTIAL_SOURCES.includes(cred.name))
      throw err(
        400,
        'invalid-config',
        `credentials.name: no credential source "${cred.name}" in the server's backup config file`,
      );
    if (c.type === 'fs' && cred)
      throw err(400, 'invalid-config', 'credentials: not used by fs repositories');
    if (existing) {
      for (const k of ['type', 'path', 'bucket', 'prefix', 'endpoint'])
        if ((existing.config[k] ?? '') !== (c[k] ?? ''))
          throw err(409, 'location-immutable', `the location (${k}) of a repository cannot change`);
    }
    return c;
  }

  const location = (c) => [c.type, c.path, c.bucket, c.prefix, c.endpoint].join('|');

  /** A simulated connection test; `/mnt/offline`, `no-such-bucket` and `legacy` endpoints misbehave. */
  function connectionTest(c) {
    const offline = c.path?.startsWith('/mnt/offline') || c.bucket === 'no-such-bucket';
    const conditional =
      !offline && !/legacy/.test(c.endpoint ?? '') && c.conditionalWrites !== false;
    const ms = c.type === 'fs' ? [0.4, 0.2, 0.3, 0.5, 0.2] : [38, 31, 22, 41, 27];
    const names = ['create', 'create-again', 'read', 'list', 'delete'];
    const steps = names.map((step, i) => ({
      step,
      ok: !offline && (step !== 'create-again' || conditional || c.conditionalWrites === false),
      millis: ms[i],
    }));
    if (offline) {
      steps[0].error =
        c.type === 'fs'
          ? `${c.path}: No such file or directory (os error 2)`
          : 'NoSuchBucket: The specified bucket does not exist';
      for (const s of steps.slice(1)) s.error = 'skipped';
    } else if (!conditional && c.conditionalWrites !== false) {
      steps[1].error = 'the second conditional create succeeded: the service ignores If-None-Match';
    }
    return { ok: steps.every((s) => s.ok), conditionalWrites: conditional, steps };
  }

  seed();

  return async function route(req, res, url, seg) {
    const [, what, a, b, c, d] = seg;
    const m = req.method;
    const reply = (status, body, headers) =>
      send(res, status, body ?? '', 'application/json', headers);
    try {
      if (what === 'whoami' && ROLE === 'dataset-admin') {
        reply(200, {
          authEnabled: true,
          principal: { kind: 'user', name: 'dana', displayName: 'Dana (foaf admin)' },
          method: 'session',
          csrfToken: 'mock-csrf',
          server: [],
          datasets: roleDatasets,
          canMintTokens: false,
          logout: false,
        });
        return true;
      }

      // generic cancellation
      if (what === 'tasks' && a && m === 'DELETE') {
        reply(202, cancel(a));
        return true;
      }

      if (what === 'repositories') {
        if (!a) {
          if (m === 'GET') {
            const list = [...repos.values()];
            if (serverAdmin()) reply(200, { repositories: list.map(repoJson) });
            else
              reply(200, {
                repositories: Object.values(roleDatasets).includes('admin') ? list.map(brief) : [],
              });
            return true;
          }
          if (m === 'POST') {
            needServerAdmin();
            const body = await jsonBody(req);
            const cfg = validateRepo(body);
            if (repos.has(cfg.name))
              throw err(409, 'repository-exists', `a repository named “${cfg.name}” exists`);
            const same = [...repos.values()].find((r) => location(r.config) === location(cfg));
            if (same)
              throw err(
                409,
                'repository-exists',
                `this location is already registered as “${same.config.name}”`,
              );
            if (cfg.path === '/srv/not-a-repo')
              throw err(
                409,
                'not-a-repository',
                `${cfg.path} is not empty and has no sparkles-repo.json`,
              );
            const verify = url.searchParams.get('verify') !== 'false';
            const test = verify ? connectionTest(cfg) : null;
            const repo = addRepo(cfg, 'api', {
              reachable: test ? test.ok : true,
              ...(test ? { conditionalWrites: test.conditionalWrites } : {}),
              ...(test && !test.ok ? { error: test.steps.find((s) => !s.ok)?.error } : {}),
              singleWriter:
                cfg.conditionalWrites === false || (!!test?.ok && !test.conditionalWrites),
            });
            reply(
              201,
              { ...repoJson(repo), ...(test ? { test } : {}) },
              {
                Location: `/$/repositories/${encodeURIComponent(cfg.name)}`,
              },
            );
            return true;
          }
          return false;
        }
        needServerAdmin();
        const repo = repos.get(a);
        if (!repo) throw err(404, 'no-such-repository', `no repository named “${a}”`);
        if (!b) {
          if (m === 'GET') reply(200, repoJson(repo));
          else if (m === 'PUT') {
            if (repo.source === 'config')
              throw err(
                409,
                'read-only-config',
                `“${a}” comes from the config file; edit it there`,
              );
            const body = await jsonBody(req);
            if (body.name && body.name !== a)
              throw err(400, 'invalid-name', 'the name cannot change');
            repo.config = validateRepo({ ...body, name: a }, repo);
            repo.status.singleWriter =
              repo.config.conditionalWrites === false || repo.status.conditionalWrites === false;
            reply(200, repoJson(repo));
          } else if (m === 'DELETE') {
            if (repo.source === 'config')
              throw err(
                409,
                'read-only-config',
                `“${a}” comes from the config file; remove it there`,
              );
            const users = [...policies.values()]
              .filter((p) => p.repository === a)
              .map((p) => p.name);
            if (users.length)
              throw err(
                409,
                'repository-in-use',
                `policies ${users.join(', ')} back up into “${a}”`,
                { policies: users },
              );
            const busy = [...jobs.values()].find((j) => j.repo === repo && isActive(j.task));
            if (busy)
              throw err(409, 'repository-in-use', `task ${busy.task.id} uses “${a}”`, {
                task: busy.task.id,
              });
            repos.delete(a);
            reply(204);
          } else return false;
          return true;
        }
        if (b === 'test' && m === 'POST') {
          const test = connectionTest(repo.config);
          if (repo.config.name === 'minio-lab') {
            test.ok = false;
            for (const s of test.steps) {
              s.ok = false;
              s.error = 'error sending request (connection refused)';
            }
          }
          repo.status = {
            ...repo.status,
            reachable: test.ok,
            checked: iso(Date.now()),
            conditionalWrites: test.conditionalWrites,
            ...(test.ok ? { error: undefined } : { error: test.steps.find((s) => !s.ok)?.error }),
          };
          if (test.ok) repo.id ??= randomUUID();
          reply(200, test);
          return true;
        }
        if (b === 'verify' && m === 'POST') {
          getRepo(a);
          const body = await jsonBody(req);
          const lvl = body.level === 'data' ? 'data' : 'exists';
          const list = [...repo.backups.values()];
          reply(
            202,
            startJob({
              kind: 'backup-verify',
              target: a,
              repo,
              lock: 'verify',
              ms: lvl === 'data' ? 6000 : 2500,
              steps: (i, n) =>
                `${lvl}: ${Math.round((i / n) * list.length)}/${list.length} backups`,
              finish: () => {
                const report = verifyReport(
                  lvl,
                  list.filter((x) => repo.backups.has(x.name)),
                  repo,
                  true,
                );
                return {
                  message: `${report.status}: ${list.length} backups (${lvl})`,
                  detail: report,
                };
              },
            }),
          );
          return true;
        }
        if (b === 'backups' && m === 'GET') {
          getRepo(a);
          const q = url.searchParams;
          const limit = Math.min(1000, Number(q.get('limit') ?? 100));
          let list = [...repo.backups.values()].sort((x, y) =>
            y.completed.localeCompare(x.completed),
          );
          if (q.get('dataset')) list = list.filter((x) => x.dataset.name === q.get('dataset'));
          if (q.get('datasetId')) list = list.filter((x) => x.dataset.id === q.get('datasetId'));
          if (q.get('policy')) list = list.filter((x) => x.policy === q.get('policy'));
          if (q.get('before')) list = list.filter((x) => x.completed < q.get('before'));
          const page = list.slice(0, limit);
          reply(200, {
            backups: page.map((x) => summary(x)),
            next: list.length > limit ? page[page.length - 1].completed : null,
          });
          return true;
        }
        if (b === 'gc' && m === 'POST') {
          getRepo(a, { write: true });
          const body = await jsonBody(req);
          const grace = Number(body.graceHours ?? 24);
          if (!(grace >= 0)) throw err(400, 'invalid-request', 'graceHours must be ≥ 0');
          reply(202, gcJob(repo, body.dryRun === true, grace));
          return true;
        }
        if (b === 'locks') {
          getRepo(a);
          if (!c && m === 'GET') {
            reply(200, { locks: repo.locks });
            return true;
          }
          if (c && m === 'DELETE') {
            if (!repo.locks.some((l) => l.id === c)) throw err(404, 'no-such-lock', `no lock ${c}`);
            repo.locks = repo.locks.filter((l) => l.id !== c);
            reply(204);
            return true;
          }
        }
        return false;
      }

      if (what === 'backups' && a) {
        const ds = a;
        if (!b) {
          if (m === 'GET') {
            needDataset(ds, 'read');
            const live = datasets.get(ds) ?? null;
            const only = url.searchParams.get('repository');
            const list = [...repos.values()]
              .filter((r) => r.status.reachable && (!only || r.config.name === only))
              .flatMap((r) => [...r.backups.values()])
              .filter((x) => x.dataset.name === ds || (live && x.dataset.id === live.id))
              .sort((x, y) => y.completed.localeCompare(x.completed));
            reply(200, {
              dataset: ds,
              datasetId: live?.id ?? null,
              backups: list.map((x) => summary(x, live)),
            });
            return true;
          }
          if (m === 'POST') {
            needDataset(ds, 'admin');
            const live = datasets.get(ds);
            if (!live) throw err(404, 'no-such-dataset', `No such dataset: ${ds}`);
            const body = await jsonBody(req);
            const repo = getRepo(String(body.repository ?? ''), { write: true });
            if (live.type === 'mem')
              throw err(501, 'backup-unsupported', 'in-memory datasets cannot be backed up yet');
            const name = body.name ? String(body.name) : `${ds}-${timeToken(Date.now())}`;
            if (!BACKUP_NAME.test(name))
              throw err(400, 'invalid-name', `invalid backup name “${name}”`);
            const task = createBackupJob(live, repo, name, { note: body.note || undefined });
            reply(202, task, {
              Location: `/$/backups/${encodeURIComponent(ds)}/${encodeURIComponent(repo.config.name)}/${encodeURIComponent(name)}`,
            });
            return true;
          }
          return false;
        }
        if (!c) return false;
        const { repo, b: backup } = (() => {
          needDataset(ds, m === 'GET' ? 'read' : 'admin');
          return findBackup(ds, b, c);
        })();
        if (!d) {
          if (m === 'GET') reply(200, manifest(backup));
          else if (m === 'DELETE') {
            if (repo.config.readonly)
              throw err(409, 'repository-read-only', `repository “${b}” is read-only`);
            const busy = [...jobs.values()].find(
              (j) => j.repo === repo && j.backup === c && isActive(j.task),
            );
            if (busy)
              throw err(409, 'backup-busy', `task ${busy.task.id} uses ${c}`, {
                task: busy.task.id,
              });
            deleteBackup(repo, backup);
            reply(204);
          } else return false;
          return true;
        }
        if (d === 'restore' && m === 'POST') {
          const body = await jsonBody(req);
          const task = restoreJob(ds, repo, backup, body);
          reply(202, task, { Location: `/$/datasets/${encodeURIComponent(task.target)}` });
          return true;
        }
        if (d === 'verify' && m === 'POST') {
          const body = await jsonBody(req);
          const lvl = ['exists', 'data', 'restore'].includes(body.level) ? body.level : 'exists';
          reply(
            202,
            startJob({
              kind: 'backup-verify',
              dataset: ds,
              target: c,
              backup: c,
              repo,
              lock: 'verify',
              ms: { exists: 1500, data: 4000, restore: 7000 }[lvl],
              steps: (i, n) =>
                lvl === 'restore' && i > n / 2
                  ? 'restoring into a temporary directory and checking'
                  : `${lvl}: ${Math.round((i / n) * 100)}% of blobs`,
              finish: () => {
                const report = verifyReport(lvl, [backup], repo, false);
                return { message: `${report.status}: ${c} (${lvl})`, detail: report };
              },
            }),
          );
          return true;
        }
        return false;
      }

      if (what === 'backup-policies') {
        needServerAdmin();
        if (!a) {
          if (m === 'GET') {
            reply(200, { policies: [...policies.values()].map(policyJson) });
            return true;
          }
          if (m === 'POST') {
            const p = validatePolicy(await jsonBody(req));
            if (policies.has(p.name) || repos.has(p.name))
              throw err(409, 'policy-exists', `a policy named “${p.name}” exists`);
            const policy = addPolicy(p, 'api');
            reply(201, policyJson(policy), {
              Location: `/$/backup-policies/${encodeURIComponent(p.name)}`,
            });
            return true;
          }
          return false;
        }
        if (a === 'preview' && m === 'POST') {
          const body = await jsonBody(req);
          const tz = String(body.timezone ?? 'UTC');
          checkZone(tz);
          let sched;
          try {
            sched = parseSchedule(body.schedule);
          } catch (e) {
            throw err(400, 'invalid-schedule', `invalid schedule: ${e.message}`);
          }
          const count = Math.min(20, Math.max(1, Number(body.count ?? 5)));
          const next = nextRuns(sched, tz, Date.now(), count);
          const out = { next: next.map(iso), description: describe(sched, tz) };
          if (body.nameTemplate) {
            try {
              out.sample = renderName(
                { name: 'policy', nameTemplate: String(body.nameTemplate), timezone: tz },
                String(body.dataset || 'dataset'),
                42,
                randomUUID(),
                next[0] ?? Date.now(),
              );
            } catch (e) {
              throw err(400, 'invalid-config', e.message);
            }
          }
          reply(200, out);
          return true;
        }
        const p = policies.get(a);
        if (!p) throw err(404, 'no-such-policy', `no policy named “${a}”`);
        if (!b) {
          if (m === 'GET') reply(200, policyJson(p));
          else if (m === 'PUT') {
            if (p.source === 'config')
              throw err(
                409,
                'read-only-config',
                `“${a}” comes from the config file; edit it there`,
              );
            const body = await jsonBody(req);
            if (body.name && body.name !== a)
              throw err(400, 'invalid-name', 'the name cannot change');
            Object.assign(p, validatePolicy({ ...body, name: a }, p));
            reply(200, policyJson(p));
          } else if (m === 'DELETE') {
            if (p.source === 'config')
              throw err(
                409,
                'read-only-config',
                `“${a}” comes from the config file; remove it there`,
              );
            policies.delete(a);
            reply(204);
          } else return false;
          return true;
        }
        if (b === 'run' && m === 'POST') {
          reply(202, runPolicy(p, 'manual'));
          return true;
        }
        if (b === 'retention' && m === 'POST') {
          getRepo(p.repository, { write: url.searchParams.get('dryRun') !== 'true' });
          reply(200, applyRetention(p, url.searchParams.get('dryRun') === 'true'));
          return true;
        }
        if (b === 'runs' && m === 'GET') {
          const limit = Number(url.searchParams.get('limit') ?? 50);
          reply(200, { runs: runs.filter((r) => r.policy === a).slice(0, limit) });
          return true;
        }
        return false;
      }
      return false;
    } catch (e) {
      if (e instanceof HttpError) {
        send(res, e.status, { error: e.message, code: e.code, ...e.extra });
        return true;
      }
      throw e;
    }
  };
}
