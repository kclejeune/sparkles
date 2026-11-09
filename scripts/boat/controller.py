#!/usr/bin/env python3
"""Root-only one-worker qualification. Inert tests inject IO; import is passive."""
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
from common import *
identity_check=identity
import qualified_worker_admission as lifetime
from qualified_worker_admission import make_admission
import owned_group
import argparse,ast,fcntl,importlib.util,math,secrets,selectors,signal,subprocess,time,shlex,base64

# An extension act publishes within EXTEND_ACT_MAX seconds of its start, and the
# guard waits up to FREEZE_WAIT for it before it stops at its deadline. The guard
# unit outlives the longest permitted lifetime, extensions included, by that wait
# and the stop margin. The provider TTL remains the worker's own backstop.
EXTEND_ACT_MAX=90
FREEZE_WAIT=100
GUARD_RUNTIME=lifetime.MAX_LIFETIME+lifetime.STOP_MARGIN+FREEZE_WAIT
EXTENSION_LOCK='lifetime-extension.lock'
# Every Boat command holds this lock shared from its admission until it exits.
JOB_LOCK='jobs.lock'

ROOT=Path(__file__).resolve().parent

# The closure watch checks a warm worker's jobs WATCH_LEAD seconds before the
# guard's present end, retries every WATCH_POLL seconds and gives up WATCH_FLOOR
# seconds before that end, which leaves the guard's own stop path in place.
WATCH_LEAD=900
WATCH_POLL=30
WATCH_FLOOR=60
WATCH_CHECKS=400
WATCH_ACCEPT=15
WATCH_DECISION=90
# A remote phase's reason code is kept only in one of these forms. A refusal
# code must name one of the packet's own literal refusal reasons.
FIXED_REASONS=frozenset({'startup_unobserved','startup_busy:unallowed_processes','startup_busy:bootstrap_lock_held','startup_busy:lock_or_manifest_absent','startup_busy:bootstrap_lock_replaced','startup_busy:state_restore_in_progress','existing_phase_pending','unreported'})
def packet_refusals():
    found=set()
    for name in ('common.py','config_schema.py','metadata-probe.py','startup-observer.py','remote-launcher.py','diagnostic-probe.py'):
        for node in ast.walk(ast.parse((ROOT/name).read_text())):
            if isinstance(node,ast.Call) and isinstance(node.func,ast.Name) and node.func.id in ('need','require','Refusal'):
                for arg in node.args:
                    if isinstance(arg,ast.Constant) and isinstance(arg.value,str):found.add(arg.value)
    return frozenset(found)
def remote_reason(code):
    """The code itself when it has a known form, otherwise 'malformed'."""
    if not isinstance(code,str):return 'unreported' if code is None else 'malformed'
    if code in FIXED_REASONS:return code
    if re.fullmatch(r'(launcher:)?unexpected:[A-Za-z_][A-Za-z0-9_]{0,63}(:[A-Za-z_<>][A-Za-z0-9_<>]{0,63}:[0-9]{1,7})?',code):return code
    if re.fullmatch(r'unit_unhealthy:(open|[a-z-]{1,32}:[0-9a-z]{1,12})',code):return code
    if re.fullmatch(r'unit_unhealthy:(memory_peak_unavailable|memory_peak_over_cap|swap_used|unloaded:status_unknown|unloaded:exit-([0-9]{1,3}|unknown))',code):return code
    m=re.fullmatch(r'(launcher:)?refusal:(.{1,150})',code)
    if m and m[2] in {bounded_text('refusal:'+x)[len('refusal:'):] for x in packet_refusals()}:return code
    return 'malformed'
def remote_name(value):
    return value if isinstance(value,str) and re.fullmatch(r'[ -~]{1,16}',value) else None

# Every phase keeps its first refusal in <phase>-refused.json through record_refusal.
REFUSAL_PHASES=('startup','metadata','admission','stopcheck','closure')
# The fixed names of the metadata checks, in the order the controller applies them.
METADATA_CHECKS=('metadata_not_closed','probe_not_qualified','startup_not_closed','engine_or_job_started','memory_cap','swap_cap','manifest_changed','worker_boot_id')
def metadata_failures(q):
    """The names of every metadata check that a phase result fails, in check order."""
    q=q if isinstance(q,dict) else {}
    failed={'metadata_not_closed':q.get('metadata_closed')is not True,'probe_not_qualified':q.get('qualified')is not True,'startup_not_closed':q.get('startup_closed')is not True,
            'engine_or_job_started':q.get('engine_or_job_started')is not False,'memory_cap':q.get('memory_max')!=str(MEMORY),'swap_cap':q.get('swap_max')!='0',
            'manifest_changed':q.get('manifest_unchanged')is not True,'worker_boot_id':not(isinstance(q.get('worker_boot_id'),str) and re.fullmatch(lifetime.UUID,q['worker_boot_id']) is not None)}
    return [name for name in METADATA_CHECKS if failed[name]]

def bounded_int(value):return value if type(value)is int and 0<=value<2**63 else None
def phase_evidence(result):
    """The bounded terminal evidence of a phase result, never its free text."""
    if not isinstance(result,dict):return {}
    terminal=result.get('terminal_properties') if isinstance(result.get('terminal_properties'),dict) else {}
    def row(x):
        return {'pid':bounded_int(x.get('pid')),'comm':remote_name(x.get('comm')),'uid':bounded_int(x.get('uid')),
                'ppid':bounded_int(x.get('ppid')),'parent_comm':remote_name(x.get('parent_comm')),'parent_uid':bounded_int(x.get('parent_uid'))}
    rows=result.get('unallowed')
    out={'phase_closed':result.get('phase_closed') is True,'existing_phase_pending':result.get('existing_phase_pending') is True,
         'unallowed':[row(x) for x in rows[:32] if isinstance(x,dict)] if isinstance(rows,list) else [],
         'unallowed_count':bounded_int(result.get('unallowed_count')),
         'elapsed_seconds':result.get('elapsed_seconds') if type(result.get('elapsed_seconds'))in(int,float) and 0<=result['elapsed_seconds']<10**6 else None,
         'terminal':{k:terminal[k] for k in ('ExecMainStatus','Result','LoadState','ActiveState') if isinstance(terminal.get(k),str) and re.fullmatch(r'[a-z0-9-]{1,32}',terminal[k])}}
    evidence={}
    for name in ('terminal_evidence','phase_memory'):
        x=result.get(name)
        if isinstance(x,dict):
            evidence[name]={k:bounded_int(x.get(k)) for k in ('memory_peak','swap_peak','unit_MemoryPeak','unit_MemorySwapPeak') if k in x}
            for k in ('source','terminal_source'):
                if isinstance(x.get(k),str) and re.fullmatch(r'[a-z_]{1,32}',x[k]):evidence[name][k]=x[k]
            if 'reason_code' in x:evidence[name]['reason_code']=remote_reason(x['reason_code'])
    events=result.get('memory_events')
    if isinstance(events,dict):evidence['memory_events']={k:v for k,v in list(events.items())[:16] if isinstance(k,str) and re.fullmatch(r'[a-z_]{1,32}',k) and isinstance(v,str) and re.fullmatch(r'[0-9]{1,20}',v)}
    out['evidence']=evidence
    return out

def record_refusal(io,phase,error=None,result=None,code=None,failed=()):
    """Keep the first refusal of ``phase`` with its bounded reason and terminal evidence.

    The reason is the local refusal's code when there is one, then a fixed code the
    caller names, then the remote phase's own code. The remote code is kept beside it.
    Recording never masks the refusal it describes, so its errors are swallowed."""
    try:
        need(phase in REFUSAL_PHASES,'unknown refusal phase')
        name=phase+'-refused.json'
        if io.exists(name):return
        remote=remote_reason(result.get('reason_code')) if isinstance(result,dict) else None
        reason=reason_code(error) if error is not None else (bounded_text(code) if isinstance(code,str) else (remote or 'unreported'))
        row={'phase':phase,'reason_code':reason,'remote_reason_code':remote,'failed_checks':[x for x in failed if x in METADATA_CHECKS],'observed_wall':io.wall()}
        row.update(phase_evidence(result))
        io.record(name,row)
    except Exception:pass

# Job work on the worker. A unit named for a screen, build or gate is job work,
# and so is any process the startup observer would count, which is every process
# of the worker's uid outside the systemd and dbus infrastructure and the allowed
# provider session, every installer, shell or build tool and every descendant of
# those. That covers detached work such as a partition share under setsid. An mbx
# lease lives only as long as its mbx process, so the process check covers it.
JOB_UNIT_PREFIXES=('sparkles-screen-','nsc-build-','gate-')
JOB_CLOSURE_ENTRY='''
if __name__=='__main__':
    bind_profile(json.loads(sys.argv[1]));prefixes=tuple(json.loads(sys.argv[2]));uid=os.getuid()
    need(uid==profile()['worker_uid'],'job closure worker user')
    r=subprocess.run(['systemctl','--user','list-units','--all','--plain','--no-legend','--state=active,activating,deactivating,reloading'],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10)
    need(len(r.stdout)<=1048576,'unit listing bound')
    units=sorted(x.split()[0] for x in r.stdout.decode(errors='replace').splitlines() if x.split() and x.split()[0].startswith(prefixes))
    rows=process_rows();me=os.getpid()
    busy=foreign_startup(rows,{'pid':me,'start':rows[me]['start']},me,set(),user_manager(rows,uid))
    print(json.dumps({'list_exit':r.returncode,'units':[bounded_text(u,64) for u in units[:32]],'unit_count':len(units),
                      'processes':unallowed_inventory(rows,busy),'process_count':len(busy),'boot_id':open('/proc/sys/kernel/random/boot_id').read().strip()}))
'''

