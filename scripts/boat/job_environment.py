#!/usr/bin/env python3
"""Prepare only deterministic nonsecret job environment bytes after qualification."""
from pathlib import Path
import argparse
import hashlib
import json
import os
import shlex
import stat
import sys
import time
sys.path.insert(0, str(Path(__file__).resolve().parent))
from common import read_owned, write_new
from config_schema import validate_profile
from qualified_worker_admission import export_operation, validate_admission
import controller

SCHEMA = 'sparkles.boat.job-admission'


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def environment(profile):
    home = profile['worker_home']
    return {'PATH': ':'.join([home+'/.local/share/mbx/bin', home+'/.local/bin',
                             home+'/.cargo/bin', '/usr/local/bin', '/usr/bin', '/bin']),
            'MBX_REMOTE_TOKEN_FILE': home+'/.config/mbx/remote-token'}


def environment_body(profile):
    return ''.join(key+'='+shlex.quote(value)+'\n' for key, value in environment(profile).items()).encode()


def expected_environment(profile, qualification):
    operation = qualification['safe_owner']['operation_id']
    require(operation == qualification['local_guard']['unit'].removesuffix('.service'), 'operation identity')
    require(operation.startswith('boat-owned-warm-qual-') and all(c in 'abcdefghijklmnopqrstuvwxyz0123456789-' for c in operation), 'safe operation name')
    body = environment_body(profile)
    return {'path': profile['worker_home']+'/.local/state/sparkles-boat/job-environments/'+operation+'.env',
            'sha256': digest(body), 'bytes': len(body), 'uid': profile['worker_uid'], 'mode': 0o600}


def local_guard(unit):
    names='Id,ActiveState,MainPID,InvocationID'
    code = "import json,subprocess,sys;p=subprocess.run(sys.argv[1:],capture_output=True,timeout=8);assert p.returncode==0 and len(p.stdout)<65536;print(json.dumps(dict(x.split('=',1)for x in p.stdout.decode().splitlines()if '='in x)))"
    result = controller.bounded([sys.executable, '-I', '-B', '-c', code,
                                 'systemctl', '--user', 'show', unit, '--property='+names], 10)
    require(result['exit']==0 and isinstance(result['body'],dict), 'local guard query unavailable')
    return result['body']


def context(profile_path, qualification_path, worker, now, guard_reader=None):
    raw_profile = read_owned(Path(profile_path).absolute())
    profile = validate_profile(json.loads(raw_profile))
    require(profile['worker']==worker, 'explicit worker/profile mismatch')
    path = Path(qualification_path).absolute()
    require(path.name=='qualified-worker-admission.json', 'original qualification filename')
    raw = read_owned(path, 65536)
    qualification = json.loads(raw)
    require(qualification.get('profile_sha256')==digest(raw_profile), 'profile file changed')
    require(qualification==export_operation(path.parent, now, worker, profile), 'qualification operation changed')
    live = (local_guard if guard_reader is None else guard_reader)(qualification['local_guard']['unit'])
    validate_admission(qualification, Path('/proc/sys/kernel/random/boot_id').read_text().strip(), live, now, profile)
    require(qualification['safe_owner']['uid']==os.getuid(), 'qualification local owner mismatch')
    release = json.loads(read_owned(path.parent/'release.json'))
    require(Path(release['operation'])==path.parent, 'operation directory mismatch')
    require(release['controller_packet_manifest_sha256']==qualification['controller_packet_sha256'], 'source binding mismatch')
    controller.verify_packet(qualification['controller_packet_sha256'])
    controller.verify_cli(release)
    return profile, qualification, raw, release


