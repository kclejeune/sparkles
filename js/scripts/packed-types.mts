import {
  Dataset,
  Catalog,
  Repository,
  factory,
  type CommitReceipt,
  type RDF,
  type SparqlDataset,
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
