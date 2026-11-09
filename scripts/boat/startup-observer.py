"""Composed after metadata-probe definitions; no provider or installer invocation."""
import fcntl,time,contextlib

INFRASTRUCTURE={'systemd','(sd-pam)','dbus-daemon'}
# Persistent processes of the provider's desktop session, which start with the
# first SSH connection and stay. Each entry names a process of the worker's uid and
# the full chains of ancestors it may have, from its parent up to pid 1, as observed
# on the admitted large worker bx_gy2ytgh8 (environment v8) in the run-1 listing and
# the op-2 process tree. An ancestor is INIT, pid 1 as root, MANAGER, the worker's
# user manager that the system manager reports for user@UID.service, or a name with
# 'worker' or 'root' for its uid. An ancestor of the worker's uid must itself be an
# allowed session process. A process outside this table, or with any other chain,
# still counts as startup work, and so does any descendant of tracked startup work.
INIT=('init',);MANAGER=('manager',)
def _w(name):return (name,'worker')
def _r(name):return (name,'root')
_AGENT=((_w('ascii-agents-se'),INIT),(_w('ascii-agents-se'),_w('ascii-agent-sup'),INIT))
_LIGHTDM=(_r('lightdm'),_r('lightdm'),INIT)
SESSION_CHAINS={}
for _name in ['pulseaudio','ascii-agent-sup','ibus-daemon','ibus-x11','sunshine','dbus-launch','at-spi-bus-laun','at-spi2-registr','ascii-agents-se']:
    SESSION_CHAINS.setdefault(_name,set()).add((INIT,))
for _name in ['pipewire','wireplumber','pipewire-pulse','gvfsd','gvfsd-fuse','at-spi-bus-laun','xdg-desktop-por','xdg-document-po',
              'xdg-permission-','cua-driver','dconf-service','ibus-portal','at-spi2-registr','gnome-keyring-d',
              'obexd','gvfs-udisks2-vo','gvfs-gphoto2-vo','gvfs-afc-volume','gvfs-goa-volume','gvfs-mtp-volume','gvfsd-metadata',
              'goa-daemon','goa-identity-se','evolution-sourc','evolution-calen','evolution-addre']:
    SESSION_CHAINS.setdefault(_name,set()).add((MANAGER,INIT))
SESSION_CHAINS['ascii-agents-se'].add((_w('ascii-agent-sup'),INIT))
for _name in ['ibus-memconf','ibus-ui-gtk3','ibus-extension-','ibus-engine-sim']:SESSION_CHAINS.setdefault(_name,set()).add((_w('ibus-daemon'),INIT))
SESSION_CHAINS['gvfsd-trash']={(_w('gvfsd'),MANAGER,INIT)}
SESSION_CHAINS['fusermount3']={(_w('xdg-document-po'),MANAGER,INIT)}
SESSION_CHAINS['dbus-run-sessio']={_LIGHTDM}
SESSION_CHAINS['ascii-desktop']={(_w('dbus-run-sessio'),)+_LIGHTDM}
SESSION_CHAINS['openbox']={(_w('ascii-desktop'),_w('dbus-run-sessio'))+_LIGHTDM}
SESSION_CHAINS['sudo']=set(_AGENT);SESSION_CHAINS['bash']=set(_AGENT)
SESSION_CHAINS={k:frozenset(v) for k,v in SESSION_CHAINS.items()}
# The children these session processes may have. The agent's shell must have none,
# because a shell is also what a startup installer runs, and its sudo may run only
# the agent's root process logger.
SESSION_CHILDREN={'bash':frozenset(),'sudo':frozenset([_r('ascii-proclog')])}
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

def ancestor_chain(pid,rows,manager):
    """The process's ancestors from its parent to pid 1, or None when the chain is broken."""
    uid=profile()['worker_uid'];out=[];seen={pid};current=rows[pid]['ppid']
    while len(out)<16:
        row=rows.get(current)
        if row is None or current in seen:return None
        if current==1:return tuple(out)+(INIT,) if row['uid']==0 and row['comm']=='systemd' else None
        if manager is not None and (current,row['start'])==tuple(manager):
            if row['uid']!=uid or row['ppid']!=1:return None
            out.append(MANAGER)
        else:out.append((row['comm'],'worker' if row['uid']==uid else ('root' if row['uid']==0 else 'other')))
        seen.add(current);current=row['ppid']
    return None

