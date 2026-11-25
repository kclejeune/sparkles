import test from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { native } from '../dist/native.js';

const PT_DYNAMIC = 2;
const PT_TLS = 7;
const DT_NULL = 0n;
const DT_FLAGS = 30n;
const DF_STATIC_TLS = 0x10n;
const MAX_TLS_MEMSZ = 2048n;

function resolveNativePath() {
  if (process.env.SPARKLES_NODE_NATIVE) return process.env.SPARKLES_NODE_NATIVE;
  const libc =
    process.platform === 'linux' && !process.report?.getReport()?.header?.glibcVersionRuntime
      ? 'musl'
      : 'gnu';
  const target = `${process.platform}-${process.arch}${process.platform === 'linux' ? '-' + libc : process.platform === 'win32' ? '-msvc' : ''}`;
  const local = fileURLToPath(new URL(`../native/sparkles.${target}.node`, import.meta.url));
  if (existsSync(local)) return local;
  return createRequire(import.meta.url).resolve(`@sparkles-rdf/engine-${target}`);
}

function inspectElf64Tls(buf) {
  assert.equal(buf[0], 0x7f, 'ELF magic byte 0');
  assert.equal(buf[1], 0x45, 'ELF magic byte 1');
  assert.equal(buf[2], 0x4c, 'ELF magic byte 2');
  assert.equal(buf[3], 0x46, 'ELF magic byte 3');
  assert.equal(buf[4], 2, 'ELFCLASS64');
  const le = buf[5] === 1;
  const view = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
  const phoff = Number(view.getBigUint64(32, le));
  const phentsize = view.getUint16(54, le);
  const phnum = view.getUint16(56, le);

  let tlsMemsz = 0n;
  let dynamicOffset = null;
  let dynamicFilesz = 0;

  for (let i = 0; i < phnum; i++) {
    const off = phoff + i * phentsize;
    const type = view.getUint32(off, le);
    if (type === PT_TLS) {
      tlsMemsz = view.getBigUint64(off + 40, le);
    } else if (type === PT_DYNAMIC) {
      dynamicOffset = Number(view.getBigUint64(off + 8, le));
      dynamicFilesz = Number(view.getBigUint64(off + 32, le));
    }
  }

  let dtFlags = 0n;
  assert.notEqual(dynamicOffset, null, 'ELF shared library must have PT_DYNAMIC');
  for (let pos = dynamicOffset; pos + 16 <= dynamicOffset + dynamicFilesz; pos += 16) {
    const tag = view.getBigInt64(pos, le);
    const val = view.getBigUint64(pos + 8, le);
    if (tag === DT_NULL) break;
    if (tag === DT_FLAGS) dtFlags |= val;
  }

  return {
    tlsMemsz,
    hasStaticTls: (dtFlags & DF_STATIC_TLS) !== 0n,
  };
}

test(
  'Linux native addon avoids DF_STATIC_TLS and keeps PT_TLS bounded for dlopen',
  { skip: process.platform !== 'linux' },
  () => {
    assert.ok(native.NativeDataset, 'native addon must load without GLIBC_TUNABLES');
    const path = resolveNativePath();
    const { tlsMemsz, hasStaticTls } = inspectElf64Tls(readFileSync(path));
    assert.equal(
      hasStaticTls,
      false,
      `${path} sets DF_STATIC_TLS (PT_TLS MemSiz=${tlsMemsz}); dlopen-loaded addons must use local-dynamic TLS`,
    );
    assert.ok(
      tlsMemsz <= MAX_TLS_MEMSZ,
      `${path} PT_TLS MemSiz=${tlsMemsz} exceeds ${MAX_TLS_MEMSZ} bytes`,
    );
  },
);
