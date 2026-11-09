"""Export and validate an explicitly qualified owned worker.

The lifecycle operation retains its original receipts. This module starts no
provider lifecycle, remote guard or source job.
"""
from pathlib import Path
from datetime import datetime,timezone
import argparse,hashlib,json,os,re,stat,time
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
from config_schema import validate_profile

def require(ok,message):
    if not ok:raise ValueError(message)
def hexsha(s):return isinstance(s,str)and re.fullmatch('[a-f0-9]{64}',s)is not None
def release_profile(release):
    raw=release.get('_profile_json');require(isinstance(raw,str)and hashlib.sha256(raw.encode()).hexdigest()==release.get('profile_sha256'),'explicit profile hash')
    p=validate_profile(json.loads(raw));require(p['worker']==release.get('worker')and p['profile_review_sha256']==release.get('profile_review_sha256'),'profile/release review')
    return p
def make_admission(release,intent,qualification,startup,guard,handoff,now):
    p=release_profile(release);worker=p['worker'];env=p['future_environment'];home=p['worker_home']
    require(release.get('authorized_by_Root')is True and release.get('worker')==worker and intent.get('worker')==worker and handoff.get('worker')==worker,'exact qualified worker/Root operation')
    require(release.get('useful_job_ready')is True and handoff.get('job_started_by_controller')is False,'explicit Root handoff required')
    job=handoff.get('separate_job_release',{});require(job==release.get('separate_Root_handoff') and hexsha(job.get('job_release_sha256')) and hexsha(job.get('different_job_review_sha256')) and job.get('Root_owns_job_and_deadline_closure')is True,'reviewed handoff binding')
    require(qualification.get('qualified')is True and qualification.get('metadata_closed')is True and qualification.get('startup_closed')is True and startup.get('startup_closed')is True,'actual qualification/closure required')
    require(qualification.get('engine_or_job_started')is False,'controller scope')
    unit=release['guard_unit']+'.service';loaded=guard.get('loaded',{})
    require(loaded.get('Id')==unit and loaded.get('ActiveState')=='active' and int(loaded.get('MainPID','0'))==guard.get('pid'),'local guard binding')
    require(re.fullmatch('[0-9a-f]{32}',loaded.get('InvocationID',''))is not None and re.fullmatch('[0-9a-f-]{36}',guard.get('boot_id',''))is not None,'local guard invocation/boot identity')
    require(type(guard.get('uid'))is int and guard['uid']>=0,'safe local owner')
    expires=min(intent['deadline_wall'],handoff['deadline_wall'],guard['observed_wall']+intent['deadline_mono']-guard['observed_boottime']);require(now<expires and expires<=intent['wall']+10500,'earlier effective deadline')
    manifest=qualification.get('actual_toolchain_manifest',{});require(manifest.get('path')==home+'/.local/state/sparkles-boat/toolchain.json' and manifest.get('sha256')==p['saved_toolchain_manifest_sha256'],'actual manifest pin')
    lock=qualification.get('bootstrap_lock',{});require(lock.get('path')==home+'/.local/state/sparkles-boat/bootstrap.lock' and lock.get('held_during_checkpoint')is True,'actual bootstrap checkpoint')
    require(qualification.get('runtime_env_file')is None,'no runtime env file created/adopted by this exporter')
    return {'schema':'sparkles.boat.qualified-worker-admission','version':1,'worker':worker,'profile_sha256':release['profile_sha256'],'profile_review_sha256':p['profile_review_sha256'],'profile_canonical_sha256':hashlib.sha256(json.dumps(p,sort_keys=True,separators=(',',':')).encode()).hexdigest(),'environment':{'name':env['name'],'version':env['version'],'version_id':env['version_id']},'expires_at':datetime.fromtimestamp(expires,timezone.utc).isoformat().replace('+00:00','Z'),'safe_owner':{'uid':guard['uid'],'operation_id':release['guard_unit']},'local_guard':{'unit':unit,'boot_id':guard['boot_id'],'invocation_id':loaded['InvocationID']},'toolchain_manifest':manifest,'bootstrap_lock_path':lock['path'],'runtime_env_file':None,'job_release_sha256':job['job_release_sha256'],'job_review_sha256':job['different_job_review_sha256'],'controller_packet_sha256':release['controller_packet_manifest_sha256'],'controller_review_sha256':release['different_controller_review_sha256'],'guard_ownership':'Root retains LOCAL guard; consumer creates no remote guard/lifecycle; matching root-job-closed required before normal stop'}