def provider_session(pid,rows,manager=None,depth=0):
    """Whether this process is an allowed session process with an allowed ancestor chain.

    ``manager`` is the (pid, start) of the worker's user manager as the system
    manager reports it, or None when that is unknown, which allows none of its
    children. A name never stands in for pid 1 or the manager.
    """
    row=rows.get(pid);uid=profile()['worker_uid']
    if row is None or row['uid']!=uid or depth>16:return False
    chain=ancestor_chain(pid,rows,manager)
    if chain is None or chain not in SESSION_CHAINS.get(row['comm'],()):return False
    if row['comm'] in SESSION_CHILDREN:
        allowed=SESSION_CHILDREN[row['comm']]
        for child in rows.values():
            if child['ppid']==pid and (child['comm'],'worker' if child['uid']==uid else ('root' if child['uid']==0 else 'other')) not in allowed:return False
    parent=rows[row['ppid']]
    return parent['uid']!=uid or (manager is not None and (parent['pid'],parent['start'])==tuple(manager)) or provider_session(parent['pid'],rows,manager,depth+1)

def user_manager(rows,uid,seconds=10):
    """The worker's user manager by the system manager's own MainPID for user@UID.service, asked within ``seconds``."""
    if seconds<=0:return None
    try:raw=command(['systemctl','show','user@%d.service'%uid,'--property=MainPID'],min(10,seconds)).decode().strip()
    except Exception:return None
    if not re.fullmatch(r'MainPID=[0-9]{1,9}',raw):return None
    row=rows.get(int(raw.split('=',1)[1]))
    if row is None or row['comm']!='systemd' or row['uid']!=uid or row['ppid']!=1:return None
    return (row['pid'],row['start'])

def unallowed_inventory(rows,busy):
    """Name, uid and parent of each busy process, bounded and without command lines."""
    out=[]
    for row in sorted(busy,key=lambda x:x['pid'])[:UNALLOWED_MAX]:
        parent=rows.get(row['ppid'])
        out.append({'pid':row['pid'],'comm':bounded_text(row['comm'],16),'uid':row['uid'],'ppid':row['ppid'],
                    'parent_comm':None if parent is None else bounded_text(parent['comm'],16),'parent_uid':None if parent is None else parent['uid']})
    return out

def foreign_startup(rows,launcher,probe_pid,tracked,manager=None):
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
        if pid in excluded or provider_session(pid,rows,manager):continue
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

class StateChanged(Refusal):
    """A state file was replaced or the state directory changed while it was observed."""

# A restore of the state directory, or an overlay copy-up of a file on its first
# open for writing, gives the same names new inodes. The setup hook writes no
# completion marker, so the observer waits until the directory's entries keep the
# same identity, size and times for SETTLE_SECONDS and then observes again.
SETTLE_SECONDS=5
STATE_ENTRIES=64
REPLACED_READS=('file replaced','public file changed')

def state_snapshot(state):
    """The identity, size and times of the state directory's entries, or None while it is absent.

    The phase directories this package keeps there are excluded and bounded apart
    from the entries that are compared.
    """
    out={};seen=0
    try:entries=os.scandir(state)
    except FileNotFoundError:return None
    with entries:
        for entry in entries:
            seen+=1;need(seen<=8192,'state directory bound')
            if entry.name.startswith('qualification-'):continue
            try:s=entry.stat(follow_symlinks=False)
            except FileNotFoundError:continue
            out[entry.name]=(s.st_dev,s.st_ino,s.st_mode,s.st_nlink,s.st_size,s.st_mtime_ns,s.st_ctime_ns)
            need(len(out)<=STATE_ENTRIES,'state directory bound')
    return out

def settle(state,clock,pause,deadline):
    """Wait, within the deadline, until the state directory is present and unchanged for SETTLE_SECONDS."""
    previous=state_snapshot(state);since=clock()
    while clock()<deadline:
        pause(min(0.5,max(0.0,deadline-clock())));current=state_snapshot(state)
        if current is None or current!=previous:previous,since=current,clock()
        elif clock()-since>=SETTLE_SECONDS:return clock()<deadline
    return False

