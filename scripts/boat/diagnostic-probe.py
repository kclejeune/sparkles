"""Read-only refusal diagnostics, run on the worker over SSH during the diagnostic hold.

It reads process metadata, the package's own state directory listing and phase
results, the host memory summary and free disk space. For each startup checkout it
names, it lists the untracked paths that git does not ignore, with their sizes and
mtimes from lstat and without their content. It opens no other file, never reads the
token or any tool file, starts no unit and writes nothing. Every value is bounded, and
its output stays below the transport bound.
"""
from pathlib import Path
import json,os,re,shutil,stat,subprocess,sys

OUTPUT_MAX=900*1024
RESULT_MAX=64*1024
ROWS_MAX=2048
ENTRIES_MAX=512
MEMINFO=('MemTotal','MemAvailable','MemFree','Buffers','Cached','AnonPages','Shmem','Unevictable','KernelStack','PageTables','SecPageTables','Percpu','SUnreclaim','SReclaimable','SwapTotal','SwapFree')

def text(value,limit=64):
    return ''.join(c if ' '<=c<='~' else '?' for c in str(value)[:limit])

def processes():
    rows=[]
    for entry in os.listdir('/proc'):
        if not entry.isdigit():continue
        try:
            raw=Path('/proc',entry,'stat').read_text();end=raw.rfind(')');parts=raw[end+2:].split()
            status=Path('/proc',entry,'status').read_text();uid=int(re.search(r'^Uid:\s+(\d+)',status,re.M)[1])
            threads=int(re.search(r'^Threads:\s+(\d+)',status,re.M)[1])
            rows.append({'pid':int(entry),'ppid':int(parts[1]),'uid':uid,'start':int(parts[19]),'threads':threads,'comm':text(raw[raw.find('(')+1:end],16)})
        except (OSError,ValueError,TypeError,IndexError):continue
    rows.sort(key=lambda r:r['pid'])
    return {'count':len(rows),'rows':rows[:ROWS_MAX]}

PHASES_MAX=64
PHASE_NAMES=('startup','metadata','stopcheck')
DIRECTORY=os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW

def entry(name,parent):
    s=os.stat(name,dir_fd=parent,follow_symlinks=False)
    kind='directory' if stat.S_ISDIR(s.st_mode) else ('file' if stat.S_ISREG(s.st_mode) else ('link' if stat.S_ISLNK(s.st_mode) else 'other'))
    return {'name':text(name,128),'kind':kind,'bytes':s.st_size,'mode':stat.S_IMODE(s.st_mode),'uid':s.st_uid,'nlink':s.st_nlink,'device':s.st_dev,'inode':s.st_ino,'mtime_ns':s.st_mtime_ns,'ctime_ns':s.st_ctime_ns}

def owned_directory(name,parent,uid):
    """A directory of the worker opened by name under ``parent``, never through a symbolic link."""
    fd=os.open(name,DIRECTORY,dir_fd=parent)
    try:
        s=os.fstat(fd)
        if not stat.S_ISDIR(s.st_mode) or s.st_uid!=uid or s.st_mode&0o022:raise PermissionError('not an owned private directory')
    except BaseException:
        os.close(fd);raise
    return fd

def phase_result(parent,uid):
    """A phase's own result file, read only when it is a private regular file of the worker."""
    try:
        fd=os.open('probe-result.json',os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK,dir_fd=parent)
        try:
            s=os.fstat(fd)
            if not stat.S_ISREG(s.st_mode) or s.st_uid!=uid or s.st_nlink!=1 or s.st_mode&0o077:return {'unread':'not a private regular file'}
            if s.st_size>RESULT_MAX:return {'unread':'over the result bound','bytes':s.st_size}
            data=os.read(fd,RESULT_MAX+1)
        finally:os.close(fd)
        body=json.loads(data)
        if not isinstance(body,dict) or body.get('phase') not in PHASE_NAMES or not isinstance(body.get('program_sha256'),str) or re.fullmatch('[0-9a-f]{64}',body['program_sha256']) is None:
            return {'unread':'not a phase result'}
        return {'result':body}
    except (OSError,ValueError) as error:return {'unread':type(error).__name__}