def validate_admission(admission,local_boot_id,live_guard,now,expected_profile):
    p=validate_profile(expected_profile);worker=p['worker'];env=p['future_environment'];home=p['worker_home']
    require(admission.get('schema')=='sparkles.boat.qualified-worker-admission' and admission.get('version')==1,'unknown admission schema')
    require(admission.get('worker')==worker and admission.get('environment')=={'name':env['name'],'version':env['version'],'version_id':env['version_id']},'worker/environment drift')
    require(admission.get('profile_review_sha256')==p['profile_review_sha256']and admission.get('profile_canonical_sha256')==hashlib.sha256(json.dumps(p,sort_keys=True,separators=(',',':')).encode()).hexdigest()and hexsha(admission.get('profile_sha256')),'profile admission identity')
    guard=admission['local_guard'];require(guard['unit'].endswith('.service') and guard['unit'].startswith('boat-owned-warm-qual-'),'fully suffixed LOCAL guard')
    require(guard['boot_id']==local_boot_id and live_guard.get('Id')==guard['unit'] and live_guard.get('InvocationID')==guard['invocation_id'] and live_guard.get('ActiveState')=='active' and int(live_guard.get('MainPID','0'))>0,'LOCAL guard not live/matching')
    require(now<datetime.fromisoformat(admission['expires_at'].replace('Z','+00:00')).timestamp(),'admission expired')
    require(admission.get('runtime_env_file')is None,'env file has not been created/validated by this source')
    require(admission['toolchain_manifest']=={'path':home+'/.local/state/sparkles-boat/toolchain.json','sha256':p['saved_toolchain_manifest_sha256']} and admission['bootstrap_lock_path']==home+'/.local/state/sparkles-boat/bootstrap.lock','tool/lock pins')
    return admission

def owned_json(path):
    path=Path(path);require(path.resolve()==path,'canonical receipt');fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        s=os.fstat(fd);require(stat.S_ISREG(s.st_mode)and s.st_uid==os.getuid()and s.st_nlink==1 and not s.st_mode&0o022 and s.st_size<=1048576,'owned singleton receipt')
        data=os.read(fd,1048577);require(len(data)==s.st_size,'receipt complete');return json.loads(data)
    finally:os.close(fd)
def export_operation(operation,now,owned_worker,expected_profile):
    root=Path(operation).absolute();require(root.resolve()==root,'canonical operation')
    names=['release.json','controller-intent.json','qualification.json','startup-closed.json','guard-admission.json','root-handoff.json']
    values=[owned_json(root/n)for n in names];p=release_profile(values[0]);require(p==validate_profile(expected_profile)and owned_worker==p['worker'],'explicit operation/profile/worker binding')
    return make_admission(*values,now)
def main():
    parser=argparse.ArgumentParser(description='Export only an already qualified, explicitly handed-off worker; no provider/job/manager action.')
    parser.add_argument('--operation',required=True);parser.add_argument('--profile',required=True);parser.add_argument('--owned-worker',required=True);args=parser.parse_args()
    try:print(json.dumps(export_operation(args.operation,time.time(),args.owned_worker,owned_json(Path(args.profile).absolute())),sort_keys=True));return 0
    except Exception:print(json.dumps({'admitted':False,'reason':'missing/mismatched/expired qualification or explicit Root handoff'}));return 1
if __name__=='__main__':raise SystemExit(main())