@contextlib.contextmanager
def bootstrap_lock(path,uid,denied):
    s=path.lstat();need(stat.S_ISREG(s.st_mode) and s.st_uid==uid and s.st_nlink==1 and stat.S_IMODE(s.st_mode)==0o600 and s.st_size==0,'bootstrap lock ownership')
    need((s.st_dev,s.st_ino)!=denied,'bootstrap lock token alias')
    fd=os.open(path,os.O_RDWR|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        current=os.fstat(fd)
        if (current.st_dev,current.st_ino,current.st_uid,current.st_mode,current.st_nlink,current.st_size)!=(s.st_dev,s.st_ino,s.st_uid,s.st_mode,s.st_nlink,s.st_size):raise StateChanged('bootstrap lock replaced')
        fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        yield {'path':str(path),'device':s.st_dev,'inode':s.st_ino,'uid':uid,'mode':0o600,'held_during_checkpoint':True,'released_after_checkpoint':True}
        after=path.lstat()
        if (after.st_dev,after.st_ino,after.st_nlink,after.st_size)!=(s.st_dev,s.st_ino,1,0):raise StateChanged('bootstrap lock changed during observation')
    finally:os.close(fd)

def checkpoint_at(state,uid,denied,launcher,probe_pid,expected_manifest,seconds,rows_reader=process_rows,clock=time.monotonic,pause=time.sleep,manager_reader=lambda rows,left:None):
    """Observe startup until it is closed or the observation deadline passes.

    A replaced bootstrap lock or manifest, or a state directory that changes
    during an attempt, means startup is still running. The observer then waits for
    the directory to settle and observes again, and refuses only at the deadline,
    with the reason code of the last thing it saw.
    """
    deadline=clock()+seconds;tracked=set()
    last='not observed';code='startup_unobserved';rows={};busy=[];unsettled=False
    def wait(n):pause(min(n,max(0.0,deadline-clock())))
    def scan():
        # The manager query gets only the time left, and none once it is gone.
        found=rows_reader();left=deadline-clock()
        return found,foreign_startup(found,launcher,probe_pid,tracked,manager_reader(found,left) if left>0 else None)
    def changed(reason):
        nonlocal last,code,busy,unsettled
        last=reason;busy=[];unsettled=True
        code='startup_busy:bootstrap_lock_replaced' if 'bootstrap lock' in reason else 'startup_busy:state_restore_in_progress'
    while clock()<deadline:
        if unsettled:
            if not settle(state,clock,pause,deadline):break
            unsettled=False
            if clock()>=deadline:break
        try:
            before=state_snapshot(state)
            if before is None:raise StateChanged('state directory changed during observation')
            with bootstrap_lock(state/'bootstrap.lock',uid,denied) as lock:
                manifest,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                # The actual pinned hook has no manifest deletion branch. If not
                # yet at its absence test it now skips bootstrap; a running
                # bootstrap still owns fd9/flock until its waited children end.
                rows,busy=scan()
                if busy:last='live or unknown startup descendants';code='startup_busy:unallowed_processes';wait(0.5);continue
                wait(0.5)
                if clock()>=deadline:break
                rows,busy=scan()
                current,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
                if current!=manifest:raise StateChanged('startup manifest path/bytes changed')
                if busy:last='live or unknown startup descendants';code='startup_busy:unallowed_processes';continue
                held=(state/'bootstrap.lock').lstat()
                if (held.st_dev,held.st_ino)!=(lock['device'],lock['inode']):raise StateChanged('bootstrap lock changed during observation')
                after=state_snapshot(state)
                if after is None or after!=before:raise StateChanged('state directory changed during observation')
                # A success observed after the deadline does not count.
                if clock()>=deadline:break
                return {'startup_closed':True,'installer_quiescent':True,'manifest_sha256':manifest['sha256'],'qualification_eligible':manifest['sha256']==expected_manifest,'bootstrap_lock':lock,'bootstrap_occurrence':'unknown','hook_terminal_observed':False,'closure_basis':'existing bootstrap fd9 lock, stable owned present manifest, a state directory unchanged across the attempt and repeated exact PID/start/descendant observations; any not-yet-entered pinned absence branch must skip bootstrap','unknown_or_live_descendants':[],'engine_or_job_started':False}
        except StateChanged as error:changed(str(error))
        except BlockingIOError:last='bootstrap lock busy or manifest not yet present';code='startup_busy:bootstrap_lock_held';busy=[];wait(0.5)
        except FileNotFoundError:last='bootstrap lock busy or manifest not yet present';code='startup_busy:lock_or_manifest_absent';busy=[];wait(0.5)
        except Refusal as error:
            if str(error) not in REPLACED_READS:raise
            changed(str(error))
    return {'startup_closed':False,'installer_quiescent':False,'qualification_eligible':False,'bootstrap_occurrence':'unknown','reason':last,'reason_code':code,
            'unallowed':unallowed_inventory(rows,busy),'unallowed_count':len(busy),'engine_or_job_started':False}

def own_cgroup_memory(unit,proc_cgroup=Path('/proc/self/cgroup'),root=Path('/sys/fs/cgroup')):
    """The phase unit's memory and swap peaks, read from its own cgroup just before it exits.

    systemd unloads a finished transient unit and then reports no peaks, so the
    launcher takes them from here. The kernel's memory and swap limits still bound
    the few writes that follow this read."""
    try:
        line=proc_cgroup.read_text().strip();need(line.startswith('0::'),'phase cgroup')
        cg=root/line[3:].lstrip('/');need(cg.name==unit,'phase unit cgroup')
        peak=(cg/'memory.peak').read_text().strip();swap=(cg/'memory.swap.peak').read_text().strip()
        need(peak.isdigit() and swap.isdigit(),'phase memory peaks')
        return {'source':'cgroup','cgroup':line[3:],'memory_peak':int(peak),'swap_peak':int(swap)}
    except Exception as error:return {'source':'unavailable','reason_code':reason_code(error)}

def run_phase(payload,observe,qualify,clock=time.monotonic):
    """One remote phase's result. Every refusal carries a bounded reason code."""
    started=clock()
    try:result=observe(payload) if payload['phase']!='metadata' else qualify(payload)
    except Exception as error:
        result={'qualified':False,'startup_closed':False,'engine_or_job_started':False,'failure':'bounded phase refused; no arbitrary error content','reason_code':reason_code(error)}
        # A source drift names each recorded field beside the observed one.
        drift=drift_evidence(getattr(error,'source_drift',None))
        if drift is not None:result['source_drift']=drift
    closed=result.get('startup_closed')is True if payload['phase']!='metadata' else result.get('qualified')is True
    if not closed:result.setdefault('reason_code','unreported')
    result['elapsed_seconds']=round(clock()-started,3)
    return result,closed

def startup_checkpoint(payload,seconds):
    home=Path.home();uid=os.getuid();need(uid==profile()['worker_uid'] and home==Path(profile()['worker_home']),'startup worker user')
    token=token_metadata(home/'.config/mbx/remote-token',uid);denied=(token['device'],token['inode']);state=home/'.local/state/sparkles-boat'
    return checkpoint_at(state,uid,denied,payload['launcher_identity'],os.getpid(),payload['pins']['saved_toolchain_manifest_sha256'],seconds,manager_reader=lambda rows,left:user_manager(rows,uid,left))

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
        rows=process_rows();need(not foreign_startup(rows,payload['launcher_identity'],os.getpid(),tracked,user_manager(rows,uid)),'startup not quiescent before tools')
        result=probe(payload)
        after,_=safe_file(state/'toolchain.json',uid,denied,1024*1024)
        rows=process_rows();need(after==before and not foreign_startup(rows,payload['launcher_identity'],os.getpid(),tracked,user_manager(rows,uid)),'startup/source drift during qualification')
        result.update({'startup_closed':True,'bootstrap_lock':lock,'bootstrap_occurrence':'unknown','actual_toolchain_manifest':{'path':str(state/'toolchain.json'),'sha256':before['sha256']},'runtime_env_file':None})
        return result
