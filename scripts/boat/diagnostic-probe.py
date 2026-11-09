"""Read-only refusal diagnostics, run on the worker over SSH during the diagnostic hold.

It reads process metadata, the package's own state directory listing and phase
results, the host memory summary and free disk space. It opens no other file,
never reads the token or any repository or tool file, starts no unit and writes
nothing. Every value is bounded, and its output stays below the transport bound.
"""
from pathlib import Path
import json,os,re,stat,subprocess,sys

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
    s=os.fstat(fd)
    if not stat.S_ISDIR(s.st_mode) or s.st_uid!=uid or s.st_mode&0o022:
        os.close(fd);raise PermissionError('not an owned private directory')
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

def collect(home=None,uid=None):
    home=Path.home() if home is None else home;uid=os.getuid() if uid is None else uid
    body={'schema':1,'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'uptime_seconds':float(Path('/proc/uptime').read_text().split()[0]),
          'uid':uid,'processes':processes(),'state':state(home,uid),'memory_bytes':memory(),'disk':disk(home),'units':units()}
    return fit(body,OUTPUT_MAX,lambda x:len(json.dumps(x)))

if __name__=='__main__':
    print(json.dumps(collect()))
