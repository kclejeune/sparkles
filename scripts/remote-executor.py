#!/usr/bin/env python3
"""Small SSH/Boat transport boundary. Boat lifecycle belongs to the admission owner."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import stat
import shlex
import selectors
import signal
import subprocess
import sys
import tarfile
import tempfile
import time

SSH = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10',
       '-o', 'ServerAliveInterval=30']
MAX_ARCHIVE = 4 * 1024**3
MAX_MEMBERS = 100000


def fail(message):
    raise ValueError(message)


def boat_id(host):
    if host.startswith('boat:'):
        value = host[5:]
        if not re.fullmatch(r'bx_[a-zA-Z0-9]+', value):
            fail('Boat selector must be boat:bx_ID')
        return value
    if not re.fullmatch(r'[A-Za-z0-9_][A-Za-z0-9_.@-]*', host):
        fail('invalid SSH host selector')
    return None


def admission(host, minimum=0):
    if minimum < 0:
        fail('minimum lifetime cannot be negative')
    worker = boat_id(host)
    if not worker:
        return None
    path = os.environ.get('SPARKLES_BOAT_ADMISSION')
    if not path:
        fail('Boat needs SPARKLES_BOAT_ADMISSION from the qualified lifecycle owner')
    p = Path(path)
    if p.is_dir():
        p = p / (worker + ".json")
    if p.is_symlink() or not stat.S_ISREG(p.stat().st_mode) or p.stat().st_size > 16384:
        fail('admission must be a bounded regular file')
    data = json.loads(p.read_text())
    required = {'schema', 'worker', 'environment', 'environment_version', 'expires_at', 'owner',
                'guard_unit', 'guard_boot_id', 'guard_invocation_id', 'toolchain_manifest_path',
                'toolchain_manifest_sha256', 'bootstrap_lock_path', 'worker_env_path', 'worker_env_sha256'}
    if not isinstance(data, dict) or set(data) != required:
        fail('admission fields do not match schema 1')
    if type(data.get('schema')) is not int or data.get('schema') != 1 or data.get('worker') != worker:
        fail('unknown admission schema or worker mismatch')
    for field in required - {'schema', 'environment_version', 'expires_at'}:
        if not isinstance(data[field], str):
            fail('admission field must be text: ' + field)
    expiry = data.get('expires_at')
    if type(expiry) not in (int, float) or not math.isfinite(expiry) or expiry <= time.time() + minimum:
        fail('admission expired or has insufficient remaining lifetime')
    if data.get('environment') != 'sparkles' or type(data.get('environment_version')) is not int:
        fail('admission environment identity is missing')
    for field in ('owner',):
        if not re.fullmatch(r'[A-Za-z0-9_.@-]+', data.get(field, '')):
            fail('invalid admission ' + field)
    if not re.fullmatch(r'[a-f0-9]{64}', data.get('toolchain_manifest_sha256', '')):
        fail('admission has no toolchain manifest identity')
    if not re.fullmatch(r'boat-owned-warm-qual-[a-z0-9-]{1,40}\.service', data.get('guard_unit', '')):
        fail('admission guard must name the lifecycle owner service')
    if data.get('guard_boot_id') != Path('/proc/sys/kernel/random/boot_id').read_text().strip():
        fail('admission guard belongs to another boot')
    invocation = data.get('guard_invocation_id', '')
    if not re.fullmatch('[a-f0-9]{32}', invocation):
        fail('admission guard has no invocation identity')
    guard = subprocess.run(['systemctl', '--user', 'show', data['guard_unit'],
                            '--property=ActiveState,InvocationID'],
                           capture_output=True, text=True, timeout=10, check=True)
    observed = dict(line.split('=', 1) for line in guard.stdout.splitlines() if '=' in line)
    if observed.get('ActiveState') != 'active' or observed.get('InvocationID') != invocation:
        fail('lifecycle guard is inactive or replaced')
    manifest = data.get('toolchain_manifest_path', '')
    if not manifest.startswith('/') or any(c in manifest for c in '\n\r\0'):
        fail('admission manifest must be an absolute path')
    for key in ('worker_env_path', 'bootstrap_lock_path'):
        if not data.get(key, '').startswith('/') or any(c in data[key] for c in '\n\r\0'):
            fail('admission needs an absolute ' + key)
    if not re.fullmatch('[a-f0-9]{64}', data.get('worker_env_sha256', '')):
        fail('admission worker environment identity missing')
    return data


def command(host, text):
    worker = boat_id(host)
    if worker:
        data = admission(host)
        wrapped = ('set -e; test \"$(sha256sum ' + shlex.quote(data['worker_env_path']) + ' | cut -d\' \' -f1)\" = ' + shlex.quote(data['worker_env_sha256']) + '; set -a; . ' + shlex.quote(data['worker_env_path']) + '; set +a; cd \"$HOME\"; ' + text)
        return ['boat', '--no-update', 'ssh', worker, '--', 'bash', '-c', shlex.quote(wrapped)]
    return [*SSH, host, text]


def run(host, text, **kwargs):
    return subprocess.run(command(host, text), **kwargs)


def admit(host, minimum):
    data = admission(host, minimum)
    if data is None:
        return
    result = subprocess.run(['boat', '--no-update', '--json', 'info', data['worker']],
                            check=True, capture_output=True, timeout=30)
    if len(result.stdout) > 65536:
        fail('oversized worker metadata')
    body = json.loads(result.stdout)
    worker = body.get('sandbox', {})
    for field, wanted in [('id', data['worker']), ('environment', data['environment']),
                          ('environmentVersion', data['environment_version']),
                          ('type', 'large'), ('vcpu', 8), ('memoryGB', 16)]:
        if worker.get(field) != wanted:
            fail('worker metadata mismatch: ' + field)
    if worker.get('state') not in ('ready', 'idle', 'running'):
        fail('worker is not ready')
    if worker.get('setupStatus') in ('failed', 'error') or worker.get('setupError'):
        fail('worker setup failed')
    # setupStatus=null alone is no readiness evidence. The owner binds startup
    # quiescence and guard lifetime; these physical checks detect subsequent drift.
    probe = """set -euo pipefail
