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
CLOSURE_RESERVE=30

def boottime():return time.clock_gettime(time.CLOCK_BOOTTIME)
def current_boot_id():return Path('/proc/sys/kernel/random/boot_id').read_text().strip()

def original_ends(intent,guard,handoff):
    """Return the original (wall, BOOTTIME) end pair from the protected receipts.

    The wall end is the qualification's expires_at. The BOOTTIME end is the same
    instant on the guard's boot, taken through the guard's paired observation and
    never later than the intent's own BOOTTIME deadline.
    """
    wall=min(intent['deadline_wall'],handoff['deadline_wall'],guard['observed_wall']+intent['deadline_mono']-guard['observed_boottime'])
    boot=min(intent['deadline_mono'],guard['observed_boottime']+wall-guard['observed_wall'])
    return wall,boot

# A lifetime extension is a separate receipted Root act. Each receipt sets a new
# end pair from its own observation, made on the guard's boot before the previous
# end, and no receipt may reach past MAX_LIFETIME after the controller intent.
EXTENSION_SCHEMA='sparkles.boat.lifetime-extension'
EXTENSION_MAX=10500
MAX_LIFETIME=43200
PROVIDER_MARGIN=300
STOP_MARGIN=200
EXTENSION_KEYS={'schema','version','sequence','worker','operation_id','guard','observed_wall','observed_boottime','published_wall','published_boottime','seconds','wall_end','boot_end','provider','adoption','reason'}
ADOPTION_KEYS={'worker_boot_id','local_boot_id','guard_pid','environment_version','environment_version_id','hook_sha256','environment_fact_sha256'}
UUID='[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}'

def finite(value):return type(value)in(int,float)and value==value and abs(value)!=float('inf')

def extension_names(names):
    """Sorted receipt sequence numbers. Attempt records are evidence and confer nothing."""
    numbers=[];accepted=[]
    for name in names:
        if not name.startswith('lifetime-extension-')or re.fullmatch('lifetime-extension-attempt-[0-9a-f]{32}(-failed)?[.]json',name):continue
        match=re.fullmatch('lifetime-extension-([0-9]{4})(-accepted)?[.]json',name);require(match is not None,'malformed lifetime extension name')
        if match[2]:accepted.append(int(match[1]))
        else:numbers.append(int(match[1]))
    numbers.sort();accepted.sort();require(numbers==list(range(1,len(numbers)+1)),'lifetime extension sequence gap')
    require(accepted==list(range(1,len(accepted)+1))and len(accepted)<=len(numbers),'lifetime extension acceptance without its receipt')
    return numbers

def printable(text):return isinstance(text,str)and 0<len(text)<=200 and all(' '<=c<='~'for c in text)

def iso_unix(value):
    require(isinstance(value,str),'lifetime extension provider time')
    try:return datetime.fromisoformat(value.replace('Z','+00:00')).timestamp()
    except ValueError:raise ValueError('lifetime extension provider time')from None

ACCEPTANCE_SCHEMA='sparkles.boat.lifetime-extension-acceptance'

def receipt_digest(e):return hashlib.sha256(json.dumps(e,sort_keys=True,separators=(',',':')).encode()).hexdigest()

def acceptance_record(number,e,identity,now_wall,now_boot):
    return {'schema':ACCEPTANCE_SCHEMA,'version':1,'sequence':number,'receipt_sha256':receipt_digest(e),
            'guard':{'boot_id':identity['boot_id'],'invocation_id':identity['invocation_id']},
            'accepted_wall':now_wall,'accepted_boottime':now_boot}

