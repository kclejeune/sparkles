import {
  Dataset,
  Catalog,
  Repository,
  factory,
  type CommitReceipt,
  type RDF,
  type SparqlDataset,
  parseQuery,
  parseUpdate,
  checkData,
  checkIri,
  checkLangtag,
  convertGeometries,
  format,
  lint,
  previewSchedule,
  type BackupPolicy,
  type PolicyRun,
  type RetentionReport,
} from '@sparkles-rdf/engine';
import { SparklesClient } from '@sparkles-rdf/client';
const local = Dataset.memory();
const ds: SparqlDataset = local;
const term: RDF.NamedNode = factory.namedNode('urn:typed');
const rows = await ds.select('SELECT * {?s ?p ?o}');
for await (const row of rows) {
  const value: RDF.Term | undefined = row.get('x');
  void value;
}
const result = await ds.update('INSERT DATA {}');
const receipt: CommitReceipt | undefined = result.receipt;
void receipt;
void term;
void Catalog;
void Repository;
const info = await local.info();
const count: bigint = info.quads;
void count;
const snapshot = await local.snapshots.create('typed');
const seq: bigint = snapshot.snapshot.seq;
void seq;
await local.branches.create('draft');
const branch: Dataset = await local.branches.open('draft');
await branch.close();
await local.settings.describe.set({ mode: 'cbd' });
await local.validation.guard.get();
const head = await local.history.commit();
const commit: bigint | undefined = head?.seq;
void commit;
const client = new SparklesClient('http://localhost:3000');
await client.api.GET('/$/datasets');
await client.api.POST('/$/datasets', { body: { dbName: 'typed', dbType: 'mem' } });
await local.close();
const parsedQuery: string = await parseQuery('ASK {}', { prefixes: { ex: 'urn:' } });
const parsedUpdate: string = await parseUpdate('INSERT DATA {}');
const issue = await checkData('', 'ttl');
const line: bigint | null | undefined = issue?.line;
const cursor: number | null = (
  await format('ASK {}', 'sparql', { cursor: 0, directiveStyle: 'turtle' })
).cursor;
const level: 'error' | 'warning' | 'info' | 'hint' | undefined = (await lint('ASK {}', 'sparql'))
  .diagnostics[0]?.severity;
const errors: string[] = (await checkIri('urn:typed')).errors;
const tag: string | null = (await checkLangtag('en-us')).canonical;
const geometry = (
  await convertGeometries([
    { value: 'POINT(1 2)', datatype: 'http://www.opengis.net/ont/geosparql#wktLiteral' },
  ])
)[0];
const future: string[] = await previewSchedule('every 6h', { count: 1, after: new Date() });
async function catalogHelpers(cat: Catalog, policy: BackupPolicy) {
  const run: PolicyRun = await cat.runPolicy(policy, { timeout: 1000 });
  const report: RetentionReport = await cat.applyRetention(policy);
  const bytes: bigint | undefined = run.datasets[0]?.addedBytes;
  const logical: bigint | undefined = report.keep[0]?.logicalBytes;
  const files: { name: string; path: string }[] = await cat.backupFiles();
  const claim = cat.reserve('new', 'clone', 'typed');
  claim.close();
  await cat.repositories.withFixed([{ name: 'archive', type: 'fs', path: './backups' }]);
  void bytes;
  void logical;
  void files;
}
void parsedQuery;
void parsedUpdate;
void line;
void cursor;
void level;
void errors;
void tag;
void geometry;
void future;
void catalogHelpers;
