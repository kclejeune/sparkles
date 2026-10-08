import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

// The addon's classes and functions as crates/sparkles-node exports them. Options,
// arguments and results cross as JSON text, and unsigned 64-bit numbers as decimal text.

/** A cancellation token that native calls poll. */
export interface NativeCancellation {
  cancel(): void;
}
/** A query result that JavaScript pulls in batches. */
export interface NativeResult {
  info(): string;
  close(): Promise<void>;
  stats(): Promise<string>;
  nextBatch(maxRows: number, maxBytes: number): Promise<string>;
}
export interface NativeTransaction {
  apply(operations: string): Promise<string>;
  query(text: string, options: string, cancel: NativeCancellation): Promise<NativeResult>;
  update(text: string, options: string, cancel: NativeCancellation): Promise<string>;
  matched(pattern: string): Promise<NativeResult>;
  end(commit: boolean): Promise<string>;
}
export interface NativeByteStream {
  next(): Promise<Buffer | null>;
  close(): void;
}
export interface NativeUpload {
  push(bytes: Buffer): Promise<void>;
  finish(): Promise<string>;
  abort(): void;
}
export interface NativeDataset {
  identity(): string;
  contextIdentity(): string;
  close(): Promise<void>;
  path(): string | null;
  count(): Promise<string>;
  countPattern(pattern: string): Promise<string>;
  matched(pattern: string): Promise<NativeResult>;
  query(text: string, options: string, cancel: NativeCancellation): Promise<NativeResult>;
  update(text: string, options: string, cancel: NativeCancellation): Promise<string>;
  begin(options: string, cancel: NativeCancellation): Promise<NativeTransaction>;
  waitCommit(after: string, cancel: NativeCancellation): Promise<string>;
  runQuery(
    name: string,
    params: string,
    version: string | null | undefined,
    options: string,
    cancel: NativeCancellation,
  ): Promise<NativeResult>;
  cloneMemory(name: string, options: string, cancel: NativeCancellation): Promise<NativeDataset>;
  branch(name: string): Promise<NativeDataset>;
  admin(op: string, args: string, options: string, cancel: NativeCancellation): Promise<string>;
  backups(
    repository: NativeRepository,
    op: string,
    args: string,
    options: string,
    cancel: NativeCancellation,
  ): Promise<string>;
  loadStart(options: string, cancel: NativeCancellation): Promise<NativeUpload>;
  loadPaths(paths: string[], options: string, cancel: NativeCancellation): Promise<string>;
  dumpStream(options: string, cancel: NativeCancellation): NativeByteStream;
  dumpFile(path: string, options: string, cancel: NativeCancellation): Promise<string>;
  queryStream(text: string, options: string, cancel: NativeCancellation): Promise<NativeByteStream>;
}
export interface NativeReservationHandle {
  close(): void;
}
export interface NativeRepository {
  admin(op: string, args: string, cancel: NativeCancellation): Promise<string>;
  close(): void;
}
export interface NativeCatalog {
  reserve(name: string, kind: string, holder: string): Promise<NativeReservationHandle>;
  cloneDataset(
    source: string,
    name: string,
    options: string,
    cancel: NativeCancellation,
  ): Promise<NativeDataset>;
  dataset(op: string, args: string): Promise<NativeDataset | null>;
  admin(op: string, args: string): Promise<string>;
  close(): void;
  runPolicy(policy: string, options: string, cancel: NativeCancellation): Promise<string>;
  applyRetention(policy: string, dryRun: boolean): Promise<string>;
  repository(name: string): Promise<NativeRepository>;
  restore(
    repository: NativeRepository,
    name: string,
    target: string,
    options: string,
    cancel: NativeCancellation,
  ): Promise<NativeDataset>;
}
export interface NativeRdfParser {
  push(bytes: Buffer): Promise<void>;
  end(): void;
  nextQuad(): Promise<string | null>;
  close(): void;
}
export interface NativeRdfSerializer {
  push(quad: string): Promise<void>;
  end(): void;
  nextBytes(): Promise<Buffer | null>;
  close(): void;
}
/** The addon module. */
export interface NativeModule {
  Cancellation: new () => NativeCancellation;
  NativeDataset: {
    memory(options: string): NativeDataset;
    open(path: string, options: string): Promise<NativeDataset>;
  };
  NativeCatalog: {
    memory(options: string): NativeCatalog;
    open(path: string, options: string): Promise<NativeCatalog>;
    inspect(path: string): Promise<string>;
  };
  NativeRepository: {
    open(config: string, init: boolean): Promise<NativeRepository>;
  };
  NativeRdfParser: new (options: string, cancel: NativeCancellation) => NativeRdfParser;
  NativeRdfSerializer: new (options: string, cancel: NativeCancellation) => NativeRdfSerializer;
  utility(op: string, args: string): Promise<string>;
  setMaxConcurrentQueries(limit: number): void;
}

const require = createRequire(import.meta.url);
const libc =
  process.platform === 'linux' &&
  !(process.report?.getReport() as { header?: { glibcVersionRuntime?: string } })?.header
    ?.glibcVersionRuntime
    ? 'musl'
    : 'gnu';
const target = `${process.platform}-${process.arch}${process.platform === 'linux' ? '-' + libc : process.platform === 'win32' ? '-msvc' : ''}`;
const override = process.env.SPARKLES_NODE_NATIVE;
const local = fileURLToPath(new URL(`../native/sparkles.${target}.node`, import.meta.url));
export const native: NativeModule = override
  ? require(override)
  : existsSync(local)
    ? require(local)
    : require(`@sparkles-rdf/engine-${target}`);
