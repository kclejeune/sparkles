"""Synthetic owned receipts for source contracts. They confer no provider authority."""
from pathlib import Path
import hashlib
import json
import os
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'boat'))
import controls
import controller
import job_environment


def write(path, value):
    path.write_text(json.dumps(value,indent=2)+'\n');path.chmod(0o600)


def fixture(root):
    release=controls.fixture();profile=controls.profile()
    profile['worker_uid']=os.getuid()
    raw=json.dumps(profile,sort_keys=True)
    release.update(_profile_json=raw,profile_sha256=hashlib.sha256(raw.encode()).hexdigest(),
                   useful_job_ready=True,separate_Root_handoff={'job_release_sha256':'a'*64,
                   'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,
                   'finite_job_and_closure_seconds':600})
    operation=root/'operation';operation.mkdir(mode=0o700)
    release['operation']=str(operation)
    cli=root/'boat-fixture';cli.write_text('#!/bin/sh\nexit 98\n');cli.chmod(0o755)
    release['boat'].update(path=str(cli),uid=os.getuid(),bytes=cli.stat().st_size,mode=0o755,
                           sha256=hashlib.sha256(cli.read_bytes()).hexdigest())
    release['controller_packet_manifest_sha256']=hashlib.sha256(controller.ROOT.joinpath('runtime-sources.json').read_bytes()).hexdigest()
    io=controls.InertIO(release)
    def guard():
        loaded={'Id':release['guard_unit']+'.service','ActiveState':'active','MainPID':'123','InvocationID':'a'*32}
        row={'pid':123,'uid':os.getuid(),'observed_wall':io.wall(),'observed_boottime':io.mono(),
             'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'loaded':loaded}
        io.record('guard-admission.json',row);return row
    io.guard_for_admission=guard
    assert controller.drive(release,io)=='QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE'
    write(operation/'release.json',release)
    for name,value in io.records.items():write(operation/name,value)
    path=root/'profile.json';path.write_text(raw);path.chmod(0o600)
    qualified=io.records['qualified-worker-admission.json']
    original=operation/'qualified-worker-admission.json'
    job={'schema':job_environment.SCHEMA,'version':1,'worker':profile['worker'],
         'qualification':{'path':str(original),'sha256':hashlib.sha256(original.read_bytes()).hexdigest()},
         'runtime_env_file':job_environment.expected_environment(profile,qualified)}
    admission=root/'admission.json';write(admission,job)
    return {'profile':profile,'profile_path':path,'qualified':qualified,'qualified_path':original,
            'release':release,'operation':operation,'job':job,'admission':admission,
            'guard':io.records['guard-admission.json']['loaded'],'now':io.wall(),'boot_now':io.mono(),
            'boot_id':io.records['guard-admission.json']['boot_id']}
