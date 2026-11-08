"""Run inside the named 512-MiB qualification service."""
from pathlib import Path
import hashlib,json,os,stat,subprocess,sys,re,shutil,tomllib

class Refusal(Exception):pass
def need(ok,why):
    if not ok:raise Refusal(why)
def hash_bytes(data):return hashlib.sha256(data).hexdigest()
def token_metadata(token,uid):
    s=token.lstat()
    need(stat.S_ISREG(s.st_mode) and s.st_uid==uid and s.st_nlink==1 and stat.S_IMODE(s.st_mode)==0o600,'token metadata')
    return {'path':str(token),'uid':s.st_uid,'gid':s.st_gid,'mode':stat.S_IMODE(s.st_mode),'kind':'regular','content_opened_or_hashed':False,'device':s.st_dev,'inode':s.st_ino}
def safe_file(path,uid,denied,limit=256*1024*1024,hardlinks=False):
    path=Path(path);s=path.lstat()
    need((s.st_dev,s.st_ino)!=denied,'credential alias denied')
    need(stat.S_ISREG(s.st_mode) and s.st_uid==uid and not s.st_mode&0o022 and (s.st_nlink==1 or hardlinks) and s.st_size<=limit,'unsafe public file')
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        a=os.fstat(fd);need((a.st_dev,a.st_ino,a.st_mtime_ns,a.st_size)==(s.st_dev,s.st_ino,s.st_mtime_ns,s.st_size),'file replaced')
        h=hashlib.sha256();data=bytearray() if limit<=1024*1024 else None;size=0
        while True:
            part=os.read(fd,1024*1024)
            if not part:break
            size+=len(part);need(size<=limit,'public file bound');h.update(part)
            if data is not None:data.extend(part)
        z=path.lstat();need((z.st_dev,z.st_ino,z.st_mtime_ns,z.st_size)==(s.st_dev,s.st_ino,s.st_mtime_ns,s.st_size) and size==s.st_size,'public file changed')
        return {'path':str(path),'uid':uid,'mode':stat.S_IMODE(s.st_mode),'bytes':size,'sha256':h.hexdigest(),'nlink':s.st_nlink,'device':s.st_dev,'inode':s.st_ino},None if data is None else bytes(data)
    finally:os.close(fd)
def directory(path,uid):
    path=Path(path);s=path.lstat();need(path.resolve()==path and stat.S_ISDIR(s.st_mode) and s.st_uid==uid and not s.st_mode&0o022,'unsafe qualification directory')
    return {'path':str(path),'uid':uid,'mode':stat.S_IMODE(s.st_mode),'device':s.st_dev,'inode':s.st_ino}
def command(argv,seconds=15,env=None,cwd=None):
    r=subprocess.run(argv,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=seconds,env=env,cwd=cwd)
    need(r.returncode==0 and len(r.stdout)<=1024*1024,'qualification command failed')
    return r.stdout

def validate_runtime(unit,pid,cgline,maxmem,maxswap,properties,runtime_seconds=240):
    need(maxmem=='536870912' and maxswap=='0','effective metadata cap')
    need(properties.get('MemoryMax')==maxmem and properties.get('MemorySwapMax')=='0' and properties.get('KillMode')=='control-group' and properties.get('OOMPolicy')=='continue' and properties.get('MainPID')==str(pid),'loaded metadata properties')
    tokens=re.findall(r'([0-9]+)(min|h|s)',properties.get('RuntimeMaxUSec',''))
    need(tokens and ''.join(n+u for n,u in tokens)==properties.get('RuntimeMaxUSec','').replace(' ',''),'loaded finite observer clock')
    total=sum(int(n)*{'min':60,'h':3600,'s':1}[u]for n,u in tokens)
    need(total==runtime_seconds and properties.get('TimeoutStopUSec')=='10s','loaded observer clock')
    need(properties.get('ControlGroup')==cgline[3:] and re.fullmatch('[0-9a-f]{32}',properties.get('InvocationID','')),'loaded metadata identity')
def validate_pressure(events,swap):
    need(all(int(v)==0 for v in events.values()),'metadata memory pressure')
    need(swap=='0','metadata swap peak')

