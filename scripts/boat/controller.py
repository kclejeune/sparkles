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
import argparse,fcntl,math,secrets,selectors,signal,subprocess,time,shlex,base64

# An extension act publishes within EXTEND_ACT_MAX seconds of its start, and the
# guard waits up to FREEZE_WAIT for it before it stops at its deadline. The guard
# unit outlives the longest permitted lifetime, extensions included, by that wait
# and the stop margin. The provider TTL remains the worker's own backstop.
EXTEND_ACT_MAX=90
FREEZE_WAIT=100
GUARD_RUNTIME=lifetime.MAX_LIFETIME+lifetime.STOP_MARGIN+FREEZE_WAIT
EXTENSION_LOCK='lifetime-extension.lock'

ROOT=Path(__file__).resolve().parent

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
        fd=os.open(self.op/EXTENSION_LOCK,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600);end=self.mono()+FREEZE_WAIT
        while True:
            try:fcntl.flock(fd,fcntl.LOCK_EX|fcntl.LOCK_NB);self._frozen=fd;return True
            except BlockingIOError:
                if self.mono()>=end:os.close(fd);return False
                time.sleep(0.2)
    def thaw_extensions(self):
        fd=getattr(self,'_frozen',None);self._frozen=None
        if fd is not None:os.close(fd)
    def worker_boot_id(self):
        code="import json;print(json.dumps({'boot_id':open('/proc/sys/kernel/random/boot_id').read().strip()}))"
        r=self.boat(['ssh',worker_id(),'--','/usr/bin/python3','-I','-B','-c',code],20,json_mode=False)
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
        r=self.boat(['ssh',worker_id(),'--','/usr/bin/python3','-I','-B','-c',code,unit],20,json_mode=False)
        need(r['exit']==0 and isinstance(r['body'],dict) and r['body'].get('closed')is True and r['body'].get('unit')==unit,'metadata closure query failed')
        self.record('qualification-closed.json',r['body']);return True
    def phase(self,phase,observation_seconds=0):
        payload={'pins':profile(),'release':{'snapshot_continuity':self.r['snapshot_continuity']},'operation_tag':self.r['guard_unit'],'phase':phase,'observation_seconds':observation_seconds}
        common=(ROOT/'config_schema.py').read_text()+'\n'+(ROOT/'common.py').read_text()
        definitions=(ROOT/'metadata-probe.py').read_text().split("\nif __name__=='__main__':",1)[0]
        observer=(ROOT/'startup-observer.py').read_text()
        entry="""
if __name__=='__main__':
    payload=json.loads(sys.argv[1]);bind_profile(payload['pins'])
    try:
        result=observe_startup(payload) if payload['phase']!='metadata' else qualify_with_startup_lock(payload)
    except Exception:
        result={'qualified':False,'startup_closed':False,'engine_or_job_started':False,'failure':'bounded phase refused; no arbitrary error content'}
    result.update({'program_sha256':payload['program_sha256'],'phase':payload['phase'],'unit':payload['operation_tag']+'-'+payload['phase']+'.service'})
    write_new(Path(payload['result_directory']),'probe-result.json',result)
    print(json.dumps(result))
    raise SystemExit(0 if (result.get('startup_closed')is True if payload['phase']!='metadata' else result.get('qualified')is True) else 1)
"""
        program=(common+'\n'+definitions+'\n'+observer+'\n'+entry).encode()
        source=(ROOT/'remote-launcher.py').read_text().replace('COMMON_SOURCE_LITERAL',repr(common)).replace('PAYLOAD_JSON_LITERAL',repr(json.dumps(payload))).replace('PROBE_BASE64_LITERAL',repr(base64.b64encode(program).decode())).encode()
        need(len(source)<262144,'remote phase stdin bound')
        seconds=330 if phase=='metadata' else (45 if phase=='stopcheck' else observation_seconds+85)
        r=self.boat(['ssh',worker_id(),'--','/usr/bin/python3','-I','-B','-'],seconds,source,False)
        need(r['exit']==0 and isinstance(r['body'],dict),'remote phase invocation failed')
        return r['body']
    def startup(self,intent,allow_qualification):
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
        result=self.phase('startup',seconds)
        if result.get('startup_closed')is not True or result.get('phase_closed')is not True:return False
        self.record('startup-closed.json',result)
        if allow_qualification:need(result.get('qualification_eligible')is True,'allowed startup produced unadopted manifest')
        return True
    def stop_quiescent(self):
        if self.exists('stop-startup-closed.json'):return True
        result=self.phase('stopcheck',5)
        need(result.get('startup_closed')is True and result.get('phase_closed')is True,'current startup/installer closure unavailable before stop')
        self.record('stop-startup-closed.json',result)
        if not self.exists('startup-closed.json'):self.record('startup-closed.json',result)
        return True
    def qualification(self,intent):
        budget(self.mono(),min(intent['deadline_mono'],self.mono()+intent['deadline_wall']-self.wall()),330)
        need(self.exists('startup-closed.json'),'startup closure absent')
        self.record('metadata-started.json',{'unit':self.r['guard_unit']+'-metadata.service','wall':self.wall()})
        q=self.phase('metadata')
        if q.get('metadata_closed')is True:self.record('qualification-closed.json',{'metadata_closed':True,'unit':self.r['guard_unit']+'-metadata.service'})
        need(q.get('qualified')is True and q.get('startup_closed')is True and q.get('metadata_closed')is True and q.get('engine_or_job_started')is False,'metadata/startup closure refused')
        need(q.get('memory_max')==str(MEMORY) and q.get('swap_max')=='0' and q.get('manifest_unchanged')is True,'metadata cap/pins refused')
        need(isinstance(q.get('worker_boot_id'),str) and re.fullmatch(lifetime.UUID,q['worker_boot_id']) is not None,'worker boot identity missing')
        self.record('qualification.json',q);return q