def bounded(argv,seconds,input_bytes=None,limit=None):
    """Only Root's real adapter calls this. Kill only this new process group.

    ``limit``, when given, returns the seconds left before an outer end, such as
    the original wall and BOOTTIME lifetime. It is checked with the monotonic
    window throughout, so a suspend or wall jump during the call is counted.

    SIGINT is recorded from before the child starts. The loop hands it to the
    previous handler at its checks, and owned_group.finish counts it during
    teardown, closes the pipes and selector unconditionally and never extends its
    window. A teardown cancellation propagates after closure, and a primary
    exception keeps its type with the count attached.
    """
    cancel=owned_group.Cancellation().install()
    try:child=subprocess.Popen(argv,stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,start_new_session=True)
    except BaseException:
        cancel.restore();raise
    select=None;failure=None
    try:
        select=selectors.DefaultSelector();select.register(child.stdout,selectors.EVENT_READ)
        if input_bytes is not None:
            os.set_blocking(child.stdin.fileno(),False);select.register(child.stdin,selectors.EVENT_WRITE)
        data=bytearray();position=0;end=time.monotonic()+seconds;reason=None
        def left():
            value=end-time.monotonic()
            return value if limit is None else min(value,limit())
        while select.get_map():
            cancel.check()
            if left()<=0:reason='timeout';break
            for key,mask in select.select(min(0.2,max(0,left()))):
                if mask&selectors.EVENT_WRITE:
                    try:position+=os.write(key.fd,input_bytes[position:position+4096])
                    except BrokenPipeError:position=len(input_bytes)
                    if position==len(input_bytes):select.unregister(key.fileobj);child.stdin.close()
                else:
                    part=os.read(key.fd,16384)
                    if not part:select.unregister(key.fileobj)
                    else:
                        data.extend(part)
                        if len(data)>1024*1024:reason='output_bound';break
            if reason:break
        if reason:result={'exit':None,'error':reason,'body':None}
        else:
            code=None
            while code is None:
                cancel.check();rest=left()
                if rest<=0:break
                try:code=child.wait(timeout=min(0.2,rest))
                except subprocess.TimeoutExpired:
                    if rest<=0.2:break
            # A completion observed after the window or outer end is not success.
            if code is not None and left()<=0:code=None
            if code is None:result={'exit':None,'error':'timeout','body':None}
            else:
                try:body=json.loads(data)
                except (ValueError,UnicodeError):body=None
                result={'exit':code,'error':None,'body':body}
    except BaseException as error:
        failure=error;raise
    finally:
        prelude=(select.close,) if select is not None else ()
        problems,_=owned_group.finish(child,(getattr(child,'stdin',None),getattr(child,'stdout',None)),prelude,subprocess.TimeoutExpired,cancel)
        if failure is not None and problems:failure.cleanup_errors=problems
        owned_group.conclude(cancel,failure,**({'cleanup_errors':problems} if problems else {}))
    if problems:return {'exit':getattr(child,'returncode',None),'error':'owned_invocation_closure_unknown','body':None}
    return result


UNIT_SUFFIXES=('service','scope','socket','timer','target','slice','mount','automount','path','swap','device')
def unit_name(unit):
    """The full unit name systemctl resolves, with .service added when no unit suffix is given."""
    return unit if unit.rsplit('.',1)[-1] in UNIT_SUFFIXES and '.' in unit else unit+'.service'

def observer_sources():
    """The maintained sources that every remote observer program is composed from."""
    common=(ROOT/'config_schema.py').read_text()+'\n'+(ROOT/'common.py').read_text()
    definitions=(ROOT/'metadata-probe.py').read_text().split("\nif __name__=='__main__':",1)[0]
    return common,definitions,(ROOT/'startup-observer.py').read_text()

def props(text):
    return dict(line.split('=',1) for line in text.splitlines() if '=' in line)

