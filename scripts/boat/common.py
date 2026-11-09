"""Narrow qualification primitives; imports perform no actions."""
from pathlib import Path
import hashlib,json,os,re,stat,time

_PROFILE=None
def bind_profile(p):
    validator=globals().get('validate_profile')
    if validator is None:
        from config_schema import validate_profile as validator
    global _PROFILE
    _PROFILE=None
    _PROFILE=validator(p)
    return _PROFILE
def profile():
    if _PROFILE is None:raise ValueError('explicit owned-worker profile not bound')
    return _PROFILE
def worker_id():return None if _PROFILE is None else _PROFILE['worker']
def hook_sha():return profile()['future_environment']['hook_sha256']
def manifest_sha():return profile()['saved_toolchain_manifest_sha256']
MEMORY=536870912
class Refusal(Exception): pass
def need(ok,reason):
    if not ok: raise Refusal(reason)
REASON_MAX=160
def bounded_text(text,limit=REASON_MAX):
    """Printable ASCII of at most ``limit`` characters, other characters as '?'."""
    return ''.join(c if ' '<=c<='~' else '?' for c in str(text)[:limit])
def reason_code(error):
    """A bounded code for a refusal that never carries arbitrary error text.

    A Refusal's message is one of this packet's own literal reasons and is kept.
    Any other exception gives only its type and the function and line where it
    was raised, so a path, provider response or credential cannot leak into a
    receipt.
    """
    if isinstance(error,Refusal):return bounded_text('refusal:'+str(error))
    tb=error.__traceback__;frame=None
    while tb is not None:frame=tb;tb=tb.tb_next
    where='' if frame is None else ':%s:%d'%(frame.tb_frame.f_code.co_name,frame.tb_lineno)
    return bounded_text('unexpected:'+type(error).__name__+where)
def digest(data): return hashlib.sha256(data).hexdigest()
def sha(value):return isinstance(value,str) and re.fullmatch('[a-f0-9]{64}',value) is not None
def read_owned(path,limit=1024*1024):
    path=Path(path);need(path.is_absolute() and path.resolve()==path,'noncanonical local path')
    fd=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    try:
        s=os.fstat(fd);need(stat.S_ISREG(s.st_mode) and s.st_uid==os.getuid() and s.st_nlink==1 and not s.st_mode&0o022 and s.st_size<=limit,'unsafe local receipt')
        data=os.read(fd,limit+1);a=path.lstat()
        need(len(data)==s.st_size and (a.st_dev,a.st_ino,a.st_mtime_ns,a.st_size)==(s.st_dev,s.st_ino,s.st_mtime_ns,s.st_size),'local receipt changed')
        return data
    finally:os.close(fd)
def open_lock(path):
    """Open or create a private lock file, refusing one of another owner, mode or link count."""
    fd=os.open(path,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW|os.O_CLOEXEC,0o600)
    try:
        s=os.fstat(fd);need(stat.S_ISREG(s.st_mode) and s.st_uid==os.getuid() and s.st_nlink==1 and not s.st_mode&0o077,'unsafe lock file')
    except BaseException:
        os.close(fd);raise
    return fd
