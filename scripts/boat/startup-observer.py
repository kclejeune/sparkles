"""Composed after metadata-probe definitions; no provider or installer invocation."""
import fcntl,time,contextlib

INFRASTRUCTURE={'systemd','(sd-pam)','dbus-daemon'}
# Persistent processes of the provider's desktop session, which start with the
# first SSH connection and stay. Each entry is (name, parent) for a process of
# the worker's uid, taken from the listing observed on the admitted large worker
# bx_gy2ytgh8 (environment v8). The parent 'init' is pid 1 as root, 'user-manager'
# is the worker's own systemd that pid 1 started, and any other parent is that
# name of the worker's uid and must itself be allowed. A process outside this
# table, or one under another parent, still counts as startup work, and so does
# any descendant of tracked startup work, whatever its name.
PROVIDER_SESSION=frozenset([
    ('pulseaudio','init'),('ascii-agent-sup','init'),('ibus-daemon','init'),('ibus-x11','init'),('sunshine','init'),
    ('dbus-launch','init'),('at-spi-bus-laun','init'),('at-spi2-registr','init'),
    ('pipewire','user-manager'),('wireplumber','user-manager'),('pipewire-pulse','user-manager'),('gvfsd','user-manager'),
    ('gvfsd-fuse','user-manager'),('at-spi-bus-laun','user-manager'),('xdg-desktop-por','user-manager'),
    ('xdg-document-po','user-manager'),('xdg-permission-','user-manager'),('cua-driver','user-manager'),
    ('dconf-service','user-manager'),('ibus-portal','user-manager'),('at-spi2-registr','user-manager'),('gnome-keyring-d','user-manager'),
    ('ascii-agents-se','ascii-agent-sup'),('ibus-memconf','ibus-daemon'),('ibus-ui-gtk3','ibus-daemon'),
    ('ibus-extension-','ibus-daemon'),('ibus-engine-sim','ibus-daemon'),('ascii-desktop','dbus-run-sessio'),('openbox','ascii-desktop'),
])
UNALLOWED_MAX=32
INSTALLER={'bash','sh','dash','sudo','apt','apt-get','dpkg','dpkg-deb','dpkg-preconfigu','needrestart','perl','curl','wget','minisign','rustup','mise','mbx','cargo','rustc','python','python3','python3.12','python3.13'}

def process_rows(proc=Path('/proc')):
    rows={}
    for entry in proc.iterdir():
        if not entry.name.isdigit():continue
        need(len(rows)<8192,'process inventory bound')
        try:
            raw=(entry/'stat').read_text();end=raw.rfind(')');parts=raw[end+2:].split();status=(entry/'status').read_text()
            uid=int(re.search(r'^Uid:\s+(\d+)',status,re.M)[1])
            rows[int(entry.name)]={'pid':int(entry.name),'comm':raw[raw.find('(')+1:end],'ppid':int(parts[1]),'group':int(parts[2]),'start':int(parts[19]),'uid':uid}
        except (FileNotFoundError,ProcessLookupError,PermissionError):continue
    return rows

def provider_session(pid,rows,depth=0):
    row=rows.get(pid);uid=profile()['worker_uid']
    if row is None or row['uid']!=uid or depth>8:return False
    parent=rows.get(row['ppid'])
    if parent is None:return False
    if parent['pid']==1 and parent['comm']=='systemd' and parent['uid']==0:ref='init'
    elif parent['comm']=='systemd' and parent['uid']==uid and parent['ppid']==1 and rows.get(1,{}).get('uid')==0:ref='user-manager'
    else:ref=parent['comm']
    if (row['comm'],ref) not in PROVIDER_SESSION:return False
    return ref in ('init','user-manager') or provider_session(parent['pid'],rows,depth+1)

def unallowed_inventory(rows,busy):
    """Name, uid and parent of each busy process, bounded and without command lines."""
    out=[]
    for row in sorted(busy,key=lambda x:x['pid'])[:UNALLOWED_MAX]:
        parent=rows.get(row['ppid'])
        out.append({'pid':row['pid'],'comm':bounded_text(row['comm'],16),'uid':row['uid'],'ppid':row['ppid'],
                    'parent_comm':None if parent is None else bounded_text(parent['comm'],16),'parent_uid':None if parent is None else parent['uid']})
    return out