class RealIO:
    def __init__(self,r,op):self.r=r;self.op=Path(op);self.boat_path=r['boat']['path']
    def wall(self):return time.time()
    def mono(self):return time.clock_gettime(time.CLOCK_BOOTTIME)
    def wait(self,seconds):time.sleep(min(seconds,1))
    def record(self,name,value):write_new(self.op,name,value)
    def exists(self,name):return (self.op/name).exists()
    def read(self,name):return load(self.op/name)
    def names(self):return sorted(os.listdir(self.op))
    def local_boot_id(self):return Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    def freeze_extensions(self):
        """Hold the operation's extension lock, waiting out a running act within FREEZE_WAIT."""
        if getattr(self,'_frozen',None) is not None:return True
        fd=open_lock(self.op/EXTENSION_LOCK);end=self.mono()+FREEZE_WAIT
        while True:
            try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);self._frozen=fd;return True
            except BlockingIOError:
                if self.mono()>=end:os.close(fd);return False
                time.sleep(0.2)
    def thaw_extensions(self):
        fd=getattr(self,'_frozen',None);self._frozen=None
        if fd is not None:os.close(fd)
    def remote_python(self,program,args,seconds):
        """Run a maintained program on the worker through stdin.

        The provider joins the words after ``--`` into one remote shell command,
        so a program or argument given as argv would be split by that shell. The
        program and its arguments travel as stdin bytes, as the phase launcher's do.
        """
        source=('import sys\nsys.argv=[\'remote\']+'+repr([str(a) for a in args])+'\n'+program).encode()
        return self.boat(['ssh',worker_id(),'--','/usr/bin/python3','-I','-B','-'],seconds,source,json_mode=False)
    def diagnose(self,seconds):
        """Run the maintained read-only diagnostic program on the worker."""
        r=self.remote_python((ROOT/'diagnostic-probe.py').read_text(),[],seconds)
        need(r['exit']==0 and isinstance(r['body'],dict),'diagnostics unavailable')
        return r['body']
    def worker_boot_id(self):
        code="import json;print(json.dumps({'boot_id':open('/proc/sys/kernel/random/boot_id').read().strip()}))"
        r=self.remote_python(code,[],20)
        need(r['exit']==0 and isinstance(r['body'],dict) and isinstance(r['body'].get('boot_id'),str),'worker boot identity unavailable')
        return r['body']['boot_id']
    def boat(self,args,seconds=20,input_bytes=None,json_mode=True):
        prefix=[self.boat_path,'--no-update']+(['--json'] if json_mode else [])
        return bounded(prefix+args,seconds,input_bytes)
    def info(self):
        r=self.boat(['info',worker_id()]);need(r['exit']==0 and r['body'] is not None,'info unavailable')
        return safe_info(r['body'])
    def system(self,args,seconds=15):
        # No shell interpolation, no raw systemctl output is persisted.
        helper="import subprocess,json,sys;r=subprocess.run(sys.argv[1:],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10);assert len(r.stdout)<=65536;print(json.dumps({'exit':r.returncode,'stdout':r.stdout.decode()}))"
        r=bounded([sys.executable,'-I','-B','-c',helper]+args,seconds)
        need(r['exit']==0 and isinstance(r['body'],dict) and r['body']['exit']==0,'systemd command failed')
        return r['body']['stdout']
    def show(self,unit):
        names='Id,LoadState,ActiveState,SubState,MainPID,ExecMainStatus,InvocationID,ExecStart,MemoryMax,MemorySwapMax,RuntimeMaxUSec,KillMode,OOMPolicy,ControlGroup'
        return props(self.system(['systemctl','--user','show',unit,'--property='+names]))
    def guard_for_admission(self):
        ready=self.read('guard-loaded.json');live=self.show(self.r['guard_unit']+'.service')
        need(live.get('Id')==self.r['guard_unit']+'.service' and live.get('ActiveState')=='active' and live.get('InvocationID')==ready['loaded']['InvocationID'] and int(live.get('MainPID','0'))==ready['pid'],'guard not live at handoff')
        need(Path('/proc/sys/kernel/random/boot_id').read_text().strip()==ready['boot_id'],'local guard boot changed')
        result=dict(ready);result['loaded']={k:live[k]for k in live if k!='ExecStart'};result['observed_wall']=self.wall();result['observed_boottime']=self.mono()
        self.record('guard-admission.json',result);return result
    def limits(self):
        result=self.boat(['limits'])
        need(result['exit']==0 and isinstance(result['body'],dict),'live account limits unavailable')
        body=result['body'];starts=body.get('starts',{});need(starts.get('unlimited')is False,'unbounded start policy')
        safe={}
        for window,limit in [('day',200),('hour',60),('minute',12)]:
            x=starts.get(window,{})
            need(x.get('limit')==limit and type(x.get('used'))is int and type(x.get('remaining'))is int and 0<=x['used']<limit and x['remaining']>=1,'shared account quota exhausted/unknown')
            safe[window]={k:x[k]for k in ('limit','used','remaining')}
        need(any(x.get('type')=='large' and x.get('available')is True for x in body.get('machineTypes',[])),'large unavailable')
        need(safe['day']['used']<self.r['start_ledger']['starts_and_reservations'],'start ledger stale or missing this reservation')
        self.record('account-starts-before-resume.json',{'observed_wall':self.wall(),'account_wide':True,'starts':safe,'large_available':True,'no_wallet_or_plan_changes':True})
        return safe
    def unpublish(self,name):
        """Rename a published receipt away, keeping its bytes as evidence."""
        os.rename(self.op/name,self.op/('unpublished-'+name))
    def lock_jobs(self):
        """Take the operation's job lock exclusively without waiting, or report that a command holds it."""
        fd=open_lock(self.op/JOB_LOCK)
        try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError:
            os.close(fd);return False
        self._jobs=fd;return True
    def unlock_jobs(self):
        fd=getattr(self,'_jobs',None);self._jobs=None
        if fd is not None:os.close(fd)
    def job_closure(self,local_units,seconds=60):
        """One check of the worker's job units and processes and the local job units.

        Each bounded call may spend owned_group.WINDOW seconds in teardown after its
        own timeout, so the timeouts leave that much, and the whole check with its
        teardown ends within ``seconds``.
        """
        end=self.mono()+seconds;teardown=owned_group.WINDOW+1
        common,definitions,observer=observer_sources()
        program=common+'\n'+definitions+'\n'+observer+'\n'+JOB_CLOSURE_ENTRY
        remote=end-self.mono()-teardown-((10+teardown) if local_units else 0);need(remote>=5,'job closure bound exhausted')
        r=self.remote_python(program,[json.dumps(profile()),json.dumps(list(JOB_UNIT_PREFIXES))],remote)
        need(r['exit']==0 and isinstance(r['body'],dict),'worker job closure check failed')
        b=r['body'];local={}
        if local_units:
            names={unit:unit_name(unit) for unit in local_units}
            left=end-self.mono()-teardown;need(left>=1,'job closure bound exhausted')
            # One systemctl call for every named unit, bounded by what is left.
            text=self.system(['systemctl','--user','show',*names.values(),'--property=Id,ActiveState,MainPID'],int(left))
            blocks=[props(x) for x in text.strip().split('\n\n')] if text.strip() else []
            seen={x.get('Id'):x for x in blocks}
            for unit,name in names.items():
                x=seen.get(name,{})
                local[unit]=x.get('ActiveState') in ('inactive','failed') and x.get('MainPID')=='0'
        need(self.mono()<=end,'job closure bound exhausted')
        return {'list_exit':b.get('list_exit'),'units':b.get('units'),'unit_count':b.get('unit_count'),'processes':b.get('processes'),'process_count':b.get('process_count'),'boot_id':b.get('boot_id'),'local_units_closed':local}
    def stop_status(self,operation):
        r=self.boat(['stop',worker_id(),'--status',operation],15)
        need(r['exit']==0 and isinstance(r['body'],dict) and r['body'].get('id')==worker_id(),'stop status unavailable')
        x=r['body'].get('stop');need(isinstance(x,dict) and x.get('id')==operation,'stop status operation mismatch')
        return {k:x.get(k) for k in ['id','status','requestedAt','lastAttemptAt','endedAt','forced']} | {'error_present':x.get('error') is not None}
    def environment_after(self,intent):
        path=self.r['environment']['post_resume_fact_path'];need(isinstance(path,str),'post-resume ENV fact path required')
        end=self.mono()+60
        while self.mono()<end:
            try:
                e=load(path);freshness(e,self.wall(),60)
                need(e['observed_unix']>=self.read('resume-intent.json')['wall'],'ENV observation precedes resume')
                need(e.get('id')==profile()['future_environment']['id'] and e.get('latestVersionNumber')==profile()['future_environment']['version'] and e.get('latestVersionId')==self.r['environment']['latest_version_id'] and e.get('hook_sha256')==hook_sha() and e.get('hook_bytes')==profile()['future_environment']['hook_bytes'] and e.get('setupBlocking')is True,'post-resume latest ENV/hook drift')
                self.record('environment-after.json',e);return e
            except FileNotFoundError:self.wait(1)
        raise Refusal('fresh post-resume ENV proof absent')
    def arm(self,intent):
        unit=self.r['guard_unit']+'.service'
        absent=self.show(unit);need(absent.get('ActiveState')=='inactive' and absent.get('MainPID')=='0' and absent.get('LoadState')=='not-found' and not absent.get('InvocationID'),'guard unit not unused')
        argv=[str(Path(sys.executable).resolve()),'-I','-B',str(ROOT/'controller.py'),'guard','--operation',str(self.op)]
        self.system(['systemd-run','--user','--unit='+self.r['guard_unit'],'--property=Type=exec','--property=MemoryMax=512M','--property=MemorySwapMax=0','--property=RuntimeMaxSec='+str(GUARD_RUNTIME),'--property=TimeoutStopSec=10','--property=KillMode=control-group','--property=OOMPolicy=continue']+argv)
        end=self.mono()+15
        while self.mono()<end:
            if self.exists('guard-ready.json'):
                ready=self.read('guard-ready.json');p=self.show(unit)
                actual=re.search(r'argv\[\]=(.*?) ;',p.get('ExecStart',''))
                need(actual is not None and shlex.split(actual[1])==argv,'guard loaded ExecStart mismatch')
                need(p.get('ActiveState')=='active' and int(p.get('MainPID','0'))==ready['pid'] and p.get('MemoryMax')==str(MEMORY) and p.get('MemorySwapMax')=='0' and p.get('KillMode')=='control-group' and p.get('OOMPolicy')=='continue' and systemd_seconds(p.get('RuntimeMaxUSec',''))==GUARD_RUNTIME,'guard loaded limits/PID/clock mismatch')
                need(p.get('ControlGroup')==ready['cgroup'].removeprefix('/sys/fs/cgroup') and re.fullmatch('[0-9a-f]{32}',p.get('InvocationID','')),'guard loaded cgroup/invocation mismatch')
                need(ready['deadline_mono']==intent['deadline_mono'] and ready['memory_max']==str(MEMORY) and ready['swap_max']=='0','guard runtime mismatch')
                ready['loaded']={k:p[k] for k in p if k!='ExecStart'}
                self.record('guard-loaded.json',ready);return ready
            self.wait(0.2)
        raise Refusal('guard readiness timeout')
    def trigger(self):
        # Finish, request-stop and the guard may race to publish the same trigger.
        if not self.exists('stop-trigger.json'):
            try:self.record('stop-trigger.json',{'requested_wall':self.wall()})
            except FileExistsError:pass
    def metadata_closed(self):
        unit=self.r['guard_unit']+'-metadata.service'
        code="""from pathlib import Path
import subprocess,json,sys
u=sys.argv[1];r=subprocess.run(['systemctl','--user','show',u,'--property=LoadState,MainPID,ControlGroup,InvocationID,ActiveState'],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=10)
p=dict(x.split('=',1)for x in r.stdout.decode().splitlines()if '='in x)
c=p.get('ControlGroup','');closed=r.returncode==0 and p.get('LoadState')=='loaded' and p.get('MainPID')=='0' and p.get('ActiveState')in['inactive','failed'] and bool(p.get('InvocationID'))
if c:
 root=Path('/sys/fs/cgroup')/c.lstrip('/')
 if root.exists():closed=closed and all(not f.read_text().strip()for f in root.rglob('cgroup.procs'))
print(json.dumps({'closed':closed,'unit':u,'properties':p}))
"""
        r=self.remote_python(code,[unit],20)
        need(r['exit']==0 and isinstance(r['body'],dict) and r['body'].get('closed')is True and r['body'].get('unit')==unit,'metadata closure query failed')
        self.record('qualification-closed.json',r['body']);return True
    def phase(self,phase,observation_seconds=0):
        payload={'pins':profile(),'release':{'snapshot_continuity':self.r['snapshot_continuity']},'operation_tag':self.r['guard_unit'],'phase':phase,'observation_seconds':observation_seconds}
        common,definitions,observer=observer_sources()
        entry="""
if __name__=='__main__':
    payload=json.loads(sys.argv[1])
    def bound(x):
        bind_profile(x['pins']);return observe_startup(x) if x['phase']!='metadata' else qualify_with_startup_lock(x)
    result,closed=run_phase(payload,bound,bound)
    result.update({'program_sha256':payload['program_sha256'],'phase':payload['phase'],'unit':payload['operation_tag']+'-'+payload['phase']+'.service'})
    result['phase_memory']=own_cgroup_memory(result['unit'])
    write_new(Path(payload['result_directory']),'probe-result.json',result)
    print(json.dumps(result))
    raise SystemExit(0 if closed else 1)
"""
        program=(common+'\n'+definitions+'\n'+observer+'\n'+entry).encode()
        source=(ROOT/'remote-launcher.py').read_text().replace('COMMON_SOURCE_LITERAL',repr(common)).replace('PAYLOAD_JSON_LITERAL',repr(json.dumps(payload))).replace('PROBE_BASE64_LITERAL',repr(base64.b64encode(program).decode())).encode()
        need(len(source)<262144,'remote phase stdin bound')
        seconds=330 if phase=='metadata' else (45 if phase=='stopcheck' else observation_seconds+85)
        r=self.boat(['ssh',worker_id(),'--','/usr/bin/python3','-I','-B','-'],seconds,source,False)
        need(r['exit']==0 and isinstance(r['body'],dict),'remote phase invocation failed')
        return r['body']
    def startup(self,intent,allow_qualification):
        state={}
        try:return self._startup(intent,allow_qualification,state)
        except Exception as error:
            record_refusal(self,'startup',error,state.get('result'));raise
    def _startup(self,intent,allow_qualification,state):
        if self.exists('startup-closed.json'):
            result=self.read('startup-closed.json')
            if allow_qualification:need(result.get('qualification_eligible')is True,'allowed startup produced unadopted manifest')
            return True
        remaining=int(min(intent['mono']+1950-self.mono(),intent['wall']+1950-self.wall()))
        seconds=max(1,min(1950,remaining)) if allow_qualification else max(1,min(60,remaining))
        if allow_qualification:need(remaining>0,'startup observation ceiling exceeded')
        if not self.exists('startup-started.json'):self.record('startup-started.json',{'unit':self.r['guard_unit']+'-startup.service','seconds':seconds,'bootstrap_occurrence':'unknown'})
        # Existing remote operation only reads its atomic receipt/unit; no second
        # observer/installer invocation, even after foreground disconnect.
        result=state['result']=self.phase('startup',seconds)
        if result.get('startup_closed')is not True or result.get('phase_closed')is not True:
            self.startup_refused(result);return False
        self.record('startup-closed.json',result)
        if allow_qualification:need(result.get('qualification_eligible')is True,'allowed startup produced unadopted manifest')
        return True
    def startup_refused(self,result):
        """Keep the first refused startup's bounded reason, never its free text."""
        record_refusal(self,'startup',result=result)
    def stop_quiescent(self):
        if self.exists('stop-startup-closed.json'):return True
        result=None
        try:
            result=self.phase('stopcheck',5)
            need(result.get('startup_closed')is True and result.get('phase_closed')is True,'current startup/installer closure unavailable before stop')
        except Exception as error:
            record_refusal(self,'stopcheck',error,result);raise
        self.record('stop-startup-closed.json',result)
        if not self.exists('startup-closed.json'):self.record('startup-closed.json',result)
        return True
    def qualification(self,intent):
        budget(self.mono(),min(intent['deadline_mono'],self.mono()+intent['deadline_wall']-self.wall()),330)
        need(self.exists('startup-closed.json'),'startup closure absent')
        self.record('metadata-started.json',{'unit':self.r['guard_unit']+'-metadata.service','wall':self.wall()})
        q=None;failed=[]
        try:
            q=self.phase('metadata')
            if q.get('metadata_closed')is True:self.record('qualification-closed.json',{'metadata_closed':True,'unit':self.r['guard_unit']+'-metadata.service'})
            failed=metadata_failures(q)
            if failed:raise Refusal('metadata refused: '+failed[0])
        except Exception as error:
            record_refusal(self,'metadata',error,q,failed=failed);raise
        self.record('qualification.json',q);return q