def probe(payload):
    uid=os.getuid();home=Path.home();need(uid==profile()['worker_uid'] and home==Path(profile()['worker_home']),'worker user')
    pins=payload['pins'];s=payload['release']['snapshot_continuity'];tag=payload['operation_tag'];unit=tag+'-metadata.service'
    need(os.uname().machine=='x86_64','architecture')
    cgline=Path('/proc/self/cgroup').read_text().strip();need(cgline.startswith('0::'),'cgroup v2')
    cg=Path('/sys/fs/cgroup')/cgline[3:].lstrip('/');need(cg.name==unit,'metadata unit cgroup')
    maxmem=(cg/'memory.max').read_text().strip();maxswap=(cg/'memory.swap.max').read_text().strip()
    need(maxmem=='536870912' and maxswap=='0','effective metadata cap')
    raw=command(['systemctl','--user','show',unit,'--property=MemoryMax,MemorySwapMax,RuntimeMaxUSec,TimeoutStopUSec,KillMode,OOMPolicy,MainPID,InvocationID,ControlGroup'])
    properties=dict(x.split('=',1) for x in raw.decode().splitlines() if '=' in x)
    validate_runtime(unit,os.getpid(),cgline,maxmem,maxswap,properties)
    token=home/'.config/mbx/remote-token';tm=token_metadata(token,uid);denied=(tm['device'],tm['inode'])
    state=home/'.local/state/sparkles-boat';new=Path(pins['installed_bundle']);old=Path(pins['retained_old_bundle'])
    directories=[directory(p,uid) for p in [home,state,new,old,Path(profile()['startup_repositories']['default']),Path(profile()['startup_repositories']['retained'])]]
    bundle_records={}
    for root,members in [(new,pins['runtime_members']),(old,pins['old_runtime_members'])]:
        for name,row in members.items():
            f,_=safe_file(root/name,uid,denied,1024*1024)
            need(f['sha256']==row['sha256'] and f['mode']==row['embedded_mode'],'setup member mismatch')
            if 'bytes' in row:need(f['bytes']==row['bytes'],'setup member length')
            bundle_records[str(root/name)]=f
    before,body=safe_file(state/'toolchain.json',uid,denied,1024*1024)
    need(before['sha256']==pins['saved_toolchain_manifest_sha256'],'saved manifest changed')
    saved=json.loads(body);need(saved['format']==2 and saved['mise']=='2026.8.16' and saved['rust']=='1.99.1' and saved['bootstrap_source_before']==saved['bootstrap_source_after'],'saved toolchain schema')
    need(s['default_source']==saved['bootstrap_source_after'],'default source continuity not bound')
    public_config,config_bytes=safe_file(home/'.config/mbx/config.toml',uid,denied,65536)
    config=tomllib.loads(config_bytes.decode());need(config.get('remote')==pins['mbx_remote_config'],'DEV read-only cache config')
    need(not os.environ.get('MBX_CACHE_DIR') and not os.environ.get('CARGO_TARGET_DIR'),'private cache/target override')
    env=dict(os.environ)
    for key in ['MISE_SHELL','MISE_ENV','MISE_ENV_FILE','MISE_CONFIG_DIR']:env.pop(key,None)
    env.update({'PATH':str(home/'.local/share/mbx/bin')+':'+str(home/'.local/bin')+':'+str(home/'.cargo/bin')+':/usr/local/bin:/usr/bin:/bin','MISE_CONFIG_FILE':str(state/'tools.toml'),'RUSTUP_TOOLCHAIN':'1.99.1','LC_ALL':'C.UTF-8','MBX_REMOTE_TOKEN_FILE':str(token)})
    need(shutil.which('cargo',path=env['PATH'])==str(home/'.local/share/mbx/bin/cargo'),'Cargo resolution')
    helper=str(new/'toolchain-state.py');sources={};checks={}
    for repo,expected in [(Path(profile()['startup_repositories']['default']),s['default_source']),(Path(profile()['startup_repositories']['retained']),s['retained_source'])]:
        first=json.loads(command(['/usr/bin/python3',helper,'source',str(repo)],30,env,repo))
        need(first==expected,'existing source drift')
        check=json.loads(command(['/bin/bash',str(new/'repository-start.sh'),str(repo)],55,env,repo))
        need(check['native_started'] is False and check['status']=='tools-ready-workload-admission-still-required' and check['uid']==uid,'checker outcome')
        last=json.loads(command(['/usr/bin/python3',helper,'source',str(repo)],30,env,repo));need(last==first,'existing source changed during qualification')
        sources[str(repo)]={'before':first,'after':last};checks[str(repo)]={'status':check['status'],'installed_bindings':check['installed_bindings']}
    after,_=safe_file(state/'toolchain.json',uid,denied,1024*1024);need(after==before,'manifest changed during checker')
    # Check parent capacity, excluding the deliberately smaller metadata service.
    ancestors=[];ancestor=cg.parent
    while ancestor.is_relative_to(Path('/sys/fs/cgroup')):
        row={'path':str(ancestor)}
        for name in ['memory.max','memory.current','memory.swap.max']:
            if (ancestor/name).exists():row[name]=(ancestor/name).read_text().strip()
        ancestors.append(row)
        if ancestor==Path('/sys/fs/cgroup'):break
        ancestor=ancestor.parent
    memory={}
    for line in Path('/proc/meminfo').read_text().splitlines():
        k,_,v=line.partition(':')
        if k in ['MemTotal','MemAvailable','SwapTotal','SwapFree']:memory[k]=int(v.split()[0])*1024
    need(memory['MemAvailable']>=14495514624,'insufficient headroom for proposed12GiB job')
    need(all(row.get('memory.max','max')=='max' or int(row['memory.max'])-int(row.get('memory.current',0))>=14495514624 for row in ancestors),'ancestor memory headroom')
    disk={}
    for root in [home,home/'.local/share/mbx',Path(profile()['startup_repositories']['default']),Path(profile()['startup_repositories']['retained'])]:
        need(root.exists(),'normal shared MBX/source path absent');fs=os.statvfs(root);need(fs.f_bavail*fs.f_frsize>=40*2**30 and fs.f_favail>=114060,'disk/inode headroom')
        disk[str(root)]={'free_bytes':fs.f_bavail*fs.f_frsize,'free_inodes':fs.f_favail,'device':root.stat().st_dev}
    heavy=[]
    for item in Path('/proc').iterdir():
        if not item.name.isdigit():continue
        try:
            comm=(item/'comm').read_text().strip()
            if comm in ['cargo','rustc','mbx','sparkles','sparkles-server','ninja','cc1','cc1plus']:heavy.append({'pid':int(item.name),'comm':comm})
        except (FileNotFoundError,ProcessLookupError,PermissionError):continue
    need(not heavy,'unexpected native/heavy process')
    need(s.get('dev_read_only_scope') is True,'token scope receipt')
    events=dict(line.split() for line in (cg/'memory.events').read_text().splitlines())
    validate_pressure(events,(cg/'memory.swap.peak').read_text().strip())
    return {'memory_events':events,'memory_peak':(cg/'memory.peak').read_text().strip(),'qualified':True,'engine_or_job_started':False,'memory_max':maxmem,'swap_max':maxswap,'manifest_unchanged':True,'saved_manifest_sha256':before['sha256'],'directories':directories,'bundle_records':bundle_records,'source_observations':sources,'checker_results':checks,'token_metadata':tm,'public_cache_config':public_config,'shared_cache_and_managed_targets_preserved':True,'private_cache_or_target_created':False,'memory_bytes':memory,'ancestor_limits':ancestors,'disk':disk,'loaded_properties':properties,'native_heavy_processes':heavy,'lease_basis':'Root previous native/lease closure + exclusive worker + no lease-acquiring operation in this controller; not a new MBX lease grant','private_credential_snapshot':True,'fresh_job_admission_still_required':True}
if __name__=='__main__':
    try:
        result=probe(json.loads(sys.argv[1]));print(json.dumps(result));raise SystemExit(0)
    except Exception:
        print(json.dumps({'qualified':False,'engine_or_job_started':False,'failure':'qualification refused; no arbitrary stderr or credential content retained'}));raise SystemExit(1)