# The only remote mutation is a private deterministic environment file. Token
# content is never opened. An uncertain result may be reconciled with --check.
REMOTE = r'''from pathlib import Path
import hashlib,json,os,stat,sys,tempfile
p=json.loads(sys.argv[1]);home=Path(p['home']);uid=p['uid'];expected=p['file'];body=p['body'].encode()
assert os.getuid()==uid and Path.home()==home and home.resolve()==home
assert hashlib.sha256(body).hexdigest()==expected['sha256'] and len(body)==expected['bytes']
assert set(p)=={'home','uid','file','body','check'}
token=home/'.config/mbx/remote-token';t=token.lstat()
assert stat.S_ISREG(t.st_mode) and t.st_uid==uid and t.st_nlink==1 and stat.S_IMODE(t.st_mode)==0o600
root=home/'.local/state/sparkles-boat';s=root.lstat()
assert root.resolve()==root and stat.S_ISDIR(s.st_mode) and s.st_uid==uid and not s.st_mode&0o022
directory=root/'job-environments'
if not directory.exists():
 assert not p['check'];directory.mkdir(mode=0o700)
s=directory.lstat();assert directory.resolve()==directory and stat.S_ISDIR(s.st_mode) and s.st_uid==uid and not s.st_mode&0o077
path=Path(expected['path']);assert path.parent==directory
if not path.exists():
 assert not p['check']
 fd,tmp=tempfile.mkstemp(prefix='.pending-',dir=directory)
 try:
  with os.fdopen(fd,'wb')as f:f.write(body);f.flush();os.fsync(f.fileno())
  os.link(tmp,path,follow_symlinks=False)
 finally:os.unlink(tmp)
s=path.lstat();assert stat.S_ISREG(s.st_mode) and s.st_uid==uid and s.st_nlink==1 and stat.S_IMODE(s.st_mode)==0o600 and s.st_size==len(body)
assert (s.st_dev,s.st_ino)!=(t.st_dev,t.st_ino)
fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
try:
 a=os.fstat(fd);assert (a.st_dev,a.st_ino)==(s.st_dev,s.st_ino)
 data=os.read(fd,len(body)+1);assert data==body
finally:os.close(fd)
print(json.dumps(expected))
'''


def prepare(profile_path, operation, worker, output, check=False):
    path = Path(operation).absolute()/'qualified-worker-admission.json'
    profile, qualification, raw, release = context(profile_path, path, worker, time.time())
    expected = expected_environment(profile, qualification)
    payload = {'home':profile['worker_home'], 'uid':profile['worker_uid'], 'file':expected,
               'body':environment_body(profile).decode(), 'check':check}
    from datetime import datetime
    seconds=min(30,datetime.fromisoformat(qualification['expires_at'].replace('Z','+00:00')).timestamp()-time.time()-30)
    require(seconds>0,'insufficient original lifetime for environment preparation')
    # Use the maintained qualification launcher's stdin transport convention.
    # Program and payload bytes never depend on provider argv shell joining.
    program=('import sys\nsys.argv=["job-environment",'+repr(json.dumps(payload))+']\n'+REMOTE).encode()
    result = controller.bounded([release['boat']['path'], '--no-update', 'ssh', worker, '--',
                                 '/usr/bin/python3', '-I', '-B', '-'], seconds, program)
    require(result['exit']==0 and result['body']==expected, 'job environment result uncertain; reconcile with --check')
    # Recheck the original guard, source/profile and deadline after transport.
    context(profile_path, path, worker, time.time())
    record = {'schema':SCHEMA, 'version':1, 'worker':worker,
              'qualification':{'path':str(path), 'sha256':digest(raw)}, 'runtime_env_file':expected}
    out = Path(output).absolute()
    if out.exists():
        require(json.loads(read_owned(out))==record, 'existing job admission differs')
    else:
        write_new(out.parent, out.name, record)
    return record


def load_job_admission(path, profile_path, worker, now, guard_reader=None):
    record = json.loads(read_owned(Path(path).absolute(),65536))
    require(set(record)=={'schema','version','worker','qualification','runtime_env_file'} and record['schema']==SCHEMA and type(record['version'])is int and record['version']==1, 'unknown job admission schema')
    require(record['worker']==worker and set(record['qualification'])=={'path','sha256'}, 'job admission worker/qualification')
    reference=record['qualification']
    profile, qualification, raw, release = context(profile_path, reference['path'], worker, now, guard_reader)
    require(digest(raw)==reference['sha256'], 'original qualification bytes changed')
    require(record['runtime_env_file']==expected_environment(profile,qualification), 'job environment identity changed')
    return profile, qualification, record['runtime_env_file'], release['boat']


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    for key in ('owned-worker','profile','operation','out'):parser.add_argument('--'+key,required=True)
    parser.add_argument('--check',action='store_true');args=parser.parse_args()
    try:
        prepare(args.profile,args.operation,args.owned_worker,args.out,args.check)
        print(json.dumps({'prepared':True,'worker':args.owned_worker,'admission':str(Path(args.out).absolute())}));return 0
    except Exception:
        print(json.dumps({'prepared':False,'reason':'qualification/environment unavailable or mismatched; no lifecycle retry'}));return 1
if __name__=='__main__':raise SystemExit(main())