def no_ready(result):
    b=result.get('body')
    return result.get('exit') not in (0,None) and isinstance(b,dict) and b.get('ok') is False and b.get('code')=='no_ready_machine' and isinstance(b.get('error'),dict) and b['error'].get('code')=='no_ready_machine'

def drive(r,io):
    validate_release(r,io.wall())
    before=io.info();stopped(before);need(before['environmentVersion']==r['snapshot_continuity']['pre_resume_environment_version'],'unexpected pre-resume environment')
    need(before['snapshotCompletedAt']==r['snapshot_continuity']['snapshot_completed_at'],'snapshot continuity drift')
    # Each clock is read once, so both deadlines are exactly 10,500 seconds after their readings.
    wall,mono=io.wall(),io.mono()
    intent={'worker':worker_id(),'wall':wall,'mono':mono,'deadline_mono':mono+10500,'deadline_wall':wall+10500,'ttl':10800,'resume_count':1}
    io.record('controller-intent.json',intent)
    try:io.arm(intent)
    except Exception:
        io.record('cancel-before-resume.json',{'no_provider_resume_intent':True});raise Refusal('guard arm failed; no resume') from None
    # Recheck time-bound facts while the Root environment-exclusion lock is held.
    validate_release(r,io.wall());budget(io.mono(),intent['deadline_mono'],2400);io.limits()
    io.record('resume-intent.json',{'worker':worker_id(),'wall':io.wall(),'mono':io.mono(),'argv':['resume',worker_id(),'--environment',profile()['future_environment']['name'],'--type','large','--ttl','10800','--fail-fast','--no-wait']})
    try:
        result=io.boat(['resume',worker_id(),'--environment',profile()['future_environment']['name'],'--type','large','--ttl','10800','--fail-fast','--no-wait'])
        io.record('resume-outcome.json',{'exit':result['exit'],'transport_error':result['error'],'no_ready_machine':no_ready(result),'acceptance':'positive' if positive_resume_ack(result,r['provider_contract']['resume_ack']) else ('no_ready_candidate' if no_ready(result) else 'uncertain'),'uncertain_acceptance':not positive_resume_ack(result,r['provider_contract']['resume_ack']) and not no_ready(result)})
        if no_ready(result):
            after=io.info();stopped(after)
            need(after['environmentVersion'] in (r['snapshot_continuity']['pre_resume_environment_version'],profile()['future_environment']['version']) and after['snapshotCompletedAt']==before['snapshotCompletedAt'] and after['stoppedAt']==before['stoppedAt'],'no-ready state drift')
            io.record('no-ready-confirmed.json',{'safe_info':after,'no_start_count_claim':'documented fail-fast no_ready_machine only'})
            return 'NO_READY_CONFIRMED'
        # Never send a second resume, including timeout/malformed/unknown responses.
        need(positive_resume_ack(result,r['provider_contract']['resume_ack']),'uncertain resume; conserve reservation, no qualification or retry')
        end=io.mono()+1950;count=0;expiry=None
        while io.mono()<end:
            budget(io.mono(),intent['deadline_mono'],30)
            try:info=io.info()
            except Refusal:io.wait(1);continue
            count+=1;io.record(f'info-{count:04d}.json',info)
            if info['state']=='stopped':raise Refusal('resume not established; retained stopped state')
            if info['state'] not in ('ready','idle','running'):
                io.wait(1);continue
            need(positive_resume_ack(result,r['provider_contract']['resume_ack']),'uncertain resume')
            expiry=resumed(info,r,intent['wall'])
            budget(io.mono(),min(intent['deadline_mono'],io.mono()+expiry-io.wall()),330)
            need(info['setupStatus'] not in ('failed','error'),'startup failed')
            break
            io.wait(1)
        else:raise Refusal('startup observation deadline')
        io.environment_after(intent)
        need(io.startup(intent,allow_qualification=True),'startup still busy or unknown')
        io.record('startup-qualified.json',{'environmentVersion':profile()['future_environment']['version'],'hook_sha256':hook_sha(),'hook_binding':'Root latest-version exclusion receipt plus returned pinned environment, not a fabricated provider hook field','earlier_local_deadline_wall':expiry,'provider_expiry_observed':False,'ttl_basis':'successful exact CLI resume --ttl10800 plus observed ttlSeconds; no archiveAfter inference','setup_status_null_is_not_success_proof':True,'safe_info':info})
        io.qualification(intent)
        # No source job or transport implementation in this packet. Default is stop.
        if r.get('useful_job_ready') is True:
            try:
                h=r.get('separate_Root_handoff',{})
                need(sha(h.get('job_release_sha256')) and sha(h.get('different_job_review_sha256')) and h.get('Root_owns_job_and_deadline_closure') is True,'unbound useful job handoff')
                budget(io.mono(),min(intent['deadline_mono'],io.mono()+expiry-io.wall()),h.get('finite_job_and_closure_seconds',10800))
                # The handoff is staged. Its admission is computed from the same
                # values and published first, and the handoff is published last, so
                # a failure at any step leaves no standing handoff and the worker
                # stops normally without a job closure.
                handoff=json.loads(json.dumps({'worker':worker_id(),'separate_job_release':h,'deadline_wall':intent['deadline_wall'],'qualification_only_controller':True,'job_started_by_controller':False}))
                admission=make_admission(r,intent,io.read('qualification.json'),io.read('startup-closed.json'),io.guard_for_admission(),handoff,io.wall())
                # Every job command holds the job lock shared from before its stopping
                # checks, so under the exclusive lock no job is admitted between a
                # publication and its withdrawal.
                need(io.lock_jobs(),'job lock held before the Root handoff')
                try:
                    io.record('qualified-worker-admission.json',admission)
                    io.record('root-handoff.json',handoff)
                except Exception as error:
                    withdraw_handoff(io,error);raise
                finally:io.unlock_jobs()
            except Exception as error:
                record_refusal(io,'admission',error)
                if not io.exists(lifetime.HANDOFF_WITHDRAWN):withdraw_handoff(io,error)
                raise
            return 'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE'
        return 'QUALIFIED_STOP_REQUESTED'
    except Exception as error:
        io.record('qualification-failed.json',{'qualification':False,'resume_not_retried':True,'Root_must_reconcile_if_guard_unresolved':True,'reason_code':reason_code(error)})
        diagnostic_hold(io,intent)
        raise
    finally:
        if not io.exists('no-ready-confirmed.json') and not handoff_active(io):io.trigger()