def state(home,uid):
    out={'present':False,'entries':[],'phases':{}};fds=[]
    try:
        try:
            fds.append(os.open(str(home),DIRECTORY))
            for name in ('.local','state','sparkles-boat'):fds.append(owned_directory(name,fds[-1],uid))
        except OSError as error:
            out['unread']=type(error).__name__;return out
        root=fds[-1];out['present']=True
        names=sorted(os.listdir(root));out['entry_count']=len(names)
        for name in names[:ENTRIES_MAX]:
            try:out['entries'].append(entry(name,root))
            except OSError:continue
        phases=[n for n in names if n.startswith('qualification-')]
        out['phase_count']=len(phases)
        for name in phases[:PHASES_MAX]:
            try:fd=owned_directory(name,root,uid)
            except OSError as error:
                out['phases'][text(name,128)]={'unread':type(error).__name__};continue
            try:
                inner=sorted(os.listdir(fd))
                row={'entries':[text(n,64) for n in inner[:32]]}
                if 'probe-result.json' in inner:row.update(phase_result(fd,uid))
            finally:os.close(fd)
            out['phases'][text(name,128)]=row
        return out
    finally:
        for fd in fds:os.close(fd)

def fit(body,limit,size):
    """Drop phase results largest first, then process rows, then listings, until the body fits."""
    while size(body)>limit:
        phases=body['state']['phases']
        kept=[k for k,v in phases.items() if 'result' in v]
        if kept:
            largest=max(kept,key=lambda k:len(json.dumps(phases[k]['result'])))
            phases[largest]={'entries':phases[largest].get('entries',[]),'unread':'dropped to fit the output bound'};continue
        if body['processes']['rows']:
            body['processes']['rows']=body['processes']['rows'][:len(body['processes']['rows'])//2];continue
        if body['state']['entries']:
            body['state']['entries']=body['state']['entries'][:len(body['state']['entries'])//2];continue
        rows=[v for v in body.get('untracked',{}).values() if v.get('paths')]
        if rows:
            for v in rows:v['paths']=v['paths'][:len(v['paths'])//2];v['truncated']=True
            continue
        if phases:
            body['state']['phases']={};continue
        break
    return body

def memory():
    found={}
    for line in Path('/proc/meminfo').read_text().splitlines():
        key,_,value=line.partition(':')
        if key in MEMINFO and value.split() and value.split()[0].isdigit():found[key]=int(value.split()[0])*1024
    return found

def disk(home):
    out={}
    for path in (home,home/'.local/share/mbx'):
        try:fs=os.statvfs(path);out[str(path)]={'free_bytes':fs.f_bavail*fs.f_frsize,'free_inodes':fs.f_favail}
        except OSError as error:out[str(path)]={'unread':type(error).__name__}
    return out

def units():
    try:
        r=subprocess.run(['systemctl','--user','list-units','--all','--plain','--no-legend','--no-pager','boat-*'],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10)
        rows=[]
        for line in r.stdout.decode('ascii','replace').splitlines()[:64]:
            parts=line.split()
            if len(parts)>=4:rows.append({'unit':text(parts[0],128),'load':text(parts[1],16),'active':text(parts[2],16),'sub':text(parts[3],16)})
        return {'exit':r.returncode,'rows':rows}
    except (OSError,subprocess.TimeoutExpired) as error:return {'unread':type(error).__name__}

# The commands the screen and build scripts need on the worker, as admission checks them.
TOOLS=('bash','sh','env','python3','git','systemd-run','systemctl','flock','timeout','mkfifo',
       'tar','gzip','zstd','curl','taskset','chrt','cc','cargo','realpath','sha256sum',
       'awk','sed','grep','tac','ls','cut','head','sort','seq','tr','wc','tee','cat','date','uname',
       'free','du','df','tail','chmod','cp','ln','mkdir','mktemp','mv','rm','dirname','sleep',
       'setsid','nohup','/lib64/ld-linux-x86-64.so.2')
def commands():
    return {name:shutil.which(name) is not None for name in TOOLS}

# At most UNTRACKED_MAX untracked paths per checkout. A path that looks like a credential
# is left out and only counted. Ignored paths never appear, and an untracked directory
# is one entry, so the listing stays small.
UNTRACKED_MAX=200
UNTRACKED_REPOS_MAX=4
UNTRACKED_BYTES=4*1024*1024
# A component is credential-like when it contains one of these stems anywhere, so backup,
# editor and archive copies such as `.netrc.bak`, `#.env#`, `backup.env` or
# `secrets.tar.gz` match too. Every component of a path is checked, so a credential-like
# parent hides its children. Matching ignores case, and a newline is an ordinary character.
CREDENTIAL_STEMS=('credential','secret','token','passw','apikey','api-key','api_key','private','netrc','npmrc','pypirc','pgpass',
                  'id_rsa','id_dsa','id_ecdsa','id_ed25519','.pem','.key','.p12','.pfx','.jks','keystore','.kdbx','.gpg','.asc','.ovpn',
                  '.env','.ssh','.gnupg','.aws','.docker','.kube')
def credential_like(name):
    return any(part and any(stem in part.lower() for stem in CREDENTIAL_STEMS) for part in name.split('/'))
def bounded_git(argv,limit,seconds):
    """Git's stdout up to limit bytes, and whether it was cut there; the child is always reaped."""
    import selectors,time
    child=subprocess.Popen(argv,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
    chunks=[];size=0;cut=False;end=time.monotonic()+seconds
    try:
        with selectors.DefaultSelector() as sel:
            sel.register(child.stdout,selectors.EVENT_READ)
            while True:
                left=end-time.monotonic()
                if left<=0:raise subprocess.TimeoutExpired(argv,seconds)
                if not sel.select(min(left,1.0)):continue
                data=os.read(child.stdout.fileno(),65536)
                if not data:break
                chunks.append(data[:max(0,limit-size)]);size+=len(data)
                # Only output past the limit is a cut; exactly the limit reads to EOF.
                if size>limit:cut=True;break
        code=None if cut else child.wait(max(0.1,end-time.monotonic()))
        return b''.join(chunks),cut,code
    finally:
        if child.poll() is None:
            child.kill()
            try:child.wait(5)
            except subprocess.TimeoutExpired:pass
        child.stdout.close()
def listed_names(data,cut):
    """The NUL-terminated names of a listing. A cut listing drops only a partial last name."""
    parts=data.split(b'\0')
    if cut and not data.endswith(b'\0'):parts=parts[:-1]
    return [n for n in parts if n]
def untracked(repos):
    out={}
    for repo in [r for r in repos if isinstance(r,str) and r.startswith('/')][:UNTRACKED_REPOS_MAX]:
        row={'paths':[],'total':0,'omitted_credential_like':0,'truncated':False}
        try:
            # No fsmonitor hook, untracked cache or lock; ls-files compares no tracked content.
            data,cut,code=bounded_git(['git','-C',repo,'-c','core.fsmonitor=false','-c','core.untrackedCache=false','--no-optional-locks',
                                       'ls-files','--others','--exclude-standard','--directory','-z'],UNTRACKED_BYTES,20)
            if code:row['unread']='git exit %d'%code;out[text(repo,200)]=row;continue
            row['truncated']=cut
            names=listed_names(data,cut)
            row['total']=len(names)
            for raw in names:
                name=raw.decode('utf-8','replace')
                if credential_like(name):row['omitted_credential_like']+=1;continue
                if len(row['paths'])>=UNTRACKED_MAX:row['truncated']=True;continue
                entry={'path':text(name,300)}
                try:
                    s=os.lstat(os.path.join(os.fsencode(repo),raw))
                    entry.update(kind='directory' if stat.S_ISDIR(s.st_mode) else ('file' if stat.S_ISREG(s.st_mode) else ('link' if stat.S_ISLNK(s.st_mode) else 'other')),
                                 bytes=s.st_size if stat.S_ISREG(s.st_mode) else None,mtime_ns=s.st_mtime_ns,mode=stat.S_IMODE(s.st_mode))
                except OSError as error:entry['unread']=type(error).__name__
                row['paths'].append(entry)
        except (OSError,subprocess.TimeoutExpired) as error:row['unread']=type(error).__name__
        out[text(repo,200)]=row
    return out

def collect(home=None,uid=None,repos=()):
    home=Path.home() if home is None else home;uid=os.getuid() if uid is None else uid
    body={'schema':1,'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'uptime_seconds':float(Path('/proc/uptime').read_text().split()[0]),
          'uid':uid,'processes':processes(),'state':state(home,uid),'memory_bytes':memory(),'disk':disk(home),'units':units(),'commands':commands(),'untracked':untracked(list(repos))}
    return fit(body,OUTPUT_MAX,lambda x:len(json.dumps(x)))

if __name__=='__main__':
    print(json.dumps(collect(repos=sys.argv[1:])))