def foreign_startup(rows,launcher,probe_pid,tracked):
    # Exact launcher identity is passed by our own remote parent, never inferred
    # from a command-line substring. Only its tree/ancestors and our own service
    # tree are excluded; other SSH/scripts/installers remain unknown/busy.
    need(launcher['pid'] in rows and rows[launcher['pid']]['start']==launcher['start'],'launcher identity changed')
    own={launcher['pid'],probe_pid};ancestors=set()
    for seed in list(own):
        current=seed
        while current in rows and current not in ancestors:
            ancestors.add(current);current=rows[current]['ppid']
    changed=True
    while changed:
        changed=False
        for pid,row in rows.items():
            if row['ppid'] in own and pid not in own:own.add(pid);changed=True
    excluded=own|ancestors
    for pid,row in rows.items():
        if pid in excluded or provider_session(pid,rows):continue
        if (row['uid']==profile()['worker_uid'] and row['comm']not in INFRASTRUCTURE) or row['comm']in INSTALLER:
            tracked.add((pid,row['start']))
    changed=True
    while changed:
        changed=False
        live={pid for pid,start in tracked if pid in rows and rows[pid]['start']==start}
        for pid,row in rows.items():
            if pid not in excluded and row['ppid']in live and (pid,row['start'])not in tracked:
                tracked.add((pid,row['start']));changed=True
    return [rows[pid] for pid,start in sorted(tracked) if pid in rows and rows[pid]['start']==start and pid not in excluded]

