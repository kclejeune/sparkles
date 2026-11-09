"""Run inside the named 512-MiB qualification service."""
from pathlib import Path
import hashlib,itertools,json,os,stat,subprocess,sys,re,shutil,tomllib

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

# The same credential-like stems as the diagnostic's untracked list.
STATUS_CREDENTIAL_STEMS=('credential','secret','token','passw','apikey','api-key','api_key','private','netrc','npmrc','pypirc','pgpass',
                         'id_rsa','id_dsa','id_ecdsa','id_ed25519','.pem','.key','.p12','.pfx','.jks','keystore','.kdbx','.gpg','.asc','.ovpn',
                         '.env','.ssh','.gnupg','.aws','.docker','.kube')
def status_credential_like(name):
    return any(part and any(stem in part.lower() for stem in STATUS_CREDENTIAL_STEMS) for part in name.split('/'))
EMPTY_SHA256=hash_bytes(b'')
STATUS_INDEX_MAX=20
def status_lines(data):
    """The (XY, path) pairs of `git status --porcelain=v1 -z`; a rename's source is skipped."""
    parts=data.split(b'\0');out=[];i=0
    while i<len(parts):
        item=parts[i];i+=1
        if not item:continue
        need(len(item)>3 and item[2:3]==b' ','status line shape')
        xy=item[:2].decode('ascii','replace');out.append((xy,item[3:]))
        if xy[0] in 'RC':i+=1
    return out
def index_records(repo,paths,env):
    """The size and mode each index entry of ``paths`` records, from `git ls-files --debug`."""
    if not paths:return {}
    data=command(['git','-C',str(repo),'-c','core.fsmonitor=false','--no-optional-locks','--literal-pathspecs','ls-files','--stage','--debug','-z','--']+[p.decode('utf-8','surrogateescape') for p in paths],15,env,repo)
    out={}
    for m in re.finditer(rb'([0-7]{6}) [0-9a-f]{40,64} [0-3]\t([^\0]*)\0((?:  [^\n]*\n)*)',data):
        size=re.search(rb'size: ([0-9]+)',m[3])
        out[m[2]]={'index_mode':int(m[1],8),'index_bytes':int(size[1]) if size else None}
    return out
def link_matches_head(repo,raw,full,env):
    """Whether a symbolic link's raw target is the blob HEAD records for it, as a link."""
    tree=command(['git','-C',str(repo),'--literal-pathspecs','ls-tree','-z','HEAD','--',raw.decode('utf-8','surrogateescape')],15,env,repo)
    m=re.fullmatch(rb'120000 blob ([0-9a-f]{40,64})\t'+re.escape(raw)+rb'\0',tree)
    if m is None:return False
    target=os.readlink(full)
    r=subprocess.run(['git','-C',str(repo),'hash-object','--no-filters','--stdin'],input=target,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=15,env=env,cwd=repo)
    return r.returncode==0 and r.stdout.strip()==m[1]
def status_record(repo,env):
    """The checkout's status lines, bounded and credential-free, with the hash of the raw listing."""
    row={'sha256':None,'total':0,'omitted_credential_like':0,'truncated':False,'stat_only':False,'entries':[]}
    try:
        data=command(['git','-C',str(repo),'-c','core.fsmonitor=false','-c','core.untrackedCache=false','--no-optional-locks','status','--porcelain=v1','-z'],30,env,repo)
        row['sha256']=hash_bytes(data);lines=status_lines(data);row['total']=len(lines)
        kept=[]
        for xy,raw in lines:
            name=raw.decode('utf-8','replace')
            if status_credential_like(name):row['omitted_credential_like']+=1;continue
            if len(kept)>=STATUS_MAX:row['truncated']=True;continue
            kept.append((xy,raw,name))
        tracked=[raw for xy,raw,_ in kept if xy not in ('??','!!')][:STATUS_INDEX_MAX]
        index=index_records(repo,tracked,env)
        for xy,raw,name in kept:
            entry={'xy':xy,'path':name}
            full=os.path.join(os.fsencode(str(repo)),raw)
            try:
                st=os.lstat(full)
                entry.update(kind='directory' if stat.S_ISDIR(st.st_mode) else ('file' if stat.S_ISREG(st.st_mode) else ('link' if stat.S_ISLNK(st.st_mode) else 'other')),bytes=st.st_size,mode=st.st_mode)
            except FileNotFoundError:entry['kind']='absent'
            entry.update(index.get(raw,{}))
            if entry.get('kind')=='link' and raw in index:entry['link_matches_head']=link_matches_head(repo,raw,full,env)
            row['entries'].append(entry)
        row['_lines']=lines
    except Exception as error:row['unread']=reason_code(error)
    return row