def apply_extensions(names,read,intent,wall,boot,identity,accept=None):
    """Return the end pair after the guard-accepted prefix of valid receipts.

    Each act is observed after the previous act was published and published
    before the previous end with the closure reserve left on both clocks. A
    receipt grants time only when the guard accepted it, by its own clocks,
    before the previous end. The guard passes ``accept``, which records that
    acceptance or returns None, and every other reader only checks the record.
    A malformed receipt or acceptance refuses the whole chain.
    """
    numbers=extension_names(names);after=identity['handoff_boottime'];granted=0;open_chain=True
    for number in numbers:
        e=read('lifetime-extension-%04d.json'%number)
        require(isinstance(e,dict)and e.get('schema')==EXTENSION_SCHEMA and type(e.get('version'))is int and e['version']==1,'unknown lifetime extension schema')
        require(set(e)==EXTENSION_KEYS,'lifetime extension fields')
        require(type(e['sequence'])is int and e['sequence']==number and e['worker']==identity['worker'] and e['operation_id']==identity['operation_id'],'lifetime extension owner')
        require(e['guard']=={'boot_id':identity['boot_id'],'invocation_id':identity['invocation_id']},'lifetime extension guard identity')
        require(all(finite(e[k])for k in('observed_wall','observed_boottime','published_wall','published_boottime','wall_end','boot_end'))and type(e['seconds'])is int and 0<e['seconds']<=EXTENSION_MAX,'lifetime extension values')
        require(after<=e['observed_boottime']<=e['published_boottime'] and e['observed_wall']<=e['published_wall'],'lifetime extension chronology')
        require(e['published_wall']+CLOSURE_RESERVE<wall and e['published_boottime']+CLOSURE_RESERVE<boot,'lifetime extension recorded after the previous end')
        require(printable(e['reason']),'lifetime extension reason')
        require(e['wall_end']==e['observed_wall']+e['seconds'] and e['boot_end']==e['observed_boottime']+e['seconds'],'lifetime extension end')
        require(e['wall_end']>=wall and e['boot_end']>=boot,'lifetime extension shortens')
        require(e['wall_end']<=intent['wall']+MAX_LIFETIME and e['boot_end']<=intent['mono']+MAX_LIFETIME,'lifetime extension beyond the total lifetime cap')
        provider=e['provider']
        require(isinstance(provider,dict)and set(provider)=={'ttl_seconds','archive_after','archive_after_unix'}and provider['ttl_seconds']==e['seconds']+PROVIDER_MARGIN,'lifetime extension provider request')
        require(finite(provider['archive_after_unix'])and abs(iso_unix(provider['archive_after'])-provider['archive_after_unix'])<0.001 and provider['archive_after_unix']>=e['wall_end']+STOP_MARGIN,'lifetime extension provider confirmation')
        adoption=e['adoption'];environment=identity['environment']
        require(isinstance(adoption,dict)and set(adoption)==ADOPTION_KEYS and identity['worker_boot_id']is not None and adoption['worker_boot_id']==identity['worker_boot_id'],'lifetime extension worker boot')
        require(adoption['local_boot_id']==identity['boot_id'] and type(adoption['guard_pid'])is int and adoption['guard_pid']==identity['guard_pid'],'lifetime extension local guard adoption')
        require(adoption['environment_version']==environment['version'] and adoption['environment_version_id']==environment['version_id'] and adoption['hook_sha256']==environment['hook_sha256'] and hexsha(adoption['environment_fact_sha256']),'lifetime extension environment adoption')
        if not open_chain:continue
        name='lifetime-extension-%04d-accepted.json'%number
        if name in names:
            a=read(name)
            require(isinstance(a,dict)and a.get('schema')==ACCEPTANCE_SCHEMA and type(a.get('version'))is int and a['version']==1,'unknown lifetime extension acceptance schema')
            require(set(a)=={'schema','version','sequence','receipt_sha256','guard','accepted_wall','accepted_boottime'}and a['sequence']==number and a['receipt_sha256']==receipt_digest(e) and a['guard']==e['guard'],'lifetime extension acceptance identity')
            require(finite(a['accepted_wall'])and finite(a['accepted_boottime'])and a['accepted_boottime']>=e['published_boottime'] and a['accepted_wall']<wall and a['accepted_boottime']<boot,'lifetime extension accepted after the previous end')
        elif accept is None or accept(number,e,wall,boot) is None:
            open_chain=False;continue
        wall,boot,after=e['wall_end'],e['boot_end'],e['published_boottime'];granted+=1
    return wall,boot,granted

def operation_identity(intent,guard,qualification,operation_id,profile):
    worker_boot=qualification.get('worker_boot_id');env=profile['future_environment']
    return {'worker':intent['worker'],'operation_id':operation_id,'boot_id':guard['boot_id'],
            'invocation_id':guard['loaded']['InvocationID'],'guard_pid':guard['pid'],'handoff_boottime':guard['observed_boottime'],
            'environment':{'version':env['version'],'version_id':env['version_id'],'hook_sha256':env['hook_sha256']},
            'worker_boot_id':worker_boot if isinstance(worker_boot,str)and re.fullmatch(UUID,worker_boot)else None}

def extended_ends(root,intent,guard,handoff,operation_id):
    """The original end pair with the operation's lifetime extensions applied."""
    wall,boot=original_ends(intent,guard,handoff);names=os.listdir(root)
    if not extension_names(names):return wall,boot,0
    profile=release_profile(owned_json(root/'release.json'))
    return apply_extensions(names,lambda n:owned_json(root/n),intent,wall,boot,operation_identity(intent,guard,owned_json(root/'qualification.json'),operation_id,profile))

def operation_lifetime(operation,qualification):
    """Recover the original end pair, tied to the qualification's boot and guard invocation."""
    root=Path(operation).absolute();require(root.resolve()==root,'canonical operation')
    intent,guard,handoff=(owned_json(root/n)for n in('controller-intent.json','guard-admission.json','root-handoff.json'))
    for value in(intent['deadline_wall'],intent['deadline_mono'],handoff['deadline_wall'],guard['observed_wall'],guard['observed_boottime']):
        require(type(value)in(int,float)and value==value and abs(value)!=float('inf'),'finite original clock receipts')
    wall,boot=original_ends(intent,guard,handoff);local=qualification['local_guard']
    require(datetime.fromtimestamp(wall,timezone.utc).isoformat().replace('+00:00','Z')==qualification['expires_at'],'original clock receipts differ from qualification')
    require(guard.get('boot_id')==local['boot_id']and guard.get('loaded',{}).get('InvocationID')==local['invocation_id'],'original clock boot/guard identity')
    wall,boot,_=extended_ends(root,intent,guard,handoff,qualification['safe_owner']['operation_id'])
    return {'wall_end':wall,'boot_end':boot,'boot_id':local['boot_id'],'invocation_id':local['invocation_id']}