def load(path):return json.loads(read_owned(path))
def write_new(root,name,data):
    # Linux/systemd qualification: renameat2(RENAME_NOREPLACE) publishes complete
    # singleton bytes atomically. No reader can observe a half-written final file.
    import ctypes,errno
    need(re.fullmatch('[a-z0-9_.-]+',name) is not None and name not in ('.','..'),'unsafe receipt name')
    root=Path(root);need(root.resolve()==root,'receipt root canonical')
    rs=root.lstat();need(stat.S_ISDIR(rs.st_mode) and rs.st_uid==os.getuid() and not rs.st_mode&0o022,'receipt root ownership')
    body=(json.dumps(data,sort_keys=True,indent=2)+'\n').encode();need(len(body)<=1024*1024,'receipt bound')
    temporary=root/('.pending-'+name)
    fd=os.open(temporary,os.O_WRONLY|os.O_CREAT|os.O_EXCL|os.O_NOFOLLOW,0o600)
    with os.fdopen(fd,'wb') as f:f.write(body);f.flush();os.fsync(f.fileno())
    libc=ctypes.CDLL(None,use_errno=True)
    rename=libc.renameat2;rename.argtypes=[ctypes.c_int,ctypes.c_char_p,ctypes.c_int,ctypes.c_char_p,ctypes.c_uint];rename.restype=ctypes.c_int
    if rename(-100,os.fsencode(temporary),-100,os.fsencode(root/name),1)!=0:
        code=ctypes.get_errno()
        # Preserve the pending evidence on failure; never replace an old final.
        if code==errno.EEXIST:raise FileExistsError(code,'receipt already exists')
        raise Refusal('atomic receipt publication unavailable')
    fd=os.open(root,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
    try:os.fsync(fd)
    finally:os.close(fd)

def timestamp(value):
    from datetime import datetime
    need(isinstance(value,str),'missing provider time')
    try:return datetime.fromisoformat(value.replace('Z','+00:00')).timestamp()
    except ValueError:raise Refusal('invalid provider time') from None
def safe_info(body):
    need(isinstance(body,dict) and isinstance(body.get('sandbox'),dict),'unknown info envelope')
    s=body['sandbox'];keys=['id','access','environment','environmentVersion','state','type','vcpu','memoryGB','ttlSeconds','archiveAfter','setupStatus','snapshotAvailable','snapshotCompletedAt','lastSnapshotStatus','stoppedAt']
    out={k:s.get(k) for k in keys}
    out['setup_error_present']=s.get('setupError') not in (None,'')
    stop=s.get('stop')
    if isinstance(stop,dict):
        out['stop']={k:stop.get(k) for k in ['id','status','endedAt','forced']}
        out['stop']['error']=None if stop.get('error') is None else 'REDACTED_PRESENT'
    return out

def identity(info):
    need(info['id']==worker_id() and info['access']=='owner','worker ownership mismatch')
    need(info['environment']==profile()['future_environment']['name'] and info['type']=='large' and info['vcpu']==8 and info['memoryGB']==16,'worker shape mismatch')
def stopped(info):
    identity(info)
    need(info['state']=='stopped' and info['snapshotAvailable'] is True and info['lastSnapshotStatus']=='completed','stop/snapshot not confirmed')
    need(info['snapshotCompletedAt'] is not None and info['stoppedAt'] is not None,'missing snapshot/stop times')
def freshness(row,now,max_age=300):
    need(isinstance(row,dict) and type(row.get('observed_unix')) in (int,float),'missing Root fact time')
    need(0<=now-row['observed_unix']<=max_age,'stale Root fact')
    need(row.get('verified_by_Root') is True and sha(row.get('receipt_sha256')),'unbound Root fact')
def source_shape(row):
    need(isinstance(row,dict) and set(row)=={'head','status_sha256','tracked_diff_sha256','inputs'},'missing source observation')
    need(re.fullmatch('[0-9a-f]{40}',row['head']) is not None,'source HEAD')
    need(sha(row['status_sha256']) and sha(row['tracked_diff_sha256']),'source status/diff')
    need(set(row['inputs'])=={'mise.toml','rust-toolchain.toml'} and all(sha(x) for x in row['inputs'].values()),'source inputs')
def validate_release(r,now):
    need(r.get('schema')==1 and r.get('authorized_by_Root') is True,'release is false')
    need(r.get('worker')==worker_id(),'release worker')
    need(sha(r.get('profile_sha256'))and r.get('profile_review_sha256')==profile()['profile_review_sha256'],'explicit profile/review binding')
    need(sha(r.get('controller_packet_manifest_sha256')) and sha(r.get('different_controller_review_sha256')),'missing concrete controller review')
    need(r['different_controller_review_sha256']!=r['controller_packet_manifest_sha256'],'review identity')
    need(r.get('no_source_transport_or_jobs') is True,'scope changed')
    need(r.get('startup_mode')=='normal_pinned_provisioning','startup mode unavailable')
    need(r.get('private_credential_snapshot_acknowledged') is True and r.get('clean_next_snapshot_required') is False,'private snapshot contract')
    need(r.get('root_exclusive_worker_use') is True,'worker not exclusive')
    for key in ['local_first','start_ledger','environment','snapshot_continuity','previous_native_closure','provider_contract']:
        freshness(r[key],now)
    book=r['start_ledger'];need(book.get('limit')==200 and type(book.get('starts_and_reservations')) is int and 0<=book['starts_and_reservations']<=200,'shared start bound')
    need(book.get('this_one_resume_reserved') is True and book.get('all_actors_included') is True and bool(book.get('window')) and bool(book.get('timezone')) and book.get('provider_rate_admitted') is True,'shared ledger incomplete')
    # A qualification run needs no current overflow when the release binds the
    # maintainer's recorded confirmation of that purpose by its hash.
    lf=r['local_first']
    need(lf.get('overflow_currently_needed') is True or (lf.get('overflow_currently_needed') is False and lf.get('purpose')=='qualification' and sha(lf.get('maintainer_confirmation_sha256'))),'local capacity available or unknown')
    e=r['environment'];freshness(e,now,30);need(e.get('version')==profile()['future_environment']['version'] and e.get('hook_sha256')==hook_sha() and e.get('name')==profile()['future_environment']['name'],'latest ENV drift')
    need(e.get('environment_id')==profile()['future_environment']['id'],'environment identity drift')
    need(e.get('assigned_editors_excluded') is True,'no environment edit exclusion')
    need(e.get('latest_version_id')==profile()['future_environment']['version_id'],'latest version identity drift')
    need(isinstance(e.get('lock_path'),str) and e['lock_path'].startswith('/'),'environment lock path')
    s=r['snapshot_continuity'];need(s.get('manifest_sha256')==manifest_sha() and s.get('continuity_is_retained_evidence_inference') is True and r.get('startup_mode')=='normal_pinned_provisioning','bootstrap continuity missing')
    need(s.get('startup_repo')==profile()['startup_repositories']['default'] and s.get('hook_token_metadata_open_fchmod_accepted') is True,'startup scope not accepted')
    need(s.get('token_path')==profile()['worker_home']+'/.config/mbx/remote-token' and s.get('dev_read_only_scope') is True,'token metadata scope')
    source_shape(s['default_source']);source_shape(s['retained_source'])
    need(r['previous_native_closure'].get('all_native_and_leases_closed') is True,'live native/lease')
    pc=r['provider_contract'];need(pc.get('readiness_predicate')=='CLI_ready_idle_running_plus_actual_probe' and pc.get('ttl_basis')=='accepted_resume_ttl10800_and_local_earlier_deadline','provider contract not verified')
    need(pc.get('stop_completed_requires_fresh_info') is True and pc.get('snapshot_policy') in ('fresh_since_request_and_newer_than_pre_resume','documented_normal_stop_final_snapshot'),'stop freshness policy missing')
    ack=pc.get('resume_ack',{});need(ack.get('verified_by_Root') is True and sha(ack.get('receipt_sha256')) and ack.get('kind')=='cli_accepted_resuming','resume acknowledgement schema unbound')
    need(ack.get('positive_literal')=='resuming','resume acknowledgement literal missing')
    need(pc.get('stop_status_schema_verified_by_Root')is True and sha(pc.get('stop_status_receipt_sha256')),'actual stop-status schema unbound')
    if pc['snapshot_policy']=='documented_normal_stop_final_snapshot':need(pc.get('final_snapshot_document_sha256')==profile()['stop_contract_sha256'],'normal-stop final snapshot guarantee unbound')
    b=r['boat'];need(isinstance(b.get('path'),str) and re.fullmatch('/[A-Za-z0-9_./+-]+',b['path']) and sha(b.get('sha256')) and b.get('version')=='1.0.92','CLI binding')
    need(isinstance(r.get('operation'),str) and re.fullmatch('/[A-Za-z0-9_./+-]+',r['operation']),'operation path')
    need(re.fullmatch('boat-owned-warm-qual-[a-z0-9-]{1,40}',r.get('guard_unit','')) is not None,'guard unit name')
    return r

def resumed(info,r,intent_wall):
    identity(info);need(info['environmentVersion']==profile()['future_environment']['version'] and info['ttlSeconds']==10800,'resumed ENV/TTL mismatch')
    need(info['state'] in ('ready','idle','running'),'worker does not yet accept commands')
    need(not info.get('setup_error_present') and info['setupStatus'] not in ('failed','error'),'startup explicit failure')
    # archiveAfter may be null; no guessed semantics or fabricated provider expiry.
    # CLI help explicitly sets resumed lifetime. Our earlier local10500 deadline
    # is the conservative bound on the accepted10800 request, not an observed expiry.
    return intent_wall+10500

def stop_complete(info,operation_id,requested_wall,pre_resume_snapshot=None,snapshot_policy='fresh_since_request_and_newer_than_pre_resume'):
    stopped(info);s=info.get('stop',{})
    need(isinstance(operation_id,str) and operation_id.startswith('stop_') and s.get('id')==operation_id,'stop operation not tied')
    need(s.get('status')=='completed' and s.get('error') is None and s.get('forced') is False,'stop operation incomplete')
    need(timestamp(s.get('endedAt'))>=requested_wall-2 and timestamp(info['stoppedAt'])>=requested_wall-2,'stale stop completion')
    if snapshot_policy=='fresh_since_request_and_newer_than_pre_resume':
        need(timestamp(info['snapshotCompletedAt'])>=requested_wall-2,'snapshot predates this stop')
        if pre_resume_snapshot is not None:need(timestamp(info['snapshotCompletedAt'])>timestamp(pre_resume_snapshot),'snapshot not newer than pre-resume snapshot')
    else:
        need(snapshot_policy=='documented_normal_stop_final_snapshot','unknown snapshot policy')
        need(pre_resume_snapshot is not None and timestamp(info['snapshotCompletedAt'])>timestamp(pre_resume_snapshot),'old snapshot cannot prove this resumed lifetime')

def budget(now_mono,deadline,seconds):need(now_mono+seconds+150<deadline,'phase exceeds remaining lifetime/stop reserve')

def systemd_seconds(text):
    need(isinstance(text,str) and text and text!='infinity','missing finite systemd duration')
    factors={'d':86400,'h':3600,'min':60,'s':1,'ms':0.001,'us':0.000001}
    tokens=re.findall(r'([0-9]+(?:\.[0-9]+)?)(min|ms|us|d|h|s)',text)
    need(tokens and ''.join(n+u for n,u in tokens)==text.replace(' ',''),'unknown systemd duration')
    return sum(float(n)*factors[u] for n,u in tokens)

def positive_resume_ack(result,contract):
    if result.get('exit')!=0 or result.get('error')is not None:return False
    b=result.get('body');need(contract.get('verified_by_Root') is True and sha(contract.get('receipt_sha256')),'ack schema unbound')
    if not isinstance(b,dict):return False
    return contract.get('kind')=='cli_accepted_resuming' and b.get('event')=='accepted' and b.get('id')==worker_id() and b.get('status')=='resuming'

UNITS={'B':1,'K':1024,'M':1024**2,'G':1024**3,'T':1024**4}
SUMMARY_PEAK=re.compile(r'Memory peak: ([0-9]{1,6})(?:\.([0-9]))?([BKMGT]) \(swap: ([0-9]{1,6})(?:\.([0-9]))?([BKMGT])\)')
def summary_bound(whole,tenth,unit):
    """The largest byte count systemd's one-decimal size text can stand for."""
    if unit=='B':
        need(tenth is None,'phase memory summary');return int(whole)
    return (int(whole)*10+int(tenth or 0)+1)*UNITS[unit]//10
def summary_peaks(text):
    """Upper bounds of the memory and swap peaks in a `systemd-run --wait` summary.

    systemd truncates each size to one decimal place, so the bound is the next
    tenth. The summary is the last matching line. The peaks are unknown when no
    line names both values."""
    if not isinstance(text,str):return None
    rows=[SUMMARY_PEAK.fullmatch(line.strip()) for line in text.splitlines()[-64:]]
    rows=[m for m in rows if m]
    if not rows:return None
    m=rows[-1]
    try:return {'source':'systemd_run_summary','memory_peak':summary_bound(*m.group(1,2,3)),'swap_peak':summary_bound(*m.group(4,5,6))}
    except Refusal:return None
def cgroup_peaks(result,loaded):
    """The peaks the phase read from its own cgroup, when they are bound to this unit."""
    row=result.get('phase_memory') if isinstance(result,dict) else None
    if not isinstance(row,dict) or row.get('source')!='cgroup':return None
    if type(row.get('memory_peak'))is not int or type(row.get('swap_peak'))is not int or row['memory_peak']<0 or row['swap_peak']<0:return None
    cg=loaded.get('ControlGroup','')
    if cg and cg!=row.get('cgroup'):return None
    return {'source':'cgroup','memory_peak':row['memory_peak'],'swap_peak':row['swap_peak']}
def terminal_health(result,loaded,closed,run_exit=None,summary=None):
    """Whether a finished phase unit ended healthy, with a bounded reason and its evidence.

    The unit must have closed with status 0, a memory peak within the cap and no
    swap. The memory peaks come from the phase's own cgroup read before it
    exited, and otherwise from the `systemd-run --wait` summary. systemd unloads a
    finished transient unit, and an unloaded unit reports default values: Result
    success, status 0 and no peaks. Its status therefore comes only from
    systemd-run's exit code, which is the unit's own. Peaks that the unit still
    reports must also be within the cap."""
    if not closed:return False,'unit_unhealthy:open',{}
    unloaded=loaded.get('LoadState')=='not-found'
    if unloaded:
        if run_exit is None:return False,'unit_unhealthy:unloaded:status_unknown',{}
        if run_exit!=0:return False,'unit_unhealthy:unloaded:exit-%d'%run_exit if type(run_exit)is int and 0<=run_exit<256 else 'unit_unhealthy:unloaded:exit-unknown',{}
    elif loaded.get('Result')!='success' or loaded.get('ExecMainStatus')!='0':
        return False,'unit_unhealthy:'+bounded_text(loaded.get('Result','unknown'),32)+':'+bounded_text(loaded.get('ExecMainStatus','unknown'),12),{}
    peaks=cgroup_peaks(result,loaded) or summary_peaks(summary)
    if peaks is None:return False,'unit_unhealthy:memory_peak_unavailable',{}
    evidence=dict(peaks,terminal_source='systemd_run_exit' if unloaded else 'unit')
    for key,value in (('MemoryPeak',peaks['memory_peak']),('MemorySwapPeak',peaks['swap_peak'])):
        own=loaded.get(key,'')
        if own.isdigit():evidence['unit_'+key]=int(own)
    if max(peaks['memory_peak'],evidence.get('unit_MemoryPeak',0))>MEMORY:return False,'unit_unhealthy:memory_peak_over_cap',evidence
    if max(peaks['swap_peak'],evidence.get('unit_MemorySwapPeak',0))!=0:return False,'unit_unhealthy:swap_used',evidence
    return True,None,evidence