# After a refused qualification, the controller holds its stop trigger for at most
# HOLD_MAX seconds to read the worker's state. The hold ends HOLD_RESERVE seconds
# before the controller unit's CONTROLLER_RUNTIME limit, which boat:qualify sets, so
# the trigger is always written, and HOLD_DEADLINE_RESERVE seconds before the
# guard's own deadline. Each read waits DIAGNOSE_RETRY seconds after a failure.
HOLD_MAX=600
CONTROLLER_RUNTIME=2700
HOLD_RESERVE=300
HOLD_DEADLINE_RESERVE=600
DIAGNOSE_SECONDS=120
DIAGNOSE_RETRY=15
# The transport's own teardown may take a few seconds after its window, so each
# read ends DIAGNOSE_CLEANUP seconds before the hold does.
DIAGNOSE_CLEANUP=15
RECEIPT_MAX=1024*1024-4096
def fit_diagnostics(body):
    """The diagnostics trimmed until their receipt, as write_new serializes it, fits its bound."""
    spec=importlib.util.spec_from_file_location('diagnostic_probe',ROOT/'diagnostic-probe.py');module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    need(isinstance(body.get('state'),dict) and isinstance(body['state'].get('phases'),dict) and isinstance(body['state'].get('entries'),list)
         and isinstance(body.get('processes'),dict) and isinstance(body['processes'].get('rows'),list),'diagnostics malformed')
    body=module.fit(body,RECEIPT_MAX,lambda x:len(json.dumps(x,sort_keys=True,indent=2)))
    need(len(json.dumps(body,sort_keys=True,indent=2))<=RECEIPT_MAX,'diagnostics over the receipt bound')
    return body
def diagnostic_hold(io,intent):
    """Read the worker's processes, state and phase results once after a refused qualification.

    It runs only in qualification, before any Root handoff, after a positive resume
    acknowledgement and a running worker. It never runs for a job, an extension or a
    closure. It returns before the controller writes its stop trigger, and every
    error ends the hold, so the trigger is never lost."""
    try:
        # A published handoff never holds, even withdrawn, since a job could be bound to it.
        if not isinstance(intent,dict) or handoff_active(io) or io.exists('no-ready-confirmed.json'):return
        if io.exists('root-handoff.json') and io.exists('qualified-worker-admission.json'):return
        if not io.exists('resume-outcome.json') or io.read('resume-outcome.json').get('acceptance')!='positive':return
        if not any(n.startswith('info-') and io.read(n).get('state') in ('ready','idle','running') for n in io.names()):return
        start=io.mono();end=min(start+HOLD_MAX,intent['mono']+CONTROLLER_RUNTIME-HOLD_RESERVE,intent['deadline_mono']-HOLD_DEADLINE_RESERVE)
        io.record('diagnostic-hold.json',{'started_boottime':start,'end_boottime':end,'seconds':max(0,end-start),'started_wall':io.wall(),'qualification_only':True})
        attempt=0
        while end-io.mono()>=DIAGNOSE_CLEANUP+10:
            attempt+=1
            try:
                body=io.diagnose(min(DIAGNOSE_SECONDS,end-io.mono()-DIAGNOSE_CLEANUP))
                need(isinstance(body,dict) and body.get('schema')==1,'diagnostics malformed')
                io.record('diagnostics.json',dict(fit_diagnostics(body),observed_wall=io.wall(),attempt=attempt));break
            except Exception as error:
                io.record('diagnostics-failed-%04d.json'%attempt,{'reason_code':reason_code(error),'observed_wall':io.wall()})
            pause(io,min(DIAGNOSE_RETRY,max(0,end-io.mono())))
        io.record('diagnostic-hold-ended.json',{'ended_boottime':io.mono(),'attempts':attempt,'captured':io.exists('diagnostics.json')})
    except Exception:return

def withdraw_handoff(io,error):
    """Withdraw a handoff the controller is not returning, without depending on any one write.

    The withdrawal receipt marks the operation for every consumer. When it cannot be
    written, a published handoff receipt is renamed away instead. Neither failure stops
    the controller from triggering the normal stop."""
    try:
        io.record(lifetime.HANDOFF_WITHDRAWN,{'worker':worker_id(),'reason_code':reason_code(error),'qualified_worker_admission_present':io.exists('qualified-worker-admission.json'),'root_handoff_present':io.exists('root-handoff.json'),'observed_wall':io.wall()})
        return
    except Exception:pass
    try:
        if io.exists('root-handoff.json'):io.unpublish('root-handoff.json')
    except Exception:pass

def handoff_active(io):
    """Whether a Root handoff stands, so that a normal stop needs the job closure.

    The controller publishes the handoff after its admission receipt, and withdraws
    it whenever it does not return a standing handoff. A handoff without its
    admission, or a withdrawn one, admitted no job."""
    return io.exists('root-handoff.json') and io.exists('qualified-worker-admission.json') and not io.exists(lifetime.HANDOFF_WITHDRAWN)

def matching_root_job_closed(r,io,closed):
    freshness(closed,io.wall())
    handoff=io.read('root-handoff.json');admission=io.read('qualified-worker-admission.json')
    need(closed.get('worker')==worker_id() and closed.get('operation_id')==r['guard_unit'],'Root job closure owner/operation')
    need(closed.get('local_guard')==admission['local_guard'],'Root job closure guard identity')
    need(closed.get('job_release_sha256')==handoff['separate_job_release']['job_release_sha256'] and closed.get('all_native_and_leases_closed')is True,'Root job/lease closure missing')
    return True

def reconcile_stop(r,io):
    """The sole stop owner. Submitted/pending/unknown is never completed."""
    try:
        if io.exists('resume-intent.json') and not io.exists('startup-closed.json'):
            need(io.stop_quiescent(),'startup closure unknown')
        if io.exists('metadata-started.json') and not io.exists('qualification-closed.json'):
            need(io.metadata_closed(), 'owned metadata closure unavailable')
        if handoff_active(io):
            closed=io.read('root-job-closed.json');matching_root_job_closed(r,io,closed)
    except Exception as error:
        io.record('unresolved.json',{'worker':worker_id(),'stop_submitted':False,'ownership_retained_by_Root':True,'reason_code':reason_code(error),'action':'Root must establish owned metadata/job/lease closure before normal stop; providerTTL remains finite; retain guard evidence.'})
        return False
    try:need(io.stop_quiescent(),'startup not currently quiescent')
    except Exception as error:
        io.record('unresolved.json',{'worker':worker_id(),'stop_submitted':False,'ownership_retained_by_Root':True,'reason_code':reason_code(error),'action':'current bootstrap-lock/installer closure unavailable; no clean stop claim'})
        return False
    request=io.wall();stop_end=io.mono()+120;io.record('stop-intent.json',{'worker':worker_id(),'requested_wall':request,'force':False,'single_request':True})
    result=io.boat(['stop',worker_id()],20);operation=None
    b=result.get('body')
    if isinstance(b,dict) and b.get('id')==worker_id():
        s=b.get('operation') or b.get('stop')
        if isinstance(s,dict) and isinstance(s.get('id'),str) and s['id'].startswith('stop_'):operation=s['id']
    io.record('stop-submission.json',{'exit':result['exit'],'transport_error':result['error'],'operation_id':operation,'completed':False})
    end=stop_end;count=0;last='stop_not_observed'
    while io.mono()+50<end:
        try:
            info=io.info();count+=1;io.record(f'stop-info-{count:04d}.json',info)
            # Unknown stop response may be reconciled through the fresh exact-ID info.stop.
            op=operation or info.get('stop',{}).get('id')
            status=io.stop_status(op);io.record(f'stop-status-{count:04d}.json',status)
            need(status['id']==op and status['status']=='completed' and status['forced']is False and status['error_present']is False,'stop status not completed')
            need(timestamp(status.get('requestedAt'))>=request-2 and timestamp(status.get('endedAt'))>=request-2,'stop status is not this fresh request')
            stop_complete(info,op,request,r['snapshot_continuity']['snapshot_completed_at'],r['provider_contract']['snapshot_policy'])
            need(io.exists('qualification-closed.json') or not io.exists('metadata-started.json'),'owned metadata closure absent')
            if handoff_active(io):
                closed=io.read('root-job-closed.json')
                matching_root_job_closed(r,io,closed)
            io.record('closed.json',{'worker':worker_id(),'stop_operation':op,'safe_info':info,'stopped_confirmed':True,'snapshot_available':True,'owned_metadata_closed':True,'private_credential_snapshot':True,'guard_may_exit':True})
            return True
        except (Refusal,FileNotFoundError) as error:last=reason_code(error);io.wait(1)
    io.record('unresolved.json',{'worker':worker_id(),'stop_operation':operation,'reason_code':last,'resume_retried':False,'stop_retried_or_forced':False,'ownership_retained_by_Root':True,'action':'Root must reconcile exact worker/operation; retain failed guard unit, do not declare shutdown/disarm/clean.'})
    return False

def guard_deadlines(io,intent):
    """The guard's (BOOTTIME, wall) deadline, moved only by valid extension receipts.

    Before a Root handoff this is the intent's own pair. After it, the base is the
    admission's conservative original pair, never later than the intent's. Any
    missing, unreadable or malformed receipt chain falls back to that base, so
    lost or damaged evidence can only end the lifetime sooner.
    """
    original=(intent['deadline_mono'],intent['deadline_wall'])
    try:return _guard_deadlines(io,intent,original)
    except Exception:
        base=getattr(io,'guard_base',None)
        return original if base is None else base