def no_ready(result):
    b=result.get('body')
    return result.get('exit') not in (0,None) and isinstance(b,dict) and b.get('ok') is False and b.get('code')=='no_ready_machine' and isinstance(b.get('error'),dict) and b['error'].get('code')=='no_ready_machine'

def drive(r,io):
    validate_release(r,io.wall())
    before=io.info();stopped(before);need(before['environmentVersion']==r['snapshot_continuity']['pre_resume_environment_version'],'unexpected pre-resume environment')
    need(before['snapshotCompletedAt']==r['snapshot_continuity']['snapshot_completed_at'],'snapshot continuity drift')
    intent={'worker':worker_id(),'wall':io.wall(),'mono':io.mono(),'deadline_mono':io.mono()+10500,'deadline_wall':io.wall()+10500,'ttl':10800,'resume_count':1}
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
            h=r.get('separate_Root_handoff',{})
            need(sha(h.get('job_release_sha256')) and sha(h.get('different_job_review_sha256')) and h.get('Root_owns_job_and_deadline_closure') is True,'unbound useful job handoff')
            budget(io.mono(),min(intent['deadline_mono'],io.mono()+expiry-io.wall()),h.get('finite_job_and_closure_seconds',10800))
            io.record('root-handoff.json',{'worker':worker_id(),'separate_job_release':h,'deadline_wall':intent['deadline_wall'],'qualification_only_controller':True,'job_started_by_controller':False})
            admission=make_admission(r,intent,io.read('qualification.json'),io.read('startup-closed.json'),io.guard_for_admission(),io.read('root-handoff.json'),io.wall())
            io.record('qualified-worker-admission.json',admission)
            return 'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE'
        return 'QUALIFIED_STOP_REQUESTED'
    except Exception:
        io.record('qualification-failed.json',{'qualification':False,'resume_not_retried':True,'Root_must_reconcile_if_guard_unresolved':True})
        raise
    finally:
        if not io.exists('no-ready-confirmed.json') and not io.exists('root-handoff.json'):io.trigger()


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
        if io.exists('root-handoff.json'):
            closed=io.read('root-job-closed.json');matching_root_job_closed(r,io,closed)
    except Exception:
        io.record('unresolved.json',{'worker':worker_id(),'stop_submitted':False,'ownership_retained_by_Root':True,'action':'Root must establish owned metadata/job/lease closure before normal stop; providerTTL remains finite; retain guard evidence.'})
        return False
    try:need(io.stop_quiescent(),'startup not currently quiescent')
    except Exception:
        io.record('unresolved.json',{'worker':worker_id(),'stop_submitted':False,'ownership_retained_by_Root':True,'action':'current bootstrap-lock/installer closure unavailable; no clean stop claim'})
        return False
    request=io.wall();stop_end=io.mono()+120;io.record('stop-intent.json',{'worker':worker_id(),'requested_wall':request,'force':False,'single_request':True})
    result=io.boat(['stop',worker_id()],20);operation=None
    b=result.get('body')
    if isinstance(b,dict) and b.get('id')==worker_id():
        s=b.get('operation') or b.get('stop')
        if isinstance(s,dict) and isinstance(s.get('id'),str) and s['id'].startswith('stop_'):operation=s['id']
    io.record('stop-submission.json',{'exit':result['exit'],'transport_error':result['error'],'operation_id':operation,'completed':False})
    end=stop_end;count=0
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
            if io.exists('root-handoff.json'):
                closed=io.read('root-job-closed.json')
                matching_root_job_closed(r,io,closed)
            io.record('closed.json',{'worker':worker_id(),'stop_operation':op,'safe_info':info,'stopped_confirmed':True,'snapshot_available':True,'owned_metadata_closed':True,'private_credential_snapshot':True,'guard_may_exit':True})
            return True
        except (Refusal,FileNotFoundError):io.wait(1)
    io.record('unresolved.json',{'worker':worker_id(),'stop_operation':operation,'resume_retried':False,'stop_retried_or_forced':False,'ownership_retained_by_Root':True,'action':'Root must reconcile exact worker/operation; retain failed guard unit, do not declare shutdown/disarm/clean.'})
    return False