def stat_only_status(recorded,observed,status):
    """Whether a status that differs from a clean recording shows only stale index stat data.

    Git reports ` M` without reading content when a path's size differs from the size its
    index entry records, as a restored symbolic link can, and no refresh clears it. Only
    that case is accepted. The recording was clean, every other field is unchanged, the
    tracked diff against HEAD is empty, the probe's own listing is the one the helper
    hashed, and every line is a worktree-only ` M` of a path that is a symbolic link in
    the worktree, the index and HEAD, whose lstat size differs from its index size and
    whose raw target is the blob HEAD records. A regular file is never accepted, since a
    clean filter or line-ending conversion can hide a content change from the diff.
    """
    r,o=source_fields(recorded),source_fields(observed)
    if any(r[k]!=o[k] for k in SOURCE_FIELDS if k!='status_sha256'):return False
    if r['status_sha256']!=EMPTY_SHA256 or o['tracked_diff_sha256']!=EMPTY_SHA256:return False
    if set(recorded)!=set(observed) or recorded.get('inputs')!=observed.get('inputs'):return False
    lines=status.get('_lines')
    if status.get('sha256')!=o['status_sha256'] or not lines or status.get('truncated') or status.get('omitted_credential_like'):return False
    entries=status.get('entries') or []
    if len(entries)!=len(lines):return False
    return all(xy==' M' and e.get('xy')==' M' and e.get('kind')=='link' and e.get('index_mode')==0o120000 and isinstance(e.get('index_bytes'),int)
               and e.get('index_bytes')!=e.get('bytes') and e.get('link_matches_head') is True for (xy,_),e in zip(lines,entries))
def public_status(status):
    return {k:v for k,v in status.items() if k!='_lines'}
# Item 788. The helper hashes its own status listing, and the probe lists the status a
# moment later. A stat-only line can clear in between, as in run 12, where the helper
# hashed the ` M` line of the checkout's only symbolic link and the probe's listing was
# empty. The helper's hash is then matched against the ` M` lines that tracked links
# could produce, and only links whose index entry, HEAD entry and worktree target agree
# can account for it. At most LINK_LISTING_MAX links are tried, every subset in git's order.
LINK_LISTING_MAX=8
def head_links(repo,env):
    """The tracked symbolic links whose index blob is HEAD's and whose raw target matches it.

    Returns None when the checkout has more than LINK_LISTING_MAX tracked links."""
    data=command(['git','-C',str(repo),'-c','core.fsmonitor=false','--no-optional-locks','ls-files','--stage','-z'],15,env,repo)
    links=[]
    for item in data.split(b'\0'):
        m=re.fullmatch(rb'120000 ([0-9a-f]{40,64}) 0\t(.+)',item,re.S)
        if m:links.append((m[1],m[2]))
    if len(links)>LINK_LISTING_MAX:return None
    out=[]
    for blob,raw in links:
        full=os.path.join(os.fsencode(str(repo)),raw)
        try:linked=stat.S_ISLNK(os.lstat(full).st_mode)
        except FileNotFoundError:continue
        if not linked:continue
        tree=command(['git','-C',str(repo),'--literal-pathspecs','ls-tree','-z','HEAD','--',raw.decode('utf-8','surrogateescape')],15,env,repo)
        if tree==b'120000 blob '+blob+b'\t'+raw+b'\0' and link_matches_head(repo,raw,full,env):out.append(raw)
    return out
def link_listing(status_sha256,links):
    """The links whose ` M` lines alone, in git's order, hash to ``status_sha256``, or None."""
    for n in range(1,len(links)+1):
        for subset in itertools.combinations(links,n):
            if hash_bytes(b''.join(b' M '+raw+b'\0' for raw in subset))==status_sha256:return list(subset)
    return None