def _guard_deadlines(io,intent,original):
    # Once established, the conservative base is kept for the guard's life, so
    # later loss of the handoff receipts cannot restore the intent's later end.
    base=getattr(io,'guard_base',None)
    if base is None:
        if not io.exists('root-handoff.json'):return original
        try:
            guard=io.read('guard-admission.json');handoff=io.read('root-handoff.json')
            wall,boot=lifetime.original_ends(intent,guard,handoff)
            base=(min(boot,original[0]),min(wall,original[1]))
        except Exception:return original
        io.guard_base=base
    try:
        names=io.names()
        if not lifetime.extension_names(names):return base
        guard=io.read('guard-admission.json')
        identity=lifetime.operation_identity(intent,guard,io.read('qualification.json'),io.r['guard_unit'],profile())
        vetoed=io.__dict__.setdefault('vetoed_acceptances',set())
        def accept(number,e,wall,boot):
            # Accept only with the closure reserve left, and confirm after the write
            # that it finished before the previous end. A late write is vetoed for the
            # guard's life, so the guard stops, and its stop trigger refuses admission.
            now_wall,now_boot=io.wall(),io.mono()
            if not(now_wall+lifetime.CLOSURE_RESERVE<wall and now_boot+lifetime.CLOSURE_RESERVE<boot):return None
            row=lifetime.acceptance_record(number,e,identity,now_wall,now_boot)
            # Any error in the write or in the check after it, such as a failed
            # directory fsync, leaves timely completion unknown, so it is vetoed
            # like a late write.
            try:
                io.record('lifetime-extension-%04d-accepted.json'%number,row)
                timely=io.wall()<wall and io.mono()<boot
            except Exception:timely=False
            if not timely:
                vetoed.add(number)
                try:io.record('lifetime-extension-%04d-vetoed.json'%number,{'sequence':number,'receipt_sha256':row['receipt_sha256'],'observed_wall':io.wall(),'observed_boottime':io.mono()})
                except Exception:pass
                return None
            return row
        wall,boot,_=lifetime.apply_extensions(names,io.read,intent,base[1],base[0],identity,accept,vetoed)
        return boot,wall
    except Exception:return base

def guard_loop(r,io,intent):
    while True:
        deadline_mono,deadline_wall=guard_deadlines(io,intent)
        if not(io.mono()<deadline_mono and io.wall()<deadline_wall):
            # Stop only while holding the extension lock, after reading the receipts
            # once more, so that no act publishes an extension after this decision.
            # A failure in any of these steps still leads to the stop below.
            try:frozen=io.freeze_extensions()
            except Exception:frozen=False
            if frozen:
                deadline_mono,deadline_wall=guard_deadlines(io,intent)
                try:extended=io.mono()<deadline_mono and io.wall()<deadline_wall
                except Exception:extended=False
                if extended:
                    try:io.thaw_extensions()
                    except Exception:pass
                    continue
            # The stopping state is published before the stop, so admission refuses it.
            try:io.trigger()
            except Exception:pass
            break
        if io.exists('no-ready-confirmed.json'):
            check=io.read('no-ready-confirmed.json');stopped(check['safe_info'])
            io.record('closed-no-start.json',{'worker':worker_id(),'no_ready_confirmed':True,'guard_may_exit':True});return True
        if io.exists('cancel-before-resume.json') and not io.exists('resume-intent.json'):
            io.record('closed-no-resume.json',{'no_resume_intent':True,'guard_may_exit':True});return True
        if io.exists('stop-trigger.json'):
            # An explicit stop also waits out and then excludes any extension act.
            try:io.freeze_extensions()
            except Exception:pass
            if not io.exists('startup-closed.json'):
                if min(deadline_mono-io.mono(),deadline_wall-io.wall())<=200:break
                try:
                    if not io.startup(intent,allow_qualification=False):io.wait(5);continue
                except Exception:io.wait(1);continue
            # A disconnected SSH may leave our finite metadata service finishing.
            # Wait read-only for its closure; do not abandon the independent deadline.
            if io.exists('metadata-started.json') and not io.exists('qualification-closed.json'):
                try:
                    if not io.metadata_closed():io.wait(1);continue
                except Exception:io.wait(1);continue
            break
        io.wait(1)
    if not io.exists('resume-intent.json'):
        io.record('closed-no-resume.json',{'no_resume_intent':True,'guard_may_exit':True});return True
    return reconcile_stop(r,io)


def extend_operation(r,io,seconds,fact_path,reason):
    """Root's explicit extension of the running qualified worker, recorded as a receipt.

    It adopts the worker only when the local boot, the live guard invocation, the
    worker's own boot, the provider environment and a fresh Root environment fact
    all match the qualification. The new end is this observation plus ``seconds``
    on both clocks, within the total cap. The provider lifetime is set first and
    confirmed through info before the receipt is published. Any uncertainty
    leaves only a failed attempt record, which grants nothing.
    """
    need(type(seconds) is int and 0<seconds<=lifetime.EXTENSION_MAX,'extension seconds out of range')
    need(isinstance(reason,str) and 0<len(reason)<=200 and all(' '<=c<='~' for c in reason),'extension reason must be short printable text')
    need(handoff_active(io) and io.exists('guard-admission.json'),'no qualified Root handoff to extend')
    for name in ('stop-trigger.json','stop-intent.json','closed.json','unresolved.json'):need(not io.exists(name),'operation is stopping or closed')
    intent=io.read('controller-intent.json');guard=io.read('guard-admission.json');handoff=io.read('root-handoff.json');q=io.read('qualification.json')
    identity=lifetime.operation_identity(intent,guard,q,r['guard_unit'],profile())
    need(identity['worker_boot_id'] is not None,'qualification lacks the worker boot identity')
    names=io.names();wall,boot=lifetime.original_ends(intent,guard,handoff)
    try:wall,boot,count=lifetime.apply_extensions(names,io.read,intent,wall,boot,identity)
    except ValueError as error:raise Refusal('existing extension receipts refused: '+str(error)) from None
    need(count==len(lifetime.extension_names(names)),'a previous extension is not accepted by the guard')
    need(min(wall-io.wall(),boot-io.mono())>lifetime.CLOSURE_RESERVE,'lifetime already inside the closure reserve')
    # Adoption: the same local boot and guard invocation, and the same running worker boot and environment.
    need(io.local_boot_id()==guard['boot_id'],'local boot changed since qualification')
    unit=r['guard_unit']+'.service';live=io.show(unit)
    need(live.get('Id')==unit and live.get('ActiveState')=='active' and live.get('InvocationID')==identity['invocation_id'] and str(live.get('MainPID'))==str(guard['pid']),'guard not live or replaced')
    info=io.info();identity_check(info)
    env=profile()['future_environment']
    need(info['state'] in ('ready','idle','running') and info['environmentVersion']==env['version'],'worker not running the qualified environment')
    need(not info.get('setup_error_present') and info.get('setupStatus') not in ('failed','error'),'worker setup failed')
    fact_bytes=read_owned(Path(fact_path).absolute());e=json.loads(fact_bytes);freshness(e,io.wall(),60)
    need(e.get('id')==env['id'] and e.get('latestVersionNumber')==env['version'] and e.get('latestVersionId')==r['environment']['latest_version_id'] and e.get('hook_sha256')==hook_sha() and e.get('hook_bytes')==env['hook_bytes'] and e.get('setupBlocking') is True,'environment or hook drift since qualification')
    need(io.worker_boot_id()==identity['worker_boot_id'],'worker rebooted or replaced since qualification')
    observed_wall,observed_boot=io.wall(),io.mono()
    new_wall,new_boot=observed_wall+seconds,observed_boot+seconds
    need(observed_wall+lifetime.CLOSURE_RESERVE<wall and observed_boot+lifetime.CLOSURE_RESERVE<boot,'lifetime already inside the closure reserve')
    need(new_wall>=wall and new_boot>=boot,'extension would shorten the lifetime')
    need(new_wall<=intent['wall']+lifetime.MAX_LIFETIME and new_boot<=intent['mono']+lifetime.MAX_LIFETIME,'extension beyond the total lifetime cap')
    started=io.mono();ttl=seconds+lifetime.PROVIDER_MARGIN;attempt='lifetime-extension-attempt-'+secrets.token_hex(16)
    io.record(attempt+'.json',{'worker':worker_id(),'sequence':count+1,'observed_wall':observed_wall,'observed_boottime':observed_boot,'seconds':seconds,'ttl_seconds':ttl,'reason':reason})
    try:
        result=io.boat(['extend',worker_id(),'--ttl',str(ttl)],20)
        need(result['exit']==0,'provider extension uncertain')
        after=io.info();identity_check(after)
        need(after['state'] in ('ready','idle','running') and after['environmentVersion']==env['version'],'provider extension left the worker in another state')
        archive=timestamp(after.get('archiveAfter'))
        need(archive>=new_wall+lifetime.STOP_MARGIN,'provider lifetime not confirmed past the new end')
        # The act's bound, the previous end, the stop path and the guard are checked
        # again just before publication. The guard holds the lock before it stops.
        live=io.show(unit);need(live.get('ActiveState')=='active' and live.get('InvocationID')==identity['invocation_id'],'guard not live or replaced')
        for name in ('stop-trigger.json','stop-intent.json','closed.json','unresolved.json'):need(not io.exists(name),'operation is stopping or closed')
        # The last blocking call is behind us. Publication keeps the closure reserve
        # of the previous end, and the receipt records this time for every reader.
        published_wall,published_boot=io.wall(),io.mono()
        need(published_boot-started<=EXTEND_ACT_MAX,'extension act exceeded its bound')
        need(published_wall+lifetime.CLOSURE_RESERVE<wall and published_boot+lifetime.CLOSURE_RESERVE<boot,'previous end reached during the extension act')
        receipt={'schema':lifetime.EXTENSION_SCHEMA,'version':1,'sequence':count+1,'worker':worker_id(),'operation_id':r['guard_unit'],
                 'guard':{'boot_id':identity['boot_id'],'invocation_id':identity['invocation_id']},
                 'observed_wall':observed_wall,'observed_boottime':observed_boot,'published_wall':published_wall,'published_boottime':published_boot,'seconds':seconds,'wall_end':new_wall,'boot_end':new_boot,
                 'provider':{'ttl_seconds':ttl,'archive_after':after['archiveAfter'],'archive_after_unix':archive},
                 'adoption':{'worker_boot_id':identity['worker_boot_id'],'local_boot_id':guard['boot_id'],'guard_pid':guard['pid'],
                             'environment_version':env['version'],'environment_version_id':r['environment']['latest_version_id'],'hook_sha256':hook_sha(),
                             'environment_fact_sha256':digest(fact_bytes)},
                 'reason':reason}
        io.record('lifetime-extension-%04d.json'%(count+1),receipt)
        return receipt
    except Exception as error:
        io.record(attempt+'-failed.json',{'extended':False,'reason':str(error) if isinstance(error,Refusal) else 'uncertain','provider_lifetime_may_be_longer':True})
        raise