def guard_deadlines(io,intent):
    """The guard's (BOOTTIME, wall) deadline, moved only by valid extension receipts.

    Before a Root handoff this is the intent's own pair. After it, the base is the
    admission's conservative original pair, never later than the intent's. Any
    missing, unreadable or malformed receipt chain falls back to that base, so
    lost or damaged evidence can only end the lifetime sooner.
    """
    original=(intent['deadline_mono'],intent['deadline_wall'])
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
            io.record('lifetime-extension-%04d-accepted.json'%number,row)
            if not(io.wall()<wall and io.mono()<boot):
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
            if io.freeze_extensions():
                deadline_mono,deadline_wall=guard_deadlines(io,intent)
                if io.mono()<deadline_mono and io.wall()<deadline_wall:
                    io.thaw_extensions();continue
            # The stopping state is published before the stop, so admission refuses it.
            io.trigger()
            break
        if io.exists('no-ready-confirmed.json'):
            check=io.read('no-ready-confirmed.json');stopped(check['safe_info'])
            io.record('closed-no-start.json',{'worker':worker_id(),'no_ready_confirmed':True,'guard_may_exit':True});return True
        if io.exists('cancel-before-resume.json') and not io.exists('resume-intent.json'):
            io.record('closed-no-resume.json',{'no_resume_intent':True,'guard_may_exit':True});return True
        if io.exists('stop-trigger.json'):
            # An explicit stop also waits out and then excludes any extension act.
            io.freeze_extensions()
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
    need(io.exists('root-handoff.json') and io.exists('qualified-worker-admission.json') and io.exists('guard-admission.json'),'no qualified Root handoff to extend')
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
    lock=os.open(op/EXTENSION_LOCK,os.O_RDWR|os.O_CREAT|os.O_NOFOLLOW,0o600)
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
    if io.exists('root-handoff.json'):
        if job_closure is not None:
            closed=load(job_closure);matching_root_job_closed(r,io,closed)
            if io.exists('root-job-closed.json'):need(io.read('root-job-closed.json')==closed,'closure replacement refused')
            else:io.record('root-job-closed.json',closed)
        matching_root_job_closed(r,io,io.read('root-job-closed.json'))
    if not io.exists('closed.json'):io.trigger()
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


def main():
    parser=argparse.ArgumentParser();parser.add_argument('action',choices=['run','guard','request-stop','finish','extend']);parser.add_argument('--release');parser.add_argument('--operation');parser.add_argument('--profile');parser.add_argument('--owned-worker');parser.add_argument('--job-closure');parser.add_argument('--seconds',type=int);parser.add_argument('--environment-fact');parser.add_argument('--reason');a=parser.parse_args()
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
        if a.action=='finish':
            result=finish_operation(op,None if a.job_closure is None else Path(a.job_closure).absolute());print(json.dumps(result));return 0
        r=bind_release_profile(load(op/'release.json'));verify_packet(r['controller_packet_manifest_sha256']);io=RealIO(r,op)
        need(not io.exists('closed.json') and io.exists('guard-ready.json'),'operation not active')
        if io.exists('root-handoff.json'):
            closed=io.read('root-job-closed.json');matching_root_job_closed(r,io,closed)
        io.trigger();return 0
    except Exception:
        # Never print arbitrary subprocess/provider exception text or environment.
        print(json.dumps({'status':'REFUSED_OR_UNCERTAIN','worker':worker_id(),'resume_retry':False,'Root_reconcile_owned_guard_and_receipts':True}));return 1
if __name__=='__main__':raise SystemExit(main())