test -f {envfile}
test "$(stat -c %a {envfile})" = 600
test "$(sha256sum {envfile} | cut -d' ' -f1)" = {envhash}
exec 8>{bootstrap}
flock -n 8
test "$(sha256sum {manifest} | cut -d' ' -f1)" = {digest}
test "$(mbx --version | awk '{{print $2}}')" = 1.29.1
shim=$(command -v cargo)
test -f "$shim"
test "$(sha256sum "$shim" | cut -d' ' -f1)" = 9cfafb636d06f397aa5c48a68ac7755865d424f7acafe075e6fce3bcaa723843
physical=$(command -v mbx)
target_record=$(dirname "$shim")/mbx-target
test -f "$target_record" && test ! -L "$target_record"
test "$(stat -c %s "$target_record")" -le 4096
IFS= read -r child_mbx < "$target_record"
test -f "$child_mbx"
test "$(sha256sum "$child_mbx" | cut -d' ' -f1)" = 4b0c0fcfc9329a9d76fd6bdefc1ee34822580864f5f5ba2f0b47dcbbffce2eca
test "$(rustc --version | awk '{{print $2}}')" = 1.99.1
test -n "${{MBX_REMOTE_TOKEN_FILE:-}}"
test -f "$MBX_REMOTE_TOKEN_FILE" && test ! -L "$MBX_REMOTE_TOKEN_FILE"
test "$(stat -c %a "$MBX_REMOTE_TOKEN_FILE")" = 600
test "$(stat -c %u "$MBX_REMOTE_TOKEN_FILE")" = "$(id -u)"
test "$(sha256sum "$physical" | cut -d' ' -f1)" = 4b0c0fcfc9329a9d76fd6bdefc1ee34822580864f5f5ba2f0b47dcbbffce2eca
for name in MBX_CACHE_DIR CARGO_TARGET_DIR CARGO_BUILD_TARGET RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER; do test -z "${{!name:-}}"; done
command -v python3 >/dev/null
command -v systemd-run >/dev/null
""".format(envfile=shlex.quote(data['worker_env_path']), envhash=shlex.quote(data['worker_env_sha256']),
           bootstrap=shlex.quote(data['bootstrap_lock_path']), manifest=shlex.quote(data['toolchain_manifest_path']),
           digest=shlex.quote(data['toolchain_manifest_sha256']))
    result = run(host, probe, stdin=subprocess.DEVNULL, timeout=30)
    if result.returncode:
        fail('worker guard or physical toolchain check failed')
    # Retain only bounded non-secret identities, never arbitrary provider metadata.
    print(json.dumps({**data, 'observed_at': int(time.time()), 'provider': 'boat'}))


# This program is also sent to Boat for extraction. Bound every archive before
# extraction, reject device files, escaping paths, and links, and never overwrite a
# symlink parent. Workload outputs use regular files/directories only.
EXTRACT = '''import pathlib,sys,tarfile,tempfile,shutil
root=pathlib.Path(sys.argv[1]).expanduser();root.mkdir(parents=True,exist_ok=True)
limit=int(sys.argv[2]);count=int(sys.argv[3])
with tempfile.TemporaryFile(buffering=0) as f:
 total=0
 while True:
  b=sys.stdin.buffer.read(1048576)
  if not b:break
  total+=len(b)
  if total>limit:raise ValueError('archive byte bound exceeded')
  f.write(b)
 f.seek(0)
 with tarfile.open(fileobj=f,mode='r:*') as t:
  members=[];expanded=0
  for m in t:
   if len(members)>=count:raise ValueError('archive member bound exceeded')
   p=pathlib.PurePosixPath(m.name)
   if p.is_absolute() or '..' in p.parts or not (m.isfile() or m.isdir()):raise ValueError('unsafe archive member')
   expanded+=m.size
   if expanded>limit:raise ValueError('expanded archive bound exceeded')
   dst=root.joinpath(*p.parts)
   if any(v.is_symlink() for v in [dst,*dst.parents]):raise ValueError('symlink destination')
   members.append(m)
  t.extractall(root,members=members,filter='data')
'''


def extract(stream, destination):
    result = subprocess.run([sys.executable, '-I', '-c', EXTRACT, str(destination),
                             str(MAX_ARCHIVE), str(MAX_MEMBERS)], stdin=stream, stderr=subprocess.DEVNULL)
    if result.returncode:
        fail('archive extraction refused')


def remote_path(spec):
    # boat:bx_ID:PATH is deliberately unambiguous; ssh paths keep rsync behaviour.
    match = re.match(r'^(boat:bx_[A-Za-z0-9]+):(.*)$', spec)
    if match:
        boat_id(match[1])
        path = match[2]
        if not path or any(c in path for c in '\n\r\0'):
            fail('invalid remote path')
        return match[1], path
    return None


def sync(args):
    if len(args) < 2:
        fail('sync requires source and destination')
    options, source, destination = args[:-2], args[-2], args[-1]
    src, dst = remote_path(source), remote_path(destination)
    if not src and not dst:
        return subprocess.call(['rsync', *args])
    if src and dst:
        fail('remote-to-remote copies are unsupported')
    excluded = []
    pending = list(options)
    while pending:
        option = pending.pop(0)
        if option in ('-a', '--delete'):
            continue
        if option == '--exclude' and pending:
            excluded.append(pending.pop(0)); continue
        fail('unsupported Boat sync option: ' + option)
    if dst and not source.endswith('/') and not destination.endswith('/'):
        if excluded or not Path(source).is_file():
            fail('direct Boat file copy requires a regular file')
        return put(dst[0], source, dst[1])
    # --delete is deliberately refused for Boat: retaining old evidence is safer
    # than mirroring into an existing result tree. Gate fetch uses fresh staging.
    if '--delete' in options and dst:
        fail('Boat upload does not delete remote evidence')
    with tempfile.TemporaryFile(buffering=0) as archive:
        if src:
            host, path = src
            text = 'tar -C ' + shlex.quote(path.rstrip('/')) + ' -cf - .'
            code = receive(host, text, archive)
            if code:
                return code
            archive.seek(0)
            extract(archive, destination)
        else:
            host, path = dst
            item = Path(source)
            with tarfile.open(fileobj=archive, mode='w') as tar:
                if source.endswith('/'):
                    for child in sorted(item.iterdir()):
                        if child.name not in excluded:
                            tar.add(child, arcname=child.name, recursive=True)
                else:
                    tar.add(item, arcname=item.name)
            if archive.tell() > MAX_ARCHIVE:
                fail('archive byte bound exceeded')
            archive.seek(0)
            # Validate locally before sending; the remote validates again before
            # writing, so neither endpoint trusts tar headers from transport.
            with tempfile.TemporaryDirectory() as check:
                extract(archive, check)
            archive.seek(0)
            text = shlex.join(['python3', '-I', '-c', EXTRACT, path, str(MAX_ARCHIVE), str(MAX_MEMBERS)])
            return run(host, text, stdin=archive).returncode
    return 0


def put(host, source, destination):
    path = Path(source)
    if not path.is_file() or path.is_symlink() or path.stat().st_size > MAX_ARCHIVE:
        fail('upload must be a bounded regular file')
    with path.open('rb') as stream:
        digest = hashlib.file_digest(stream, 'sha256').hexdigest()
    target = shlex.quote(destination)
    text = ('set -e; p=' + target + '; mkdir -p "$(dirname "$p")"; '
            't=$(mktemp "${p}.XXXXXXXX"); cat > "$t"; '
            'test "$(sha256sum "$t" | cut -d\' \' -f1)" = ' + digest + '; mv "$t" "$p"')
    with path.open('rb') as stream:
        return run(host, text, stdin=stream).returncode


def get(host, source, destination):
    target = Path(destination)
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as stream:
        temporary = Path(stream.name)
        try:
            code = receive(host, 'cat ' + shlex.quote(source), stream)
            if code:
                return code
            stream.flush()
            with temporary.open('rb') as check:
                actual = hashlib.file_digest(check, 'sha256').hexdigest()
            expected = run(host, 'sha256sum ' + shlex.quote(source), capture_output=True,
                           timeout=30, check=True).stdout.decode().split()[0]
            if actual != expected:
                fail('download digest mismatch')
            temporary.replace(target)
            return 0
        finally:
            temporary.unlink(missing_ok=True)


def collect(argv, output, limit=MAX_ARCHIVE, timeout=300):
    process = subprocess.Popen(argv, stdout=subprocess.PIPE, start_new_session=True)
    finished = False
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    try:
        total = 0
        deadline = time.monotonic() + timeout
        while selector.get_map():
            left = deadline - time.monotonic()
            if left <= 0:
                fail('transfer deadline exceeded')
            for key, _ in selector.select(min(left, 1)):
                block = os.read(key.fd, min(1024**2, limit-total+1))
                if not block:
                    selector.unregister(key.fileobj); continue
                total += len(block)
                if total > limit:
                    fail('archive byte bound exceeded')
                output.write(block)
        code = process.wait(timeout=max(0.1, deadline-time.monotonic()))
        finished = True
        return code
    finally:
        selector.close()
        process.stdout.close()
        if not finished:
            try:os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:pass
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                pass
            # The leader may have exited before a descendant that ignores TERM.
            # This session contains only this invocation's local transport children.
            try:os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:pass
            process.wait()


def receive(host, text, output):
    return collect(command(host, text), output, limit=MAX_ARCHIVE)


COVERAGE = r"""import hashlib,json,subprocess,sys
repo,revision=sys.argv[1:]
p=subprocess.run(['git','-C',repo,'rev-list','--objects','--no-object-names','--missing=error',revision],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=60)
if p.returncode or len(p.stdout)>42000000:raise SystemExit(3)
ids=sorted(set(p.stdout.splitlines()))
if len(ids)>1000000 or any(len(x)!=40 for x in ids):raise SystemExit(3)
q=subprocess.run(['git','-C',repo,'cat-file','--batch-check=%(objectname) %(objecttype)'],input=b'\n'.join(ids)+b'\n',stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=60)
rows=q.stdout.splitlines()
if q.returncode or len(rows)!=len(ids) or any(row.split()[-1] not in (b'commit',b'tree',b'blob',b'tag') for row in rows):raise SystemExit(3)
print(json.dumps({'commit':revision,'objects':len(ids),'objects_sha256':hashlib.sha256(b'\n'.join(ids)+b'\n').hexdigest()}))
"""


def coverage(host, revision):
    if not revision:
        return None
    local = subprocess.run([sys.executable, '-I', '-c', COVERAGE, '.', revision],
                           capture_output=True, timeout=130)
    if local.returncode:
        fail('local source object coverage could not be established')
    expected = json.loads(local.stdout)
    text = shlex.join(['python3', '-I', '-c', COVERAGE]) + ' "$HOME/gate/repo.git" ' + shlex.quote(revision)
    with tempfile.TemporaryFile(buffering=0) as result:
        code = collect(command(host, text), result, limit=65536, timeout=130)
        if code:
            return None
        result.seek(0)
        observed = json.load(result)
    return observed if observed == expected else None


def bundle(host, full, base, record=None):
    """Reuse proven objects or send a bounded standard incremental/full bundle."""
    boat_id(host)
    if not re.fullmatch('[a-f0-9]{40}', full) or (base and not re.fullmatch('[a-f0-9]{40}', base)):
        fail('source transfer requires full commit identities')
    evidence = {'schema': 1, 'worker': host, 'source': full, 'base': base,
                'mode': None, 'bundle_sha256': None, 'prerequisite_proof': None,
                'prerequisites': [], 'status': 'checking'}
    def retain():
        if record:
            Path(record).write_text(json.dumps(evidence, indent=2)+'\n')
    retain()
    existing = coverage(host, full)
    if existing:
        evidence.update(mode='reuse', prerequisite_proof=existing, status='verified')
        retain(); return 0
    prerequisite = coverage(host, base) if base else None
    evidence.update(mode='incremental' if prerequisite else 'full-seed', prerequisite_proof=prerequisite)
    retain()
    objects = subprocess.run(['git', 'rev-parse', '--path-format=absolute', '--git-path', 'objects'],
                             capture_output=True, check=True, text=True, timeout=10).stdout.strip()
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / 'repo.git'
        subprocess.run(['git', 'init', '--bare', '-q', str(repo)], check=True, timeout=10)
        (repo/'objects/info/alternates').write_text(objects+'\n')
        ref = 'refs/heads/source'
        subprocess.run(['git', '-C', str(repo), 'update-ref', ref, full], check=True, timeout=10)
        with tempfile.TemporaryFile(buffering=0) as packet:
            argv = ['git', '-C', str(repo), '-c', 'pack.threads=1', '-c', 'pack.windowMemory=32m',
                    'bundle', 'create', '-', ref, *(['^'+base] if prerequisite else [])]
            try:
                code = collect(argv, packet)
            except (ValueError, OSError, subprocess.SubprocessError):
                evidence['status'] = 'creation-refused-or-incomplete'; retain()
                raise
            if code:
                evidence.update(status='creation-failed', exit=code); retain(); return code
            packet.seek(0)
            evidence['bundle_sha256'] = hashlib.file_digest(packet, 'sha256').hexdigest()
            packet.seek(0)
            header = []
            while True:
                line = packet.readline(4096)
                if not line or line == b'\n':break
                if len(header) >= 10000 or len(line) >= 4096:fail('bundle header bound exceeded')
                header.append(line)
            evidence['prerequisites'] = [line[1:41].decode('ascii') for line in header if line.startswith(b'-')]
            if not prerequisite and evidence['prerequisites']:
                fail('full seed unexpectedly requires prerequisite objects')
            evidence['status'] = 'transfer-requested'; retain()
            packet.seek(0)
            text = ('set -e; mkdir -p "$HOME/gate/repo.git" "$HOME/gate/bin"; '
                    'git -C "$HOME/gate/repo.git" rev-parse --is-bare-repository >/dev/null 2>&1 || '
                    'git init --bare -q "$HOME/gate/repo.git"; '
                    'f=$(mktemp "$HOME/gate/source.XXXXXXXX"); cat > "$f"; '
                    'test "$(sha256sum "$f" | cut -d\' \' -f1)" = ' + evidence['bundle_sha256'] + '; '
                    'git -C "$HOME/gate/repo.git" fetch -q "$f" refs/heads/source:refs/heads/gate-' + full[:8] +
                    '; code=$?; rm -f "$f"; exit "$code"')
            code = run(host, text, stdin=packet).returncode
            evidence['exit'] = code
            if code:
                evidence['status'] = 'uncertain-or-failed'; retain(); return code
    if not coverage(host, full):
        evidence['status'] = 'post-transfer-coverage-unproven'; retain()
        fail('worker source coverage unproven after transfer')
    evidence['status'] = 'verified'; retain()
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    ex = sub.add_parser('exec'); ex.add_argument('host'); ex.add_argument('command')
    va = sub.add_parser('validate'); va.add_argument('host')
    ec = sub.add_parser('extract-command'); ec.add_argument('destination'); ec.add_argument('--max-bytes', type=int, default=MAX_ARCHIVE)
    ad = sub.add_parser('admit'); ad.add_argument('host'); ad.add_argument('--minimum', type=int, default=300)
    sy = sub.add_parser('sync'); sy.add_argument('args', nargs=argparse.REMAINDER)
    un = sub.add_parser('unpack'); un.add_argument('destination')
    for action in ('put', 'get'):
        transfer = sub.add_parser(action); transfer.add_argument('host'); transfer.add_argument('source'); transfer.add_argument('destination')
    bu = sub.add_parser('bundle'); bu.add_argument('host'); bu.add_argument('full'); bu.add_argument('base', nargs='?', default=''); bu.add_argument('--record')
    args = parser.parse_args()
    if args.action == 'validate':
        admission(args.host); return 0
    if args.action == 'extract-command':
        if not 0 < args.max_bytes <= 64 * 1024**3:
            fail('invalid archive byte bound')
        print(shlex.join(['python3', '-I', '-c', EXTRACT, args.destination, str(args.max_bytes), str(MAX_MEMBERS)])); return 0
    if args.action == 'exec':
        argv = command(args.host, args.command)
        os.execvp(argv[0], argv)
    if args.action == 'admit':
        admit(args.host, args.minimum); return 0
    if args.action in ('put', 'get'):
        return globals()[args.action](args.host, args.source, args.destination)
    if args.action == 'unpack':
        extract(sys.stdin.buffer, args.destination); return 0
    if args.action == 'sync':
        return sync(args.args[1:] if args.args[:1] == ['--'] else args.args)
    return bundle(args.host, args.full, args.base, args.record)


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (ValueError, OSError, subprocess.SubprocessError, json.JSONDecodeError) as error:
        # No arbitrary provider output or environment is included in diagnostics.
        print('remote-executor: ' + (str(error) if isinstance(error, ValueError) else type(error).__name__), file=sys.stderr)
        sys.exit(97)