def stat_only_lines(recorded,observed,repo,env):
    """The link paths that alone account for a drifted status the probe could not list.

    Every field but the status equals a clean recording, the tracked diff is empty, and
    the helper's status hash is that of ` M` lines of tracked links that match HEAD."""
    r,o=source_fields(recorded),source_fields(observed)
    if any(r[k]!=o[k] for k in SOURCE_FIELDS if k!='status_sha256'):return None
    if r['status_sha256']!=EMPTY_SHA256 or o['tracked_diff_sha256']!=EMPTY_SHA256 or o['status_sha256'] is None:return None
    if set(recorded)!=set(observed) or recorded.get('inputs')!=observed.get('inputs'):return None
    links=head_links(repo,env)
    names=None if links is None else link_listing(o['status_sha256'],links)
    # A credential-like path is never listed, so it never accounts for a status either.
    return None if names is None or any(status_credential_like(raw.decode('utf-8','replace')) for raw in names) else names
SOURCE_REREADS=3
def startup_source(repo,expected,observe,env,pause=None):
    """The checkout's first source observation and, when a stat-only status is accepted, its evidence.

    ``observe`` runs the installed helper. A first observation equal to the recording
    passes. A drift passes when the probe's own listing shows it is stat-only. When the
    listing cannot show the hashed lines (item 788), the hash must belong to ` M` lines of
    HEAD-matching links. Up to SOURCE_REREADS further observations, a second apart, then
    look for one that equals the recording or is stat-only by a listing read right after
    it. A reread that is neither and whose status the links cannot account for in the
    same way is drift at once, so a change seen between two clean observations is never
    accepted. SourceDrift names the last pair read.
    """
    first=observe()
    if first==expected:return first,None
    status=status_record(repo,env)
    if stat_only_status(expected,first,status):return first,dict(public_status(status),stat_only=True)
    try:names=stat_only_lines(expected,first,repo,env)
    except Exception:names=None
    if names is None:raise SourceDrift('existing source drift',repo,expected,first,status)
    again,now=first,status
    for attempt in range(SOURCE_REREADS):
        if attempt:(pause or __import__('time').sleep)(1)
        again=observe();now=status_record(repo,env)
        if again==expected or stat_only_status(expected,again,now):
            return first,dict(public_status(now),stat_only=True,transient=True,attributed=[raw.decode('utf-8','replace') for raw in names])
        try:still=stat_only_lines(expected,again,repo,env)
        except Exception:still=None
        if still is None:break
    raise SourceDrift('existing source drift',repo,expected,again,now)

class SourceDrift(Refusal):
    """A checkout whose source identity differs, with every field recorded beside observed."""
    def __init__(self,reason,repo,recorded,observed,status=None):
        super().__init__(reason);r,o=source_fields(recorded),source_fields(observed)
        other=lambda row:{k:v for k,v in row.items() if k not in ('head','status_sha256','tracked_diff_sha256','inputs')} if isinstance(row,dict) else None
        inputs=lambda row:{k:v for k,v in row.get('inputs',{}).items() if k not in ('mise.toml','rust-toolchain.toml')} if isinstance(row,dict) and isinstance(row.get('inputs'),dict) else None
        self.source_drift=drift_evidence({'repository':str(repo),'fields':{k:{'recorded':r[k],'observed':o[k],'differs':r[k]!=o[k]} for k in SOURCE_FIELDS},
                                          'other_fields_differ':other(recorded)!=other(observed) or inputs(recorded)!=inputs(observed),
                                          **({} if status is None else {'status':public_status(status)})})
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

# A 12 GiB job, the 512 MiB metadata service and a 1 GiB host reserve.
JOB_HEADROOM=12*2**30+512*2**20+2**30
HOST_NONRECLAIMABLE=['AnonPages','Shmem','Unevictable','KernelStack','PageTables','SecPageTables','Percpu','SUnreclaim']

def cgroup_nonreclaimable(stat_text):
    """Charge a cgroup cannot give back without swap: anon, shmem, unevictable, kernel.

    Page cache is reclaimable and does not count. The fields overlap a little, since
    unevictable pages are also anon or file, so the sum errs on the side of refusal.
    """
    stat={}
    for line in stat_text.splitlines():
        key,_,value=line.partition(' ')
        if value.strip().isdigit():stat[key]=int(value)
    need('anon' in stat,'cgroup memory.stat incomplete')
    if 'kernel' in stat:kernel=stat['kernel']-stat.get('slab_reclaimable',0)
    else:kernel=sum(stat.get(k,0) for k in ['kernel_stack','pagetables','sec_pagetables','percpu','sock','vmalloc','slab_unreclaimable'])
    return stat['anon']+stat.get('shmem',0)+stat.get('unevictable',0)+max(0,kernel)