def extend_entry(op,seconds,fact_path,reason):
    r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);verify_cli(r)
    # One extension at a time per operation, so a provider TTL cannot be set by a racing act.
    lock=open_lock(op/EXTENSION_LOCK)
    try:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        io=RealIO(r,op);receipt=extend_operation(r,io,seconds,fact_path,reason)
    finally:os.close(lock)
    # The receipt grants time only once the guard accepts it before the previous end.
    name='lifetime-extension-%04d-accepted.json'%receipt['sequence'];end=io.mono()+15
    while io.mono()<end and not io.exists(name):io.wait(0.2)
    receipt['accepted_by_guard']=io.exists(name)
    return receipt

def verify_packet(expected):
    b=read_owned(ROOT/'runtime-sources.json');need(digest(b)==expected,'controller seal mismatch')
    m=json.loads(b)
    for name,row in m['files'].items():
        need('/' not in name and name not in ('.','..'),'packet member path')
        p=ROOT/name;v=read_owned(p,2*1024*1024)
        need(digest(v)==row['sha256'] and len(v)==row['bytes'] and stat.S_IMODE(p.stat().st_mode)==row['mode'],'controller member changed')

def verify_cli(r):
    b=r['boat'];p=Path(b['path']);need(p.resolve()==p,'CLI not physical')
    s=p.lstat();need(stat.S_ISREG(s.st_mode) and s.st_uid==b['uid'] and stat.S_IMODE(s.st_mode)==b['mode'] and s.st_size==b['bytes'] and not s.st_mode&0o022 and s.st_mode&0o111,'CLI metadata')
    h=hashlib.sha256()
    with p.open('rb') as f:
        for part in iter(lambda:f.read(1024*1024),b''):h.update(part)
    need(h.hexdigest()==b['sha256'],'CLI bytes')