@contextlib.contextmanager
def bootstrap_lock(path,uid,denied):
    s=path.lstat();need(stat.S_ISREG(s.st_mode) and s.st_uid==uid and s.st_nlink==1 and stat.S_IMODE(s.st_mode)==0o600 and s.st_size==0,'bootstrap lock ownership')
    need((s.st_dev,s.st_ino)!=denied,'bootstrap lock token alias')
    fd=os.open(path,os.O_RDWR|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        current=os.fstat(fd);need((current.st_dev,current.st_ino,current.st_uid,current.st_mode,current.st_nlink,current.st_size)==(s.st_dev,s.st_ino,s.st_uid,s.st_mode,s.st_nlink,s.st_size),'bootstrap lock replaced')
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        yield {'path':str(path),'device':s.st_dev,'inode':s.st_ino,'uid':uid,'mode':0o600,'held_during_checkpoint':True,'released_after_checkpoint':True}
        after=path.lstat();need((after.st_dev,after.st_ino,after.st_nlink,after.st_size)==(s.st_dev,s.st_ino,1,0),'bootstrap lock changed during observation')
    finally:os.close(fd)

def checkpoint_at(state,uid,denied,launcher,probe_pid,expected_manifest,seconds,rows_reader=process_rows,clock=time.monotonic,pause=time.sleep):
    deadline=clock()+seconds;tracked=set();last='not observed';code='startup_unobserved';rows={};busy=[]
    while clock()<deadline:
        try:
            with bootstrap_lock(state/'bootstrap.lock',uid,denied) as lock:
                manifest,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                # The actual pinned hook has no manifest deletion branch. If not
                # yet at its absence test it now skips bootstrap; a running
                # bootstrap still owns fd9/flock until its waited children end.
                rows=rows_reader();busy=foreign_startup(rows,launcher,probe_pid,tracked)
                if busy:last='live or unknown startup descendants';code='startup_busy:unallowed_processes';pause(0.5);continue
                pause(0.5)
                rows=rows_reader();busy=foreign_startup(rows,launcher,probe_pid,tracked)
                current,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                need(current==manifest,'startup manifest path/bytes changed')
                if busy:last='live or unknown startup descendants';code='startup_busy:unallowed_processes';continue
                return {'startup_closed':True,'installer_quiescent':True,'manifest_sha256':manifest['sha256'],'qualification_eligible':manifest['sha256']==expected_manifest,'bootstrap_lock':lock,'bootstrap_occurrence':'unknown','hook_terminal_observed':False,'closure_basis':'existing bootstrap fd9 lock, stable owned present manifest and repeated exact PID/start/descendant observations; any not-yet-entered pinned absence branch must skip bootstrap','unknown_or_live_descendants':[],'engine_or_job_started':False}
        except BlockingIOError:last='bootstrap lock busy or manifest not yet present';code='startup_busy:bootstrap_lock_held';busy=[];pause(0.5)
        except FileNotFoundError:last='bootstrap lock busy or manifest not yet present';code='startup_busy:lock_or_manifest_absent';busy=[];pause(0.5)
    return {'startup_closed':False,'installer_quiescent':False,'qualification_eligible':False,'bootstrap_occurrence':'unknown','reason':last,'reason_code':code,
            'unallowed':unallowed_inventory(rows,busy),'unallowed_count':len(busy),'engine_or_job_started':False}

def run_phase(payload,observe,qualify,clock=time.monotonic):
    """One remote phase's result. Every refusal carries a bounded reason code."""
    started=clock()
    try:result=observe(payload) if payload['phase']!='metadata' else qualify(payload)
    except Exception as error:
        result={'qualified':False,'startup_closed':False,'engine_or_job_started':False,'failure':'bounded phase refused; no arbitrary error content','reason_code':reason_code(error)}
    closed=result.get('startup_closed')is True if payload['phase']!='metadata' else result.get('qualified')is True
    if not closed:result.setdefault('reason_code','unreported')
    result['elapsed_seconds']=round(clock()-started,3)
    return result,closed

def startup_checkpoint(payload,seconds):
    home=Path.home();uid=os.getuid();need(uid==profile()['worker_uid'] and home==Path(profile()['worker_home']),'startup worker user')
    token=token_metadata(home/'.config/mbx/remote-token',uid);denied=(token['device'],token['inode']);state=home/'.local/state/sparkles-boat'
    return checkpoint_at(state,uid,denied,payload['launcher_identity'],os.getpid(),payload['pins']['saved_toolchain_manifest_sha256'],seconds)

def observe_startup(payload):
    seconds=payload['observation_seconds'];need(type(seconds)is int and 1<=seconds<=1950,'startup observation bound')
    unit=payload['operation_tag']+'-'+payload['phase']+'.service';line=Path('/proc/self/cgroup').read_text().strip();need(line.startswith('0::'),'startup cgroup')
    cg=Path('/sys/fs/cgroup')/line[3:].lstrip('/');need(cg.name==unit,'startup unit identity')
    raw=command(['systemctl','--user','show',unit,'--property=MemoryMax,MemorySwapMax,RuntimeMaxUSec,TimeoutStopUSec,KillMode,OOMPolicy,MainPID,InvocationID,ControlGroup'])
    properties=dict(x.split('=',1)for x in raw.decode().splitlines()if '='in x)
    validate_runtime(unit,os.getpid(),line,(cg/'memory.max').read_text().strip(),(cg/'memory.swap.max').read_text().strip(),properties,payload['phase_runtime_seconds'])
    result=startup_checkpoint(payload,seconds)
    events=dict(line.split()for line in(cg/'memory.events').read_text().splitlines());validate_pressure(events,(cg/'memory.swap.peak').read_text().strip())
    result.update({'memory_max':'536870912','swap_max':'0','loaded_properties':properties,'memory_events':events})
    return result

def qualify_with_startup_lock(payload):
    home=Path.home();uid=os.getuid();token=token_metadata(home/'.config/mbx/remote-token',uid);denied=(token['device'],token['inode']);state=home/'.local/state/sparkles-boat';tracked=set()
    with bootstrap_lock(state/'bootstrap.lock',uid,denied) as lock:
        before,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
        need(before['sha256']==payload['pins']['saved_toolchain_manifest_sha256'],'qualification manifest pin')
        need(not foreign_startup(process_rows(),payload['launcher_identity'],os.getpid(),tracked),'startup not quiescent before tools')
        result=probe(payload)
        after,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
        need(after==before and not foreign_startup(process_rows(),payload['launcher_identity'],os.getpid(),tracked),'startup/source drift during qualification')
        result.update({'startup_closed':True,'bootstrap_lock':lock,'bootstrap_occurrence':'unknown','actual_toolchain_manifest':{'path':str(state/'toolchain.json'),'sha256':before['sha256']},'runtime_env_file':None})
        return result
