"""Composed after metadata-probe definitions; no provider or installer invocation."""
import fcntl,time,contextlib

INFRASTRUCTURE={'systemd','(sd-pam)','dbus-daemon'}
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
        if pid in excluded:continue
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
    deadline=clock()+seconds;tracked=set();last='not observed'
    while clock()<deadline:
        try:
            with bootstrap_lock(state/'bootstrap.lock',uid,denied) as lock:
                manifest,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                # The actual pinned hook has no manifest deletion branch. If not
                # yet at its absence test it now skips bootstrap; a running
                # bootstrap still owns fd9/flock until its waited children end.
                busy=foreign_startup(rows_reader(),launcher,probe_pid,tracked)
                if busy:last='live or unknown startup descendants';pause(0.5);continue
                pause(0.5)
                busy=foreign_startup(rows_reader(),launcher,probe_pid,tracked)
                current,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                need(current==manifest,'startup manifest path/bytes changed')
                if busy:last='live or unknown startup descendants';continue
                return {'startup_closed':True,'installer_quiescent':True,'manifest_sha256':manifest['sha256'],'qualification_eligible':manifest['sha256']==expected_manifest,'bootstrap_lock':lock,'bootstrap_occurrence':'unknown','hook_terminal_observed':False,'closure_basis':'existing bootstrap fd9 lock, stable owned present manifest and repeated exact PID/start/descendant observations; any not-yet-entered pinned absence branch must skip bootstrap','unknown_or_live_descendants':[],'engine_or_job_started':False}
        except (BlockingIOError,FileNotFoundError):last='bootstrap lock busy or manifest not yet present';pause(0.5)
    return {'startup_closed':False,'installer_quiescent':False,'qualification_eligible':False,'bootstrap_occurrence':'unknown','reason':last,'engine_or_job_started':False}

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
