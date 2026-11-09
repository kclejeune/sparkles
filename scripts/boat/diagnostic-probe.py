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

def entry(path):
    s=path.lstat()
    kind='directory' if stat.S_ISDIR(s.st_mode) else ('file' if stat.S_ISREG(s.st_mode) else ('link' if stat.S_ISLNK(s.st_mode) else 'other'))
    return {'name':text(path.name,128),'kind':kind,'bytes':s.st_size,'mode':stat.S_IMODE(s.st_mode),'uid':s.st_uid,'nlink':s.st_nlink,'device':s.st_dev,'inode':s.st_ino,'mtime_ns':s.st_mtime_ns,'ctime_ns':s.st_ctime_ns}

def phase_result(path,uid):
    """A phase's own result file, read only when it is a private regular file of the worker."""
    try:
        s=path.lstat()
        if not stat.S_ISREG(s.st_mode) or s.st_uid!=uid or s.st_nlink!=1 or s.st_mode&0o077:return {'unread':'not a private regular file'}
        if s.st_size>RESULT_MAX:return {'unread':'over the result bound','bytes':s.st_size}
        fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
        try:data=os.read(fd,RESULT_MAX+1)
        finally:os.close(fd)
        body=json.loads(data)
        return {'result':body} if isinstance(body,dict) else {'unread':'not an object'}
    except (OSError,ValueError) as error:return {'unread':type(error).__name__}

def state(home,uid):
    root=home/'.local/state/sparkles-boat';out={'present':root.is_dir(),'entries':[],'phases':{}}
    if not out['present']:return out
    names=sorted(os.listdir(root))
    out['entry_count']=len(names)
    for name in names[:ENTRIES_MAX]:
        try:out['entries'].append(entry(root/name))
        except OSError:continue
    for name in names:
        if not name.startswith('qualification-'):continue
        phase=root/name
        try:
            if not stat.S_ISDIR(phase.lstat().st_mode):continue
            inner=sorted(os.listdir(phase))[:32]
        except OSError:continue
        row={'entries':[text(n,64) for n in inner]}
        if 'probe-result.json' in inner:row.update(phase_result(phase/'probe-result.json',uid))
        out['phases'][text(name,128)]=row
    return out

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
    # Results are dropped, largest first, until the output fits the transport bound.
    while len(json.dumps(body))>OUTPUT_MAX:
        phases=[k for k,v in body['state']['phases'].items() if 'result' in v]
        if not phases:
            body['processes']['rows']=body['processes']['rows'][:len(body['processes']['rows'])//2];continue
        largest=max(phases,key=lambda k:len(json.dumps(body['state']['phases'][k]['result'])))
        body['state']['phases'][largest]={'entries':body['state']['phases'][largest]['entries'],'unread':'dropped to fit the output bound'}
    return body

if __name__=='__main__':
    print(json.dumps(collect()))