def memory_admission(meminfo,ancestors):
    """Refuse unless the host and every limited ancestor cgroup have room for the job.

    Room is the limit minus memory that cannot be reclaimed, so a worker whose usage
    is mostly page cache admits and one full of anonymous memory refuses.
    """
    memory={};kb={}
    for line in meminfo.splitlines():
        k,_,v=line.partition(':')
        if v.split() and v.split()[0].isdigit():kb[k]=int(v.split()[0])*1024
    need('MemTotal' in kb and 'AnonPages' in kb,'host meminfo incomplete')
    for k in ['MemTotal','MemAvailable','SwapTotal','SwapFree']:
        if k in kb:memory[k]=kb[k]
    host=sum(kb.get(k,0) for k in HOST_NONRECLAIMABLE);memory['host_nonreclaimable']=host
    need(kb['MemTotal']-host>=JOB_HEADROOM,'insufficient host headroom for proposed12GiB job')
    for row in ancestors:
        if row.get('memory.max','max')=='max':continue
        need('memory.stat' in row,'limited ancestor without memory.stat')
        row['nonreclaimable']=cgroup_nonreclaimable(row['memory.stat'])
        need(int(row['memory.max'])-row['nonreclaimable']>=JOB_HEADROOM,'ancestor memory headroom')
    return memory

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
        first,stale=startup_source(repo,expected,lambda:json.loads(command(['/usr/bin/python3',helper,'source',str(repo)],30,env,repo)),env)
        check=json.loads(command(['/bin/bash',str(new/'repository-start.sh'),str(repo)],55,env,repo))
        need(check['native_started'] is False and check['status']=='tools-ready-workload-admission-still-required' and check['uid']==uid,'checker outcome')
        last=json.loads(command(['/usr/bin/python3',helper,'source',str(repo)],30,env,repo))
        # The checker may refresh the index, so a stat-only status can clear, but content cannot move.
        if last!=first and not (stale is not None and (last==expected or stat_only_status(expected,last,status_record(repo,env)))):
            raise SourceDrift('existing source changed during qualification',repo,first,last,status_record(repo,env))
        sources[str(repo)]={'before':first,'after':last};checks[str(repo)]={'status':check['status'],'installed_bindings':check['installed_bindings']}
        if stale is not None:sources[str(repo)]['stat_only_status']=status_evidence(stale)
    after,_=safe_file(state/'toolchain.json',uid,denied,1024*1024);need(after==before,'manifest changed during checker')
    # Check parent capacity, excluding the deliberately smaller metadata service.
    ancestors=[];ancestor=cg.parent
    while ancestor.is_relative_to(Path('/sys/fs/cgroup')):
        row={'path':str(ancestor)}
        for name in ['memory.max','memory.current','memory.swap.max','memory.stat']:
            if (ancestor/name).exists():row[name]=(ancestor/name).read_text().strip()
        ancestors.append(row)
        if ancestor==Path('/sys/fs/cgroup'):break
        ancestor=ancestor.parent
    memory=memory_admission(Path('/proc/meminfo').read_text(),ancestors)
    for row in ancestors:row.pop('memory.stat',None)
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
    return {'memory_events':events,'memory_peak':(cg/'memory.peak').read_text().strip(),'qualified':True,'worker_boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'engine_or_job_started':False,'memory_max':maxmem,'swap_max':maxswap,'manifest_unchanged':True,'saved_manifest_sha256':before['sha256'],'directories':directories,'bundle_records':bundle_records,'source_observations':sources,'checker_results':checks,'token_metadata':tm,'public_cache_config':public_config,'shared_cache_and_managed_targets_preserved':True,'private_cache_or_target_created':False,'memory_bytes':memory,'ancestor_limits':ancestors,'disk':disk,'loaded_properties':properties,'native_heavy_processes':heavy,'lease_basis':'Root previous native/lease closure + exclusive worker + no lease-acquiring operation in this controller; not a new MBX lease grant','private_credential_snapshot':True,'fresh_job_admission_still_required':True}
if __name__=='__main__':
    try:
        result=probe(json.loads(sys.argv[1]));print(json.dumps(result));raise SystemExit(0)
    except Exception:
        print(json.dumps({'qualified':False,'engine_or_job_started':False,'failure':'qualification refused; no arbitrary stderr or credential content retained'}));raise SystemExit(1)