def lifetime_left(lifetime,now_wall,now_boot=None,boot_id=None):
    """Seconds of original lifetime left, the earlier of the wall and BOOTTIME ends."""
    require((current_boot_id()if boot_id is None else boot_id)==lifetime['boot_id'],'local boot changed since qualification')
    now_boot=boottime()if now_boot is None else now_boot
    return min(lifetime['wall_end']-now_wall,lifetime['boot_end']-now_boot)

def make_admission(release,intent,qualification,startup,guard,handoff,now,horizon=None):
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
    # expires_at stays the original end. A receipted extension moves only the horizon.
    expires,_=original_ends(intent,guard,handoff);require(now<(expires if horizon is None else horizon) and expires<=intent['wall']+10500,'earlier effective deadline')
    manifest=qualification.get('actual_toolchain_manifest',{});require(manifest.get('path')==home+'/.local/state/sparkles-boat/toolchain.json' and manifest.get('sha256')==p['saved_toolchain_manifest_sha256'],'actual manifest pin')
    lock=qualification.get('bootstrap_lock',{});require(lock.get('path')==home+'/.local/state/sparkles-boat/bootstrap.lock' and lock.get('held_during_checkpoint')is True,'actual bootstrap checkpoint')
    require(qualification.get('runtime_env_file')is None,'no runtime env file created/adopted by this exporter')
    return {'schema':'sparkles.boat.qualified-worker-admission','version':1,'worker':worker,'profile_sha256':release['profile_sha256'],'profile_review_sha256':p['profile_review_sha256'],'profile_canonical_sha256':hashlib.sha256(json.dumps(p,sort_keys=True,separators=(',',':')).encode()).hexdigest(),'environment':{'name':env['name'],'version':env['version'],'version_id':env['version_id']},'expires_at':datetime.fromtimestamp(expires,timezone.utc).isoformat().replace('+00:00','Z'),'safe_owner':{'uid':guard['uid'],'operation_id':release['guard_unit']},'local_guard':{'unit':unit,'boot_id':guard['boot_id'],'invocation_id':loaded['InvocationID']},'toolchain_manifest':manifest,'bootstrap_lock_path':lock['path'],'runtime_env_file':None,'job_release_sha256':job['job_release_sha256'],'job_review_sha256':job['different_job_review_sha256'],'controller_packet_sha256':release['controller_packet_manifest_sha256'],'controller_review_sha256':release['different_controller_review_sha256'],'guard_ownership':'Root retains LOCAL guard; consumer creates no remote guard/lifecycle; matching root-job-closed required before normal stop'}

def validate_admission(admission,local_boot_id,live_guard,now,expected_profile,wall_end=None):
    p=validate_profile(expected_profile);worker=p['worker'];env=p['future_environment'];home=p['worker_home']
    require(admission.get('schema')=='sparkles.boat.qualified-worker-admission' and admission.get('version')==1,'unknown admission schema')
    require(admission.get('worker')==worker and admission.get('environment')=={'name':env['name'],'version':env['version'],'version_id':env['version_id']},'worker/environment drift')
    require(admission.get('profile_review_sha256')==p['profile_review_sha256']and admission.get('profile_canonical_sha256')==hashlib.sha256(json.dumps(p,sort_keys=True,separators=(',',':')).encode()).hexdigest()and hexsha(admission.get('profile_sha256')),'profile admission identity')
    guard=admission['local_guard'];require(guard['unit'].endswith('.service') and guard['unit'].startswith('boat-owned-warm-qual-'),'fully suffixed LOCAL guard')
    require(guard['boot_id']==local_boot_id and live_guard.get('Id')==guard['unit'] and live_guard.get('InvocationID')==guard['invocation_id'] and live_guard.get('ActiveState')=='active' and int(live_guard.get('MainPID','0'))>0,'LOCAL guard not live/matching')
    require(now<(datetime.fromisoformat(admission['expires_at'].replace('Z','+00:00')).timestamp() if wall_end is None else wall_end),'admission expired')
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
    horizon,_,_=extended_ends(root,values[1],values[4],values[5],values[0]['guard_unit'])
    return make_admission(*values,now,horizon)
def main():
    parser=argparse.ArgumentParser(description='Export only an already qualified, explicitly handed-off worker; no provider/job/manager action.')
    parser.add_argument('--operation',required=True);parser.add_argument('--profile',required=True);parser.add_argument('--owned-worker',required=True);args=parser.parse_args()
    try:print(json.dumps(export_operation(args.operation,time.time(),args.owned_worker,owned_json(Path(args.profile).absolute())),sort_keys=True));return 0
    except Exception:print(json.dumps({'admitted':False,'reason':'missing/mismatched/expired qualification or explicit Root handoff'}));return 1
if __name__=='__main__':raise SystemExit(main())
