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


WORKER_BOOT_ID='87654321-4321-4321-4321-210987654321'


class ExtendIO(controls.DiskIO):
    """The fixture's operation on disk with an inert provider that only answers extend."""
    names=controller.RealIO.names

    def __init__(self,f,wall=None,boot=None):
        super().__init__(f['release'],f['operation']);self.f=f;self.started=True
        self.w=f['now'] if wall is None else wall;self.t=f['boot_now'] if boot is None else boot
        self.archive=None;self.worker_boot=WORKER_BOOT_ID;self.extend_exit=0;self.archive_shortfall=0
        self.live=dict(f['guard']);self.local_boot=f['boot_id']
        self.env_version=None;self.shows=0;self.show_delay=0

    def show(self,unit):
        assert unit==self.r['guard_unit']+'.service';self.shows+=1
        if self.show_delay and self.shows>1:self.t+=self.show_delay;self.w+=self.show_delay
        return dict(self.live)

    def boat(self,args,*a,**k):
        self.calls.append(list(args))
        if args[0]=='extend' and args[1]==controller.worker_id() and args[2]=='--ttl':
            if self.extend_exit==0:self.archive=self.w+int(args[3])-self.archive_shortfall
            return {'exit':self.extend_exit,'error':None,'body':{'id':controller.worker_id()}}
        raise AssertionError('unexpected provider call')

    def info(self):
        row=controls.info(self.r,True)
        if self.archive is not None:row['archiveAfter']=controls.iso(self.archive)
        if self.env_version is not None:row['environmentVersion']=self.env_version
        return row

    def worker_boot_id(self):return self.worker_boot
    def local_boot_id(self):return self.local_boot


def environment_fact(f,path,observed,**changed):
    """A Root-verified latest environment observation like the post-resume one."""
    p=f['profile']['future_environment']
    row={'observed_unix':observed,'verified_by_Root':True,'receipt_sha256':'c'*64,'id':p['id'],
         'latestVersionNumber':p['version'],'latestVersionId':f['release']['environment']['latest_version_id'],
         'hook_sha256':p['hook_sha256'],'hook_bytes':p['hook_bytes'],'setupBlocking':True}
    row.update(changed);write(path,row);return path