def guard_entry(op):
    r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);verify_cli(r)
    io=RealIO(r,op);intent=load(op/'controller-intent.json')
    line=Path('/proc/self/cgroup').read_text().strip();need(line.startswith('0::'),'guard cgroup v2')
    cg=Path('/sys/fs/cgroup')/line[3:].lstrip('/')
    need(cg.name==r['guard_unit']+'.service','guard cgroup identity')
    io.record('guard-ready.json',{'pid':os.getpid(),'uid':os.getuid(),'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'deadline_mono':intent['deadline_mono'],'memory_max':(cg/'memory.max').read_text().strip(),'swap_max':(cg/'memory.swap.max').read_text().strip(),'cgroup':str(cg)})
    return 0 if guard_loop(r,io,intent) else 1

def startup_scope_gate(pins,mode):
    need(mode=='normal_pinned_provisioning','check-only prebootstrap mode unavailable')
    need(pins['bootstrap_contract']=='existing_lock_manifest_fence_v1','unreviewed startup contract')
    return {'mode':mode,'first_bootstrap_allowed':True,'prebootstrap_interior_observed':False,'bootstrap_occurrence':'unknown','installer_closure_required':True}

def bind_release_profile(r):
    raw=r.get('_profile_json');need(isinstance(raw,str)and digest(raw.encode())==r.get('profile_sha256'),'private operation profile binding')
    p=bind_profile(json.loads(raw));need(r.get('worker')==p['worker']and r.get('profile_review_sha256')==p['profile_review_sha256'],'owned profile/release mismatch')
    return r

def run_entry(release,profile_path,owned_worker):
    raw=read_owned(profile_path).decode();p=bind_profile(json.loads(raw));need(owned_worker==p['worker'],'explicit owned worker mismatch')
    r=load(release);need(r.get('profile_sha256')==digest(raw.encode()),'profile file hash mismatch');r['_profile_json']=raw;bind_release_profile(r)
    validate_release(r,time.time());verify_packet(r['controller_packet_manifest_sha256'])
    startup_scope_gate(profile(),r['startup_mode'])
    verify_cli(r)
    op=Path(r['operation']);need(op.is_absolute() and op.parent.resolve()==op.parent,'operation parent')
    op.mkdir(mode=0o700);write_new(op,'release.json',r)
    # This lock must be the common Root environment-edit exclusion, not a private imitation.
    data=read_owned(r['environment']['lock_path']);need(len(data)<=4096,'lock receipt bound')
    lock=os.open(r['environment']['lock_path'],os.O_RDWR|os.O_NOFOLLOW)
    try:
        fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        io=RealIO(r,op)
        status=drive(r,io);io.record('controller-result.json',{'status':status,'worker':worker_id(),'runtime_job_started':False})
        if status!='QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE':
            end=io.mono()+145
            while io.mono()<end:
                if io.exists('closed.json') or io.exists('closed-no-start.json') or io.exists('closed-no-resume.json'):return 0
                if io.exists('unresolved.json'):return 2
                io.wait(1)
            raise Refusal('guard completion not observed; retain guard and reconcile')
        return 0
    finally:os.close(lock)

def finish_operation(op,job_closure=None,seconds=180):
    """Publish only an externally verified closure, then observe the sole guard."""
    r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);verify_cli(r)
    io=RealIO(r,op)
    try:return _finish(r,io,job_closure,seconds)
    except Exception as error:
        record_refusal(io,'closure',error);raise

def _finish(r,io,job_closure,seconds):
    if handoff_active(io):
        if job_closure is not None:
            closed=load(job_closure);matching_root_job_closed(r,io,closed)
            if io.exists('root-job-closed.json'):need(io.read('root-job-closed.json')==closed,'closure replacement refused')
            else:io.record('root-job-closed.json',closed)
        matching_root_job_closed(r,io,io.read('root-job-closed.json'))
    if not io.exists('closed.json'):io.trigger()
    return await_stop(r,io,seconds)

def await_stop(r,io,seconds=180):
    """Observe the guard's verified stop and its own clean exit, within ``seconds``."""
    end=io.mono()+seconds
    while io.mono()<end:
        if io.exists('unresolved.json'):raise Refusal('stop unresolved; owner retains reconciliation')
        if io.exists('closed.json'):
            closed=io.read('closed.json');need(closed.get('worker')==worker_id()and closed.get('stopped_confirmed')is True and closed.get('owned_metadata_closed')is True and closed.get('guard_may_exit')is True,'terminal closure mismatch')
            request=io.read('stop-intent.json');stop_complete(closed['safe_info'],closed['stop_operation'],request['requested_wall'],r['snapshot_continuity']['snapshot_completed_at'],r['provider_contract']['snapshot_policy'])
            loaded=io.show(r['guard_unit']+'.service');original=io.read('guard-loaded.json')
            need(Path('/proc/sys/kernel/random/boot_id').read_text().strip()==original['boot_id'],'guard boot changed during finish')
            need(loaded.get('Id')==r['guard_unit']+'.service'and loaded.get('InvocationID')==original['loaded']['InvocationID'],'guard replaced during finish')
            if loaded.get('ActiveState')in('inactive','failed')and loaded.get('MainPID')=='0':
                need(loaded.get('ExecMainStatus')=='0','guard did not exit cleanly')
                cg=loaded.get('ControlGroup','')
                if cg:
                    root=Path('/sys/fs/cgroup')/cg.lstrip('/')
                    need(not root.exists()or all(not f.read_text().strip()for f in root.rglob('cgroup.procs')),'guard cgroup not empty')
                return {'worker':worker_id(),'stopped_confirmed':True,'guard_closed':True,'stop_operation':closed['stop_operation']}
        io.wait(0.2)
    raise Refusal('verified stop not observed within finish bound; do not retry lifecycle')


def watch_ends(r,io):
    """The guard's present end pair and whether an extension awaits its acceptance.

    The watch only reads. It starts from the guard's conservative base, applies
    the extensions the guard accepted, and falls back to the base on any damaged
    chain, so a reading error can only move its check earlier.
    """
    intent=io.read('controller-intent.json');guard=io.read('guard-admission.json');handoff=io.read('root-handoff.json')
    wall,boot=lifetime.original_ends(intent,guard,handoff)
    wall,boot=min(wall,intent['deadline_wall']),min(boot,intent['deadline_mono'])
    names=io.names();numbers=lifetime.extension_names(names)
    io.watch_granted=0
    if not numbers:return wall,boot,False
    try:
        identity=lifetime.operation_identity(intent,guard,io.read('qualification.json'),r['guard_unit'],profile())
        ext_wall,ext_boot,granted=lifetime.apply_extensions(names,io.read,intent,wall,boot,identity)
    except Exception:return wall,boot,False
    io.watch_granted=granted
    following=granted+1
    pending=following in numbers and 'lifetime-extension-%04d-accepted.json'%following not in names and 'lifetime-extension-%04d-vetoed.json'%following not in names
    return ext_wall,ext_boot,pending

def pause(io,seconds):
    """Wait ``seconds`` on the boot clock, whatever step the IO's own wait takes."""
    end=io.mono()+seconds
    while io.mono()<end:io.wait(end-io.mono())

def job_closed(r,io,check):
    q=io.read('qualification.json')
    return (check.get('list_exit')==0 and check.get('units')==[] and check.get('unit_count')==0 and check.get('processes')==[] and check.get('process_count')==0
            and isinstance(check.get('boot_id'),str) and check['boot_id']==q.get('worker_boot_id') and all(v is True for v in check['local_units_closed'].values()))

def closure_watch(r,io,lead=WATCH_LEAD,local_units=(),poll=WATCH_POLL,observe=await_stop):
    """Close a warm worker's operation before its deadline, once its jobs have ended.

    A worker kept warm past its jobs would otherwise reach the guard's deadline
    with no job-closure record, and the guard would leave it to the provider TTL.
    ``lead`` seconds before the present end, extensions included, the watch
    checks that no job unit or job process remains on the worker and that the
    named local job units are inactive. When a check finds them closed, it takes
    the extension lock and then the operation's job lock exclusively. Every Boat
    command holds that job lock shared for its whole life from admission, so the
    exclusive lock proves that no command is running or launching, and new
    commands wait while the watch holds it. Within WATCH_DECISION seconds the
    watch waits out a pending acceptance, checks the jobs again, and publishes
    the job-closure record from that check with the stop trigger only if the
    end, the accepted extensions and the terminal receipts are unchanged. It then
    observes the guard's verified stop. An open job is checked again every
    ``poll`` seconds. If none closes by WATCH_FLOOR seconds before the end, the
    watch records that and leaves the guard's stop path unchanged.
    """
    need(type(lead)is int and 300<=lead<=3600,'closure watch lead out of range')
    need(len(local_units)<=8 and all(isinstance(u,str) and re.fullmatch(r'[A-Za-z0-9@_.:-]{1,200}',u) for u in local_units),'local job unit name')
    need(handoff_active(io) and io.exists('guard-admission.json'),'no qualified Root handoff to watch')
    io.record('closure-watch.json',{'worker':worker_id(),'operation_id':r['guard_unit'],'lead_seconds':lead,'poll_seconds':poll,'local_units':list(local_units),'started_wall':io.wall(),'started_boottime':io.mono()})
    checks=0;last='not_checked'
    terminal=('stop-trigger.json','closed.json','unresolved.json','root-job-closed.json')
    def unresolved(code):
        record_refusal(io,'closure',code='closure_watch:'+code)
        io.record('closure-watch-unresolved.json',{'worker':worker_id(),'reason_code':code,'checks':checks,'observed_wall':io.wall(),'action':'the guard keeps its own deadline stop; Root must establish job closure or reconcile the worker'})
        return 'UNRESOLVED_JOB_OPEN'
    def check_jobs(seconds):
        nonlocal checks,last
        checks+=1
        try:check=io.job_closure(local_units,seconds)
        except Exception as error:last=reason_code(error);return None
        check['observed_unix']=io.wall();check['closed']=job_closed(r,io,check)
        io.record('closure-watch-check-%04d.json'%checks,check)
        if not check['closed']:last='job_open'
        return check
    while True:
        if io.exists('closed.json'):return 'CLOSED'
        if io.exists('unresolved.json'):return 'GUARD_UNRESOLVED'
        if io.exists('root-job-closed.json'):
            observe(r,io);return 'FINISHED'
        wall,boot,pending=watch_ends(r,io)
        left=min(wall-io.wall(),boot-io.mono())
        if left<=WATCH_FLOOR:return unresolved(last)
        if left>lead:pause(io,min(poll,left-lead));continue
        if io.exists('stop-trigger.json'):return unresolved('stop_triggered_without_job_closure')
        if checks>=WATCH_CHECKS:return unresolved('closure_check_bound')
        check=check_jobs(60)
        if check is None or not check['closed']:pause(io,poll);continue
        if not io.freeze_extensions():
            last='extension_act_running';pause(io,1);continue
        published=False
        try:
            # The whole decision ends within WATCH_DECISION, inside the guard's
            # FREEZE_WAIT for this lock.
            decision_end=io.mono()+WATCH_DECISION
            if not io.lock_jobs():last='job_command_live'
            else:
                try:
                    # An act published before the lock may still await the guard's
                    # acceptance, which it gives within a second of its next pass.
                    # One it has not accepted after WATCH_ACCEPT seconds grants
                    # nothing to this decision, and a stop now ends the lifetime sooner.
                    wall,boot,pending=watch_ends(r,io);accept_end=io.mono()+WATCH_ACCEPT
                    while pending and io.mono()<accept_end:
                        io.wait(1);wall,boot,pending=watch_ends(r,io)
                    granted=io.watch_granted;left=min(wall-io.wall(),boot-io.mono())
                    if left<=lead and left>WATCH_FLOOR and not any(io.exists(n) for n in terminal):
                        check=check_jobs(decision_end-io.mono()) if decision_end-io.mono()>=15 else None
                        if check is not None and check['closed']:
                            after_wall,after_boot,_=watch_ends(r,io);left=min(after_wall-io.wall(),after_boot-io.mono())
                            if (io.mono()<decision_end and (after_wall,after_boot)==(wall,boot) and io.watch_granted==granted and WATCH_FLOOR<left<=lead
                                    and io.wall()-check['observed_unix']<=WATCH_DECISION and not any(io.exists(n) for n in terminal)):
                                admission=io.read('qualified-worker-admission.json');handoff=io.read('root-handoff.json')
                                row={'observed_unix':check['observed_unix'],'verified_by_Root':True,'receipt_sha256':lifetime.receipt_digest(check),'worker':worker_id(),
                                     'operation_id':r['guard_unit'],'local_guard':admission['local_guard'],'job_release_sha256':handoff['separate_job_release']['job_release_sha256'],
                                     'all_native_and_leases_closed':True,'closure_check':'closure-watch-check-%04d.json'%checks}
                                matching_root_job_closed(r,io,row);io.record('root-job-closed.json',row);io.trigger();published=True
                            else:last='closure_decision_changed'
                finally:io.unlock_jobs()
        finally:io.thaw_extensions()
        if not published:pause(io,poll);continue
        observe(r,io);return 'FINISHED'

def closure_watch_entry(op,lead,local_units):
    r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);verify_cli(r)
    io=RealIO(r,op)
    try:return closure_watch(r,io,lead,tuple(local_units))
    except Exception as error:
        record_refusal(io,'closure',error)
        try:io.record('closure-watch-unresolved.json',{'worker':worker_id(),'reason_code':reason_code(error),'observed_wall':io.wall(),'action':'the guard keeps its own deadline stop; Root must establish job closure or reconcile the worker'})
        except Exception:pass
        raise

def main():
    parser=argparse.ArgumentParser();parser.add_argument('action',choices=['run','guard','request-stop','finish','extend','closure-watch']);parser.add_argument('--release');parser.add_argument('--operation');parser.add_argument('--profile');parser.add_argument('--owned-worker');parser.add_argument('--job-closure');parser.add_argument('--seconds',type=int);parser.add_argument('--environment-fact');parser.add_argument('--reason');parser.add_argument('--lead',type=int,default=WATCH_LEAD);parser.add_argument('--local-unit',action='append',default=[]);a=parser.parse_args()
    try:
        if a.action=='run':
            need(a.release is not None and a.profile is not None and a.owned_worker is not None,'explicit owned worker/profile/release required')
            return run_entry(Path(a.release).absolute(),Path(a.profile).absolute(),a.owned_worker)
        need(a.operation is not None,'operation required');op=Path(a.operation).absolute()
        if a.action=='guard':return guard_entry(op)
        if a.action=='extend':
            need(a.seconds is not None and a.environment_fact is not None and a.reason is not None,'extension seconds, environment fact and reason required')
            receipt=extend_entry(op,a.seconds,Path(a.environment_fact).absolute(),a.reason)
            print(json.dumps({'extended':receipt['accepted_by_guard'],'worker':receipt['worker'],'sequence':receipt['sequence'],'wall_end':receipt['wall_end'],'boot_end':receipt['boot_end']}));return 0 if receipt['accepted_by_guard'] else 1
        if a.action=='closure-watch':
            status=closure_watch_entry(op,a.lead,a.local_unit);print(json.dumps({'status':status,'worker':worker_id()}));return 0 if status in ('FINISHED','CLOSED') else 2
        if a.action=='finish':
            result=finish_operation(op,None if a.job_closure is None else Path(a.job_closure).absolute());print(json.dumps(result));return 0
        r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);io=RealIO(r,op)
        need(not io.exists('closed.json') and io.exists('guard-ready.json'),'operation not active')
        if handoff_active(io):
            closed=io.read('root-job-closed.json');matching_root_job_closed(r,io,closed)
        io.trigger();return 0
    except Exception as error:
        # Never print arbitrary subprocess/provider exception text or environment.
        print(json.dumps({'status':'REFUSED_OR_UNCERTAIN','worker':worker_id(),'resume_retry':False,'Root_reconcile_owned_guard_and_receipts':True,'reason_code':reason_code(error)}));return 1
if __name__=='__main__':raise SystemExit(main())
