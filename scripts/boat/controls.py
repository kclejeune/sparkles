"""Finite inert controls. No subprocess, provider, manager, host or real token."""
from pathlib import Path
import sys
sys.path.insert(0,str(Path(__file__).resolve().parent))
import controller as c
from common import *
import copy,unittest,tempfile,importlib.util,ast,resource,signal,fcntl
from unittest.mock import patch
from datetime import datetime,timezone
SPEC=importlib.util.spec_from_file_location('metadata_probe',Path(__file__).with_name('metadata-probe.py'))
p=importlib.util.module_from_spec(SPEC);SPEC.loader.exec_module(p)

# Exact same source composition used by Root's remote service, without its entry.
OBSERVER={"__name__":"inert_startup_definitions"}
exec(compile(Path(__file__).with_name('common.py').read_text()+'\n'+Path(__file__).with_name('metadata-probe.py').read_text().split("\nif __name__=='__main__':",1)[0]+'\n'+Path(__file__).with_name('startup-observer.py').read_text(),'inert-composed-startup','exec'),OBSERVER)

def synthetic_profile(worker='bx_fixture'):
    p=json.loads(Path(__file__).with_name('profile.FALSE.json').read_text())
    p.update(admitted_by_Root=True,profile_review_sha256='a'*64,worker=worker,worker_uid=1000,worker_home='/home/worker-fixture',installed_bundle='/home/worker-fixture/setup-new',retained_old_bundle='/home/worker-fixture/setup-old',saved_toolchain_manifest_sha256='b'*64,stop_contract_sha256='c'*64)
    p['mbx_remote_config']={'url':'https://cache.invalid','namespace':'fixture/dev','mode':'read-only'}
    p['future_environment'].update(id='fixture-environment-id',name='fixture-environment',version=3,version_id='fixture-version-id',hook_sha256='d'*64,hook_bytes=2000)
    p['startup_repositories']={'default':'/home/worker-fixture/default','retained':'/home/worker-fixture/retained'}
    for group in ['runtime_members','old_runtime_members']:
        for row in p[group].values():row['sha256']='e'*64
    return p

def cli_cases():
    return {'results':[{'name':'synthetic-positive','exit':0,'stdout':json.dumps({'event':'accepted','id':worker_id(),'status':'resuming'})},{'name':'synthetic-null-status','exit':0,'stdout':json.dumps({'event':'accepted','id':worker_id(),'status':None})},{'name':'synthetic-malformed','exit':0,'stdout':json.dumps({'event':'accepted','id':worker_id(),'status':None})}]}

def limits_fixture():
    return {'safe_body':{'starts':{'unlimited':False,'day':{'limit':200,'used':4,'remaining':196},'hour':{'limit':60,'used':2,'remaining':58},'minute':{'limit':12,'used':1,'remaining':11}},'machineTypes':[{'type':'large','available':True}]}}

def iso(t):return datetime.fromtimestamp(t,timezone.utc).isoformat().replace('+00:00','Z')
def fixture():
    p=bind_profile(synthetic_profile());OBSERVER['bind_profile'](p);raw=json.dumps(p,sort_keys=True)
    r=json.loads(Path(__file__).with_name('release.FALSE.json').read_text());r.update(profile_sha256=digest(raw.encode()),profile_review_sha256=p['profile_review_sha256'],_profile_json=raw);now=1800000000
    r.update(authorized_by_Root=True,private_credential_snapshot_acknowledged=True,worker=p['worker'],controller_packet_manifest_sha256='1'*64,different_controller_review_sha256='2'*64,operation='/tmp/boat-inert-qualification',guard_unit='boat-owned-warm-qual-inert',root_exclusive_worker_use=True)
    for name in ['local_first','start_ledger','environment','snapshot_continuity','previous_native_closure','provider_contract']:r[name].update(observed_unix=now,verified_by_Root=True,receipt_sha256='3'*64)
    r['local_first']['overflow_currently_needed']=True
    r['start_ledger'].update(starts_and_reservations=1,this_one_resume_reserved=True,all_actors_included=True,window='synthetic-day',timezone='UTC',provider_rate_admitted=True)
    r['environment'].update(assigned_editors_excluded=True,environment_id=p['future_environment']['id'],lock_path='/tmp/boat-inert-environment-lock',name=p['future_environment']['name'],version=p['future_environment']['version'],latest_version_id=p['future_environment']['version_id'],hook_sha256=p['future_environment']['hook_sha256'])
    src={'head':'4'*40,'status_sha256':'5'*64,'tracked_diff_sha256':'6'*64,'inputs':{'mise.toml':'7'*64,'rust-toolchain.toml':'8'*64}}
    r['snapshot_continuity'].update(manifest_sha256=p['saved_toolchain_manifest_sha256'],pre_resume_environment_version=2,startup_repo=p['startup_repositories']['default'],token_path=p['worker_home']+'/.config/mbx/remote-token',continuity_is_retained_evidence_inference=True,snapshot_completed_at=iso(now-3600),dev_read_only_scope=True,hook_token_metadata_open_fchmod_accepted=True,default_source=src,retained_source=copy.deepcopy(src))
    r['previous_native_closure']['all_native_and_leases_closed']=True
    # SYNTHETIC parser fixture, never a claim of actual provider setup-success value.
    r['provider_contract'].update(final_snapshot_document_sha256=p['stop_contract_sha256'],stop_completed_requires_fresh_info=True,stop_status_schema_verified_by_Root=True,stop_status_receipt_sha256='d'*64)
    r['provider_contract']['resume_ack'].update(verified_by_Root=True,receipt_sha256=digest(json.dumps(cli_cases()).encode()))
    r['boat'].update(path='/inert/boat',sha256='9'*64,uid=os.getuid(),mode=0o755,bytes=1)
    return r

def info(r,ready=False):
    now=1800000000
    return {'id':worker_id(),'access':'owner','environment':profile()['future_environment']['name'],'environmentVersion':profile()['future_environment']['version'] if ready else r['snapshot_continuity']['pre_resume_environment_version'],'state':'idle' if ready else 'stopped','type':'large','vcpu':8,'memoryGB':16,'ttlSeconds':10800,'archiveAfter':iso(now+10800) if ready else None,'setupStatus':'synthetic-success' if ready else None,'snapshotAvailable':True,'snapshotCompletedAt':r['snapshot_continuity']['snapshot_completed_at'],'lastSnapshotStatus':'completed','stoppedAt':None if ready else iso(now-3500),'stop':{'id':'stop_old','status':'completed','error':None,'forced':False,'endedAt':iso(now-3500)}}
class InertIO:
    def __init__(self,r):
        self.r=r;self.t=100.;self.w=1800000000.;self.records={};self.calls=[];self.armed=False;self.started=False;self.stopping=False;self.mode='ok';self.qual_bad=False;self.closure=True;self.guard_bad=False;self.stopped_bad=False;self.expiry_bad=False;self.env_bad=False;self.live_lease=False
    def wall(self):return self.w
    def mono(self):return self.t
    def wait(self,s):self.t+=max(s,1);self.w+=max(s,1)
    def record(self,name,value):
        assert name not in self.records,name
        self.records[name]=copy.deepcopy(value)
    def exists(self,n):return n in self.records
    def names(self):return sorted(self.records)
    def freeze_extensions(self):self.frozen=True;return True
    def thaw_extensions(self):self.frozen=False
    def read(self,n):
        if n not in self.records:raise FileNotFoundError(n)
        return copy.deepcopy(self.records[n])
    def arm(self,intent):
        if self.guard_bad:raise Refusal('inert guard failure')
        self.armed=True;self.record('guard-ready.json',{'deadline_mono':intent['deadline_mono']});return self.records['guard-ready.json']
    def trigger(self):
        if not self.exists('stop-trigger.json'):self.record('stop-trigger.json',{})
    def boat(self,args,*a,**k):
        self.calls.append(list(args))
        if args[0]=='resume':
            assert self.armed and self.exists('resume-intent.json')
            if self.mode=='no_ready':return {'exit':1,'error':None,'body':{'ok':False,'code':'no_ready_machine','error':{'code':'no_ready_machine'}}}
            self.started=True
            if self.mode=='timeout':return {'exit':None,'error':'timeout','body':None}
            if self.mode=='malformed':return {'exit':0,'error':None,'body':None}
            return {'exit':0,'error':None,'body':json.loads(cli_cases()['results'][0]['stdout'])}
        if args[0]=='stop':
            self.stopping=True
            if self.mode=='stop_timeout':return {'exit':None,'error':'timeout','body':None}
            return {'exit':0,'error':None,'body':{'id':worker_id(),'operation':{'id':'stop_new','status':'pending'}}}
        raise AssertionError('unexpected call')
    def info(self):
        result=info(self.r,self.started)
        if self.env_bad and self.started:result['environmentVersion']=9
        if self.expiry_bad and self.started:result['ttlSeconds']=None
        if self.stopping:
            result.update(state='stopped',stoppedAt=iso(self.wall()),snapshotCompletedAt=iso(self.wall()),snapshotAvailable=not self.stopped_bad)
            result['stop']={'id':'stop_new','status':'completed','error':None,'forced':False,'endedAt':iso(self.wall())}
        return result
    def environment_after(self,intent):
        if getattr(self,'environment_after_bad',False):raise Refusal('post ENV drift')
        self.record('environment-after.json',{'latestVersionNumber':profile()['future_environment']['version'],'hook_sha256':hook_sha()})
    def stop_status(self,operation):
        return {'id':operation,'status':'completed','requestedAt':iso(self.wall()),'endedAt':iso(self.wall()),'forced':False,'error_present':False}
    def limits(self):
        self.record('account-starts-before-resume.json',{'synthetic_account_fixture':True})
        return {}
    def startup(self,intent,allow_qualification):
        if getattr(self,'startup_bad',False):return False
        if not self.exists('startup-closed.json'):self.record('startup-closed.json',{'startup_closed':True,'qualification_eligible':True,'bootstrap_occurrence':'unknown'})
        return True
    def stop_quiescent(self):
        if getattr(self,'startup_bad',False):return False
        if not self.exists('stop-startup-closed.json'):self.record('stop-startup-closed.json',{'startup_closed':True})
        if not self.exists('startup-closed.json'):self.record('startup-closed.json',{'startup_closed':True})
        return True
    def guard_for_admission(self):
        loaded={'Id':self.r['guard_unit']+'.service','ActiveState':'active','MainPID':'123','InvocationID':'a'*32}
        row={'pid':123,'uid':os.getuid(),'observed_wall':self.wall(),'observed_boottime':self.mono(),'boot_id':'12345678-1234-1234-1234-123456789012','loaded':loaded}
        self.record('guard-admission.json',row);return row
    def qualification(self,intent):
        self.record('metadata-started.json',{})
        if self.closure:self.record('qualification-closed.json',{'metadata_closed':True})
        if self.qual_bad:raise Refusal('inert wrong caps/source')
        self.record('qualification.json',{'qualified':True,'metadata_closed':True,'startup_closed':True,'engine_or_job_started':False,'actual_toolchain_manifest':{'path':profile()['worker_home']+'/.local/state/sparkles-boat/toolchain.json','sha256':manifest_sha()},'bootstrap_lock':{'path':profile()['worker_home']+'/.local/state/sparkles-boat/bootstrap.lock','held_during_checkpoint':True},'runtime_env_file':None,'worker_boot_id':'87654321-4321-4321-4321-210987654321'});return {}
    def metadata_closed(self):
        if self.closure:self.record('qualification-closed.json',{'metadata_closed':True});return True
        return False

class DiskIO(InertIO):
    record=c.RealIO.record
    exists=c.RealIO.exists
    read=c.RealIO.read
    def __init__(self,r,root):super().__init__(r);self.op=root
    def arm(self,intent):
        if self.guard_bad:raise Refusal('inert guard failure')
        self.armed=True;self.record('guard-ready.json',{'deadline_mono':intent['deadline_mono']});return self.read('guard-ready.json')

class PhaseIO(InertIO):
    startup=c.RealIO.startup
    _startup=c.RealIO._startup
    startup_refused=c.RealIO.startup_refused
    def __init__(self,r):super().__init__(r);self.phase_calls=[];self.pending=False;self.bad_manifest=False;self.delay=0
    def phase(self,phase,seconds=0):
        self.phase_calls.append((phase,seconds))
        if self.delay:self.wait(self.delay)
        if getattr(self,'refusal_body',None) is not None:return copy.deepcopy(self.refusal_body)
        return {'startup_closed':not self.pending,'phase_closed':not self.pending,'qualification_eligible':not self.bad_manifest,'bootstrap_occurrence':'unknown'}

class Clock:
    def __init__(self):self.t=0
    def now(self):return self.t
    def pause(self,n):self.t+=n

def proc_fixture():
    return {1:{'pid':1,'ppid':0,'uid':0,'comm':'systemd','start':1,'group':1},10:{'pid':10,'ppid':1,'uid':0,'comm':'sshd','start':1,'group':10},11:{'pid':11,'ppid':10,'uid':1000,'comm':'python3','start':1,'group':11},12:{'pid':12,'ppid':1,'uid':1000,'comm':'python3','start':1,'group':12},13:{'pid':13,'ppid':11,'uid':1000,'comm':'systemd-run','start':1,'group':11}}

# The 39 uid-1000 rows that `ps -u 1000 -o pid,ppid,lstart,etimes,comm` listed on
# the admitted large worker bx_gy2ytgh8 (environment v8) right after its first
# SSH connection, with pid 1 added as root's init. The startup observer counted
# 35 busy processes there, these rows less the seven infrastructure rows plus a
# bash, a sudo and a fusermount3 whose parents the listing did not show. Those
# three hang from pid 9999 here, which is absent like pid 8001, the unlisted
# parent of dbus-run-sessio.
OBSERVED_SESSION="""976 1 pulseaudio
1220 1 systemd
1244 1220 (sd-pam)
1264 1220 pipewire
1265 1220 pipewire
1266 1220 wireplumber
1267 1220 pipewire-pulse
1276 1220 dbus-daemon
1759 1220 gvfsd
1781 1220 gvfsd-fuse
1830 1220 at-spi-bus-laun
1837 1830 dbus-daemon
1842 1220 xdg-desktop-por
1882 1220 xdg-document-po
1903 1220 xdg-permission-
2132 1220 cua-driver
2217 1220 dconf-service
4541 1 ascii-agent-sup
4543 4541 ascii-agents-se
8008 8001 dbus-run-sessio
8095 1 ibus-daemon
8096 8008 dbus-daemon
8097 8008 ascii-desktop
8101 8097 openbox
8103 8095 ibus-memconf
8104 8095 ibus-ui-gtk3
8105 8095 ibus-extension-
8107 1 ibus-x11
8111 1220 ibus-portal
8136 1220 at-spi2-registr
8149 8095 ibus-engine-sim
8172 1 sunshine
8193 1220 gnome-keyring-d
8194 1220 gnome-keyring-d
8273 1 dbus-launch
8274 1 dbus-daemon
8276 1 at-spi-bus-laun
8282 8276 dbus-daemon
8294 1 at-spi2-registr"""
UNPARENTED={20001:('bash',1000),20002:('sudo',0),20003:('fusermount3',1000)}
def observed_session_rows(unparented=True):
    rows=proc_fixture()
    for line in OBSERVED_SESSION.splitlines():
        pid,ppid,comm=line.split(' ',2);rows[int(pid)]={'pid':int(pid),'ppid':int(ppid),'uid':1000,'comm':comm,'start':5,'group':int(pid)}
    if unparented:
        for pid,(comm,uid) in UNPARENTED.items():rows[pid]={'pid':pid,'ppid':9999,'uid':uid,'comm':comm,'start':5,'group':pid}
    return rows
UNKNOWN_SESSION={8008,8096,8097,8101}

def proc_fixture_reader():
    return lambda:proc_fixture()

class LoadedGuardIO(InertIO):
    # Exercise the actual RealIO.arm gate with entirely inert systemd methods.
    arm=c.RealIO.arm
    def __init__(self,r):super().__init__(r);self.launched=False;self.loaded_override={};self.exec_wrong=False
    def system(self,argv,seconds=15):
        self.launched=True
        self.record('guard-ready.json',{'pid':123,'deadline_mono':10600,'memory_max':'536870912','swap_max':'0','cgroup':'/sys/fs/cgroup/inert/boat-owned-warm-qual-inert.service'})
        return ''
    def show(self,unit):
        if not self.launched:return {'LoadState':'not-found','ActiveState':'inactive','MainPID':'0','InvocationID':''}
        expected=[str(Path(sys.executable).resolve()),'-I','-B',str(c.ROOT/'controller.py'),'guard','--operation','/tmp/boat-inert-qualification']
        if self.exec_wrong:expected[-1]='/tmp/other-operation'
        row={'LoadState':'loaded','ActiveState':'active','InvocationID':'a'*32,'ControlGroup':'/inert/boat-owned-warm-qual-inert.service','MainPID':'123','MemoryMax':'536870912','MemorySwapMax':'0','KillMode':'control-group','RuntimeMaxUSec':'12h 5min','OOMPolicy':'continue','ExecStart':'{ path='+expected[0]+' ; argv[]='+' '.join(expected)+' ; ignore_errors=no ; }'}
        row.update(self.loaded_override);return row
    @property
    def op(self):return Path('/tmp/boat-inert-qualification')


class Controls(unittest.TestCase):
    def setUp(self):fixture()
    def test_false_template_has_no_adapter_actions(self):
        r=json.loads(Path(__file__).with_name('release.FALSE.json').read_text());io=InertIO(r)
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertEqual(io.calls,[]);self.assertEqual(io.records,{})
    def test_mandatory_release_negative_fields(self):
        cases=[('root_exclusive_worker_use',False),('private_credential_snapshot_acknowledged',False),('clean_next_snapshot_required',True),('no_source_transport_or_jobs',False),('different_controller_review_sha256',None),('worker','bx_other')]
        for key,v in cases:
            with self.subTest(key=key):
                r=fixture();r[key]=v;io=InertIO(r)
                with self.assertRaises(Refusal):c.drive(r,io)
                self.assertEqual(io.calls,[])
    def test_nested_fact_refusals(self):
        cases=[('environment','version',9),('environment','hook_sha256','0'*64),('environment','assigned_editors_excluded',False),('snapshot_continuity','continuity_is_retained_evidence_inference',False),('snapshot_continuity','manifest_sha256','0'*64),('snapshot_continuity','hook_token_metadata_open_fchmod_accepted',False),('snapshot_continuity','dev_read_only_scope',False),('previous_native_closure','all_native_and_leases_closed',False),('start_ledger','starts_and_reservations',201),('start_ledger','all_actors_included',False),('start_ledger','this_one_resume_reserved',False),('local_first','overflow_currently_needed',False),('provider_contract','readiness_predicate',None),('provider_contract','ttl_basis',None)]
        for section,key,value in cases:
            with self.subTest(section=section,key=key):
                r=fixture();r[section][key]=value;io=InertIO(r)
                with self.assertRaises(Refusal):c.drive(r,io)
                self.assertEqual(io.calls,[])
    def test_qualification_without_current_overflow_needs_a_bound_maintainer_confirmation(self):
        for changed in ({'purpose':'qualification'},{'purpose':'other','maintainer_confirmation_sha256':'9'*64},{'maintainer_confirmation_sha256':'9'*64},{'purpose':'qualification','maintainer_confirmation_sha256':'short'}):
            with self.subTest(changed=changed):
                r=fixture();r['local_first'].update(overflow_currently_needed=False,**changed);io=InertIO(r)
                with self.assertRaises(Refusal):c.drive(r,io)
                self.assertEqual(io.calls,[])
        r=fixture();r['local_first'].update(overflow_currently_needed=False,purpose='qualification',maintainer_confirmation_sha256='9'*64)
        self.assertIs(c.validate_release(r,1800000000),r)
    def test_stale_fact_refused(self):
        r=fixture();r['environment']['observed_unix']-=301
        with self.assertRaises(Refusal):c.drive(r,InertIO(r))
    def test_no_resume_if_guard_arm_fails(self):
        r=fixture();io=InertIO(r);io.guard_bad=True
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertEqual(io.calls,[]);self.assertTrue(io.exists('cancel-before-resume.json'))
    def test_one_exact_resume_and_default_stop_trigger(self):
        r=fixture();io=InertIO(r);self.assertEqual(c.drive(r,io),'QUALIFIED_STOP_REQUESTED')
        self.assertEqual(io.calls,[['resume',worker_id(),'--environment',profile()['future_environment']['name'],'--type','large','--ttl','10800','--fail-fast','--no-wait']]);self.assertTrue(io.exists('stop-trigger.json'))
        self.assertTrue(c.guard_loop(r,io,io.read('controller-intent.json')));self.assertTrue(io.exists('closed.json'))
    def test_no_ready_proof_no_stop_request(self):
        r=fixture();io=InertIO(r);io.mode='no_ready';self.assertEqual(c.drive(r,io),'NO_READY_CONFIRMED')
        self.assertTrue(c.guard_loop(r,io,io.read('controller-intent.json')))
        self.assertEqual([a[0] for a in io.calls],['resume']);self.assertTrue(io.exists('closed-no-start.json'))
    def test_no_ready_requires_structured_error(self):
        for body in [None,{'code':'no_ready_machine'},{'ok':False,'code':'no_ready_machine','error':{'code':'other'}}]:
            self.assertFalse(c.no_ready({'exit':1,'body':body}))
    def test_uncertain_and_malformed_exit0_never_qualify_or_retry(self):
        for mode in ['timeout','malformed']:
            with self.subTest(mode=mode):
                r=fixture();io=InertIO(r);io.mode=mode
                with self.assertRaises(Refusal):c.drive(r,io)
                self.assertEqual(sum(a[0]=='resume' for a in io.calls),1);self.assertTrue(c.guard_loop(r,io,io.read('controller-intent.json')))
    def test_actual_ENV_drift_and_missing_TTL_refuse(self):
        for key in ['env_bad','expiry_bad']:
            r=fixture();io=InertIO(r);setattr(io,key,True)
            with self.assertRaises(Refusal):c.drive(r,io)
            self.assertEqual(sum(a[0]=='resume' for a in io.calls),1);self.assertTrue(io.exists('stop-trigger.json'))
    def test_phase_exhaustion_refuses(self):
        with self.assertRaises(Refusal):budget(100,400,330)
        budget(100,700,330)
    def test_disconnect_leaves_guard_deadline_active(self):
        r=fixture();io=InertIO(r);io.started=True;intent={'deadline_mono':103,'deadline_wall':io.wall()+3}
        io.record('resume-intent.json',{});io.record('controller-intent.json',intent);self.assertTrue(c.guard_loop(r,io,intent));self.assertEqual([x[0] for x in io.calls],['stop'])
    def test_wall_deadline_earlier_than_mono(self):
        r=fixture();io=InertIO(r);io.started=True;io.record('resume-intent.json',{})
        intent={'deadline_mono':10000,'deadline_wall':io.wall()};io.record('controller-intent.json',intent);self.assertTrue(c.guard_loop(r,io,intent));self.assertLess(io.mono(),200)
    def test_guard_no_resume_intent_never_stops_provider(self):
        r=fixture();io=InertIO(r);self.assertTrue(c.guard_loop(r,io,{'deadline_mono':io.mono(),'deadline_wall':io.wall()}));self.assertEqual(io.calls,[])
    def test_wrong_caps_or_source_causes_failure_not_qualification(self):
        r=fixture();io=InertIO(r);io.qual_bad=True
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertFalse(io.exists('qualification.json'));self.assertTrue(io.exists('stop-trigger.json'));self.assertTrue(c.guard_loop(r,io,io.read('controller-intent.json')))
    def test_unknown_owned_metadata_closure_does_not_fake_stop(self):
        r=fixture();io=InertIO(r);io.record('metadata-started.json',{});io.closure=False
        self.assertFalse(c.reconcile_stop(r,io));self.assertTrue(io.exists('unresolved.json'));self.assertEqual(io.calls,[])
        self.assertTrue(io.read('unresolved.json')['reason_code'].startswith('refusal:'))
    def test_snapshot_failure_never_shutdown_success_or_retry(self):
        r=fixture();io=InertIO(r);io.started=True;io.stopped_bad=True
        self.assertFalse(c.reconcile_stop(r,io));self.assertFalse(io.exists('closed.json'));self.assertTrue(io.exists('unresolved.json'));self.assertEqual([x[0] for x in io.calls],['stop'])
        self.assertTrue(io.read('unresolved.json')['reason_code'].startswith('refusal:'))
    def test_stop_timeout_can_reconcile_fresh_operation(self):
        r=fixture();io=InertIO(r);io.started=True;io.mode='stop_timeout'
        self.assertTrue(c.reconcile_stop(r,io));self.assertEqual([x[0] for x in io.calls],['stop'])
    def test_old_completion_not_fresh_stop(self):
        r=fixture();old=info(r)
        with self.assertRaises(Refusal):stop_complete(old,'stop_old',1800000000)
    def test_stop_forced_or_wrong_operation_refused(self):
        r=fixture();s=info(r);s['stoppedAt']=iso(1800000000);s['stop'].update(endedAt=iso(1800000000),forced=True)
        with self.assertRaises(Refusal):stop_complete(s,'stop_old',1800000000)
        s['stop']['forced']=False
        with self.assertRaises(Refusal):stop_complete(s,'stop_other',1800000000)
    def test_safe_info_does_not_emit_secret_or_arbitrary_error(self):
        row=info(fixture());row['secretFiles']={'x':'DO_NOT_COPY'};row['envContents']='DO_NOT_COPY';row['setupError']='DO_NOT_COPY'
        projected=safe_info({'sandbox':row});self.assertNotIn('DO_NOT_COPY',json.dumps(projected))
    def test_unknown_info_envelope_refuses(self):
        with self.assertRaises(Refusal):safe_info({'worker':{}})
    def test_live_Root_handoff_lease_refuses_before_stop(self):
        r=fixture();io=InertIO(r);io.record('root-handoff.json',{'separate_job_release':{'job_release_sha256':'1'*64}})
        self.assertFalse(c.reconcile_stop(r,io));self.assertEqual(io.calls,[])
    def test_unreviewed_useful_job_handoff_refused(self):
        r=fixture();r['useful_job_ready']=True;r['separate_Root_handoff']={};io=InertIO(r)
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertFalse(io.exists('root-handoff.json'));self.assertTrue(io.exists('stop-trigger.json'))
    def test_reviewed_handoff_runs_no_job_and_guard_stays(self):
        r=fixture();r['useful_job_ready']=True;r['separate_Root_handoff']={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':600};io=InertIO(r)
        self.assertEqual(c.drive(r,io),'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE');self.assertFalse(io.exists('stop-trigger.json'));self.assertEqual([x[0] for x in io.calls],['resume'])
    def test_systemd_finite_clock_parser(self):
        self.assertEqual(systemd_seconds('2h 58min 20s'),10700);self.assertEqual(systemd_seconds('4min'),240)
        for t in ['infinity','','unknown','10700']:
            with self.assertRaises(Refusal):systemd_seconds(t)
    def test_token_lstat_only_and_alias_denial(self):
        with tempfile.TemporaryDirectory(prefix='boat-qual-ascii-') as d:
            token=Path(d)/'token';token.write_text('INERT_ASCII_NOT_A_CREDENTIAL');token.chmod(0o600)
            with patch.object(os,'open',side_effect=AssertionError('no token open')):
                tm=p.token_metadata(token,os.getuid())
            alias=Path(d)/'alias';os.link(token,alias)
            with patch.object(os,'open',side_effect=AssertionError('no alias open')):
                with self.assertRaises(p.Refusal):p.safe_file(alias,os.getuid(),(tm['device'],tm['inode']),1024)
    def test_public_metadata_file_and_mutation_modes(self):
        with tempfile.TemporaryDirectory(prefix='boat-qual-public-') as d:
            f=Path(d)/'config';f.write_text('ASCII');f.chmod(0o600)
            row,data=p.safe_file(f,os.getuid(),(-1,-1),1024);self.assertEqual(data,b'ASCII');self.assertEqual(row['bytes'],5)
            f.chmod(0o666)
            with self.assertRaises(p.Refusal):p.safe_file(f,os.getuid(),(-1,-1),1024)
    def test_local_receipts_reject_symlink_and_hardlink(self):
        with tempfile.TemporaryDirectory(prefix='boat-qual-receipt-') as d:
            root=Path(d);f=root/'receipt';f.write_text('{}');f.chmod(0o600);os.symlink(f,root/'sym')
            with self.assertRaises(Refusal):read_owned(root/'sym')
            os.link(f,root/'hard')
            with self.assertRaises(Refusal):read_owned(f)
    def test_all_python_source_parses_without_execution(self):
        for f in Path(__file__).parent.glob('*.py'):ast.parse(f.read_text(),filename=str(f))
    def test_actual_loaded_guard_gate(self):
        io=LoadedGuardIO(fixture());self.assertEqual(io.arm({'deadline_mono':10600})['pid'],123);self.assertTrue(io.exists('guard-loaded.json'))
    def test_actual_loaded_guard_negative_limits_pid_clock_command(self):
        for field,value in [('MemoryMax','1073741824'),('MemorySwapMax','1'),('MainPID','124'),('ActiveState','failed'),('RuntimeMaxUSec','infinity'),('KillMode','process'),('ControlGroup','/different'),('InvocationID','')]:
            with self.subTest(field=field):
                io=LoadedGuardIO(fixture());io.loaded_override[field]=value
                with self.assertRaises(Refusal):io.arm({'deadline_mono':10600})
        io=LoadedGuardIO(fixture());io.exec_wrong=True
        with self.assertRaises(Refusal):io.arm({'deadline_mono':10600})
    def test_actual_metadata_runtime_gate_and_negatives(self):
        unit='boat-owned-warm-qual-inert-metadata.service';line='0::/test/'+unit
        row={'MemoryMax':'536870912','MemorySwapMax':'0','KillMode':'control-group','OOMPolicy':'continue','MainPID':'123','RuntimeMaxUSec':'4min','TimeoutStopUSec':'10s','ControlGroup':line[3:],'InvocationID':'a'*32}
        p.validate_runtime(unit,123,line,'536870912','0',row)
        for key,value in [('MemoryMax','1073741824'),('MemorySwapMax','1'),('RuntimeMaxUSec','infinity'),('TimeoutStopUSec','90s'),('ControlGroup','/other'),('MainPID','124'),('InvocationID','')]:
            with self.subTest(key=key):
                bad=dict(row);bad[key]=value
                with self.assertRaises(p.Refusal):p.validate_runtime(unit,123,line,'536870912','0',bad)
        with self.assertRaises(p.Refusal):p.validate_runtime(unit,123,line,'1073741824','0',row)
    def test_actual_metadata_pressure_gate(self):
        p.validate_pressure({'high':'0','max':'0','oom':'0'},'0')
        with self.assertRaises(p.Refusal):p.validate_pressure({'max':'1'},'0')
        with self.assertRaises(p.Refusal):p.validate_pressure({'max':'0'},'1')
    def test_null_setup_and_null_archive_are_not_invented_success(self):
        r=fixture();x=info(r,True);x['setupStatus']=None;x['archiveAfter']=None
        self.assertEqual(resumed(x,r,1800000000),1800010500)
    def test_post_resume_ENV_drift_refuses_and_stops(self):
        r=fixture();io=InertIO(r);io.environment_after_bad=True
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertFalse(io.exists('qualification.json'));self.assertTrue(io.exists('stop-trigger.json'))
    def test_old_checkonly_mode_held_and_named_normal_mode_authorized(self):
        fixture();pins=profile()
        with self.assertRaises(Refusal):c.startup_scope_gate(pins,'check_only_warm_prebootstrap')
        mode=c.startup_scope_gate(pins,'normal_pinned_provisioning');self.assertEqual(mode['bootstrap_occurrence'],'unknown')

    def test_documented_ready_states_and_explicit_setup_failure(self):
        r=fixture()
        for state in ['ready','idle','running']:
            x=info(r,True);x['state']=state;x['setupStatus']=None;x['archiveAfter']=None
            self.assertEqual(resumed(x,r,1800000000),1800010500)
        x['setup_error_present']=True
        with self.assertRaises(Refusal):resumed(x,r,1800000000)
    def test_no_ready_may_repin_ENV_without_start(self):
        r=fixture();io=InertIO(r);io.mode='no_ready'
        original=io.info
        def observation():
            row=original()
            if io.calls:row['environmentVersion']=profile()['future_environment']['version']
            return row
        io.info=observation
        self.assertEqual(c.drive(r,io),'NO_READY_CONFIRMED');self.assertTrue(c.guard_loop(r,io,io.read('controller-intent.json')))
        self.assertEqual([x[0]for x in io.calls],['resume'])
    def test_real_writer_handoff_and_export_receipts(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-real-handoff-') as d:
            r=fixture();r['operation']=d;r['useful_job_ready']=True;r['separate_Root_handoff']={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':600}
            io=DiskIO(r,Path(d));c.write_new(Path(d),'release.json',r);self.assertEqual(c.drive(r,io),'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE')
            admission=load(Path(d)/'qualified-worker-admission.json');handoff=load(Path(d)/'root-handoff.json')
            from qualified_worker_admission import export_operation
            self.assertEqual(export_operation(Path(d),io.wall(),worker_id(),profile()),admission)
            self.assertEqual(admission['worker'],worker_id());self.assertEqual(admission['local_guard']['unit'],r['guard_unit']+'.service');self.assertIsNone(admission['runtime_env_file'])
            self.assertFalse(io.exists('stop-trigger.json'));self.assertFalse(handoff['job_started_by_controller']);self.assertEqual([x[0]for x in io.calls],['resume'])
            closed={'observed_unix':io.wall(),'verified_by_Root':True,'receipt_sha256':'c'*64,'worker':worker_id(),'operation_id':r['guard_unit'],'local_guard':admission['local_guard'],'job_release_sha256':'a'*64,'all_native_and_leases_closed':True}
            c.write_new(Path(d),'root-job-closed.json',closed);self.assertTrue(c.matching_root_job_closed(r,io,load(Path(d)/'root-job-closed.json')))
            closed['local_guard']=dict(closed['local_guard'],invocation_id='d'*32)
            with self.assertRaises(Refusal):c.matching_root_job_closed(r,io,closed)
    def test_real_writer_unresolved_native_and_snapshot_paths(self):
        for kind in ['native','snapshot']:
            with self.subTest(kind=kind),tempfile.TemporaryDirectory(prefix='boat-v2-real-unresolved-') as d:
                r=fixture();io=DiskIO(r,Path(d));io.started=True
                if kind=='native':io.record('metadata-started.json',{});io.closure=False
                else:io.stopped_bad=True
                self.assertFalse(c.reconcile_stop(r,io));self.assertTrue(load(Path(d)/'unresolved.json')['ownership_retained_by_Root']);self.assertFalse(io.exists('closed.json'))
                self.assertLessEqual(sum(x[0]=='stop'for x in io.calls),1)
    def test_real_atomic_writer_no_overwrite_or_partial_final(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-atomic-') as d:
            root=Path(d);seen=[];original=os.fsync
            def observe(fd):
                if (root/'.pending-ready.json').exists() and not(root/'ready.json').exists():seen.append(True)
                original(fd)
            with patch.object(os,'fsync',side_effect=observe):c.write_new(root,'ready.json',{'ready':True})
            self.assertTrue(seen);self.assertTrue(load(root/'ready.json')['ready']);self.assertEqual((root/'ready.json').stat().st_nlink,1);self.assertFalse((root/'.pending-ready.json').exists())
            with self.assertRaises(FileExistsError):c.write_new(root,'ready.json',{'ready':False})
            self.assertTrue(load(root/'ready.json')['ready']);self.assertTrue((root/'.pending-ready.json').exists())
    def test_real_reader_partial_receipt_refuses(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-partial-') as d:
            root=Path(d);f=root/'unresolved.json';f.write_text('{');f.chmod(0o600)
            with self.assertRaises(ValueError):load(f)
            with self.assertRaises(FileExistsError):c.write_new(root,'unresolved.json',{'fake':True})
            self.assertEqual(f.read_text(),'{')
    def test_every_constant_receipt_name_accepted_by_real_writer(self):
        tree=ast.parse(Path(__file__).with_name('controller.py').read_text());names=set()
        for call in ast.walk(tree):
            if isinstance(call,ast.Call)and isinstance(call.func,ast.Attribute)and call.func.attr=='record'and call.args and isinstance(call.args[0],ast.Constant):names.add(call.args[0].value)
        with tempfile.TemporaryDirectory(prefix='boat-v2-all-names-') as d:
            for name in names:c.write_new(Path(d),name,{'inert':True});self.assertTrue(load(Path(d)/name)['inert'])
    def test_CLI_ack_contract_synthetic_cases(self):
        facts=cli_cases();contract=fixture()['provider_contract']['resume_ack']
        for index,row in enumerate(facts['results']):
            actual={'exit':row['exit'],'error':None,'body':json.loads(row['stdout'])}
            self.assertEqual(positive_resume_ack(actual,contract),index==0)
        good=json.loads(facts['results'][0]['stdout'])
        for body in [None,{},dict(good,id='bx_other'),dict(good,status='ready'),dict(good,event='ready')]:self.assertFalse(positive_resume_ack({'exit':0,'error':None,'body':body},contract))
        self.assertFalse(positive_resume_ack({'exit':1,'error':None,'body':good},contract))
    def test_null_status_contract_synthetic_cases_never_qualify(self):
        rows=cli_cases()['results']
        for row in rows[1:]:
            r=fixture();io=InertIO(r);original=io.boat
            def actual_counter(args,*a,**k):
                out=original(args,*a,**k)
                return dict(out,body=json.loads(row['stdout']))if args[0]=='resume'else out
            io.boat=actual_counter
            with self.assertRaises(Refusal):c.drive(r,io)
            self.assertFalse(io.exists('qualification.json'));self.assertTrue(io.read('resume-outcome.json')['uncertain_acceptance']);self.assertEqual(sum(x[0]=='resume'for x in io.calls),1)
    def test_actual_bootstrap_lock_held_and_busy(self):
        lock=OBSERVER['bootstrap_lock']
        with tempfile.TemporaryDirectory(prefix='boat-v2-bootstrap-lock-') as d:
            f=Path(d)/'bootstrap.lock';f.touch(mode=0o600)
            with lock(f,os.getuid(),(-1,-1)) as proof:
                self.assertTrue(proof['held_during_checkpoint'])
                with self.assertRaises(BlockingIOError):
                    with lock(f,os.getuid(),(-1,-1)):pass
            with lock(f,os.getuid(),(-1,-1)):pass
    def test_bootstrap_lock_alias_mode_and_replace_refuse(self):
        lock=OBSERVER['bootstrap_lock']
        with tempfile.TemporaryDirectory(prefix='boat-v2-lock-negative-') as d:
            f=Path(d)/'bootstrap.lock';f.touch(mode=0o600);s=f.stat()
            with self.assertRaises(OBSERVER['Refusal']):
                with lock(f,os.getuid(),(s.st_dev,s.st_ino)):pass
            f.chmod(0o666)
            with self.assertRaises(OBSERVER['Refusal']):
                with lock(f,os.getuid(),(-1,-1)):pass
    def test_actual_startup_checkpoint_and_manifest_change(self):
        checkpoint=OBSERVER['checkpoint_at'];rows=proc_fixture()
        with tempfile.TemporaryDirectory(prefix='boat-v2-startup-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII_MANIFEST');m.chmod(0o600);clock=Clock();expected=digest(m.read_bytes())
            q=checkpoint(root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,expected,2,lambda:rows,clock.now,clock.pause)
            self.assertTrue(q['startup_closed']);self.assertTrue(q['qualification_eligible']);self.assertEqual(q['bootstrap_occurrence'],'unknown');self.assertFalse(q['hook_terminal_observed'])
            q=checkpoint(root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,'0'*64,2,lambda:rows,clock.now,clock.pause)
            self.assertTrue(q['startup_closed']);self.assertFalse(q['qualification_eligible'])
            m.unlink();q=checkpoint(root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,expected,2,lambda:rows,clock.now,clock.pause);self.assertFalse(q['startup_closed'])
    def test_startup_descendants_remain_tracked_after_parent_exit(self):
        rows=proc_fixture();rows[20]={'pid':20,'ppid':1,'uid':1000,'comm':'bash','start':1,'group':20};rows[21]={'pid':21,'ppid':20,'uid':0,'comm':'apt-get','start':1,'group':20};rows[22]={'pid':22,'ppid':21,'uid':0,'comm':'maintainer','start':1,'group':20};tracked=set()
        scan=OBSERVER['foreign_startup'];self.assertEqual({x['pid']for x in scan(rows,{'pid':11,'start':1},12,tracked)},{20,21,22})
        del rows[20];rows[21]['comm']='renamed';self.assertEqual({x['pid']for x in scan(rows,{'pid':11,'start':1},12,tracked)},{21,22})
        del rows[21];del rows[22];self.assertEqual(scan(rows,{'pid':11,'start':1},12,tracked),[])
    def test_observed_listing_counts_35_busy(self):
        # The diagnosis's own count on bx_gy2ytgh8, from the old rule alone.
        rows=observed_session_rows();infra={'systemd','(sd-pam)','dbus-daemon'}
        old=[r for p,r in rows.items() if p not in (1,10,11,12,13) and ((r['uid']==1000 and r['comm'] not in infra) or r['comm'] in OBSERVER['INSTALLER'])]
        self.assertEqual(len(old),35)
    def test_observed_provider_session_admits_checkpoint(self):
        rows=observed_session_rows(unparented=False)
        for pid in UNKNOWN_SESSION:del rows[pid]
        with tempfile.TemporaryDirectory(prefix='boat-v2-session-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600);clock=Clock()
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,lambda:rows,clock.now,clock.pause,lambda rows,left:(1220,5))
            self.assertTrue(q['startup_closed']);self.assertTrue(q['qualification_eligible'])
            # Without the system manager's own user-manager identity none of its children is allowed.
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,lambda:rows,clock.now,clock.pause)
            self.assertFalse(q['startup_closed']);self.assertEqual(len(q['unallowed']),17)
    def test_observed_listing_unknowns_refuse_with_bounded_inventory(self):
        rows=observed_session_rows()
        with tempfile.TemporaryDirectory(prefix='boat-v2-session-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600);clock=Clock()
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,lambda:rows,clock.now,clock.pause,lambda rows,left:(1220,5))
        self.assertFalse(q['startup_closed']);self.assertEqual(q['reason_code'],'startup_busy:unallowed_processes')
        self.assertEqual({x['pid'] for x in q['unallowed']},UNKNOWN_SESSION|set(UNPARENTED));self.assertEqual(q['unallowed_count'],7)
        row={x['pid']:x for x in q['unallowed']}[8008]
        self.assertEqual(row,{'pid':8008,'comm':'dbus-run-sessio','uid':1000,'ppid':8001,'parent_comm':None,'parent_uid':None})
        self.assertEqual({x['pid']:x['parent_comm'] for x in q['unallowed']}[8097],'dbus-run-sessio')
    def test_provider_session_allowance_binds_parent_and_uid(self):
        scan=OBSERVER['foreign_startup']
        def busy(extra):
            rows=observed_session_rows(unparented=False)
            for pid in UNKNOWN_SESSION:del rows[pid]
            rows.update(extra);return {x['pid'] for x in scan(rows,{'pid':11,'start':1},12,set(),manager)}
        manager=(1220,5)
        self.assertEqual(busy({}),set())
        # An allowed name under another parent, a parent of the wrong uid, a user
        # manager that is not init's child and an unknown name all stay busy.
        self.assertEqual(busy({30:{'pid':30,'ppid':1220,'uid':1000,'comm':'pulseaudio','start':5,'group':30}}),{30})
        self.assertEqual(busy({30:{'pid':30,'ppid':8095,'uid':1000,'comm':'pipewire','start':5,'group':30}}),{30})
        # Another uid's process was never startup work unless it is an installer.
        self.assertEqual(busy({30:{'pid':30,'ppid':1,'uid':1001,'comm':'pulseaudio','start':5,'group':30}}),set())
        self.assertEqual(busy({30:{'pid':30,'ppid':1,'uid':0,'comm':'pipewire','start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'pipewire','start':5,'group':30}}),{31})
        # A second process named systemd is not the manager the system manager reports.
        self.assertEqual(busy({30:{'pid':30,'ppid':1,'uid':1000,'comm':'systemd','start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'gvfsd','start':5,'group':30}}),{31})
        # Names never stand in for the anchors.
        for name in ['init','user-manager','systemd']:
            self.assertEqual(busy({30:{'pid':30,'ppid':9999,'uid':0,'comm':name,'start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'pulseaudio','start':5,'group':30}}),{31})
            self.assertEqual(busy({30:{'pid':30,'ppid':9999,'uid':1000,'comm':name,'start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'pipewire','start':5,'group':30}}),{30,31} if name!='systemd' else {31})
        # A reused manager pid with another start time is not the manager.
        manager=(1220,6);self.assertEqual(len(busy({})),17);manager=(1220,5)
        self.assertEqual(busy({30:{'pid':30,'ppid':4543,'uid':1000,'comm':'systemd','start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'gvfsd','start':5,'group':30}}),{31})
        self.assertEqual(busy({30:{'pid':30,'ppid':1,'uid':1000,'comm':'ibus-daemonx','start':5,'group':30}}),{30})
        self.assertEqual(busy({30:{'pid':30,'ppid':1,'uid':0,'comm':'sudo','start':5,'group':30}}),{30})
        # An allowed child of an allowed process whose own parent is unknown stays busy.
        self.assertEqual(busy({30:{'pid':30,'ppid':9999,'uid':1000,'comm':'ibus-daemon','start':5,'group':30},31:{'pid':31,'ppid':30,'uid':1000,'comm':'ibus-memconf','start':5,'group':30}}),{30,31})
    def test_user_manager_comes_from_the_system_manager(self):
        rows=observed_session_rows();um=OBSERVER['user_manager']
        with patch.dict(OBSERVER,{'command':lambda argv,seconds=15:b'MainPID=1220\n'}):self.assertEqual(um(rows,1000),(1220,5))
        for answer in [b'MainPID=0\n',b'MainPID=4541\n',b'MainPID=1220 x\n',b'']:
            with patch.dict(OBSERVER,{'command':lambda argv,seconds=15,a=answer:a}):self.assertIsNone(um(rows,1000))
        def fail(argv,seconds=15):raise OBSERVER['Refusal']('qualification command failed')
        with patch.dict(OBSERVER,{'command':fail}):self.assertIsNone(um(rows,1000))
    def test_allowed_name_started_by_installer_stays_tracked(self):
        rows=observed_session_rows(unparented=False);tracked=set()
        for pid in UNKNOWN_SESSION:del rows[pid]
        rows[40]={'pid':40,'ppid':1,'uid':1000,'comm':'bash','start':5,'group':40};rows[41]={'pid':41,'ppid':40,'uid':1000,'comm':'pulseaudio','start':5,'group':40}
        scan=OBSERVER['foreign_startup'];self.assertEqual({x['pid'] for x in scan(rows,{'pid':11,'start':1},12,tracked,(1220,5))},{40,41})
        del rows[40];rows[41]['ppid']=1;self.assertEqual({x['pid'] for x in scan(rows,{'pid':11,'start':1},12,tracked,(1220,5))},{41})
    def test_live_installer_never_checkpoint_success(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-live-installer-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600);rows=proc_fixture();rows[20]={'pid':20,'ppid':1,'uid':1000,'comm':'bash','start':1,'group':20};clock=Clock()
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,lambda:rows,clock.now,clock.pause)
            self.assertFalse(q['startup_closed']);self.assertFalse(q['qualification_eligible'])
    def checkpoint_dir(self,d):
        root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600)
        return root,m
    def replace(self,path,text=''):
        # A copy-up or restore gives the same name a new inode, possibly with the same bytes.
        new=path.with_name('.restore-'+path.name);new.write_text(text);new.chmod(0o600);os.replace(new,path)
    def test_manifest_replaced_during_checkpoint_settles_then_observes_again(self):
        for text,eligible in [('ASCII',True),('CHANGED',False)]:
            with tempfile.TemporaryDirectory(prefix='boat-v2-manifest-race-') as d:
                root,m=self.checkpoint_dir(d);clock=Clock();calls=[0];expected=digest(m.read_bytes())
                def rows():
                    calls[0]+=1
                    if calls[0]==2:self.replace(m,text)
                    return proc_fixture()
                q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,expected,30,rows,clock.now,clock.pause)
                self.assertTrue(q['startup_closed'],q);self.assertEqual(q['qualification_eligible'],eligible)
                self.assertGreaterEqual(clock.t,OBSERVER['SETTLE_SECONDS'])
    def test_bootstrap_lock_replaced_during_observation_settles_then_observes_again(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-lock-race-') as d:
            root,m=self.checkpoint_dir(d);clock=Clock();calls=[0]
            def rows():
                calls[0]+=1
                if calls[0]==1:self.replace(root/'bootstrap.lock')
                return proc_fixture()
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),30,rows,clock.now,clock.pause)
            self.assertTrue(q['startup_closed'],q);self.assertTrue(q['qualification_eligible'])
    def test_lock_copied_up_by_its_own_open_settles(self):
        # The first open of a lower-layer file for writing gives it a new inode.
        with tempfile.TemporaryDirectory(prefix='boat-v2-copy-up-') as d:
            root,m=self.checkpoint_dir(d);clock=Clock();real=os.open;first=[True]
            def copy_up(path,flags,*a):
                if first[0] and str(path).endswith('bootstrap.lock') and flags&os.O_RDWR:
                    first[0]=False;self.replace(Path(path))
                return real(path,flags,*a)
            with patch.dict(OBSERVER['os'].__dict__,{'open':copy_up}):
                q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),30,proc_fixture_reader(),clock.now,clock.pause)
            self.assertTrue(q['startup_closed'],q)
    def test_settling_never_authorizes_startup_after_the_deadline(self):
        for seconds in [5.5,6,6.5,7]:
            with tempfile.TemporaryDirectory(prefix='boat-v2-late-') as d:
                root,m=self.checkpoint_dir(d);clock=Clock();calls=[0]
                def rows():
                    calls[0]+=1
                    if calls[0]==1:self.replace(root/'bootstrap.lock')
                    return proc_fixture()
                q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),seconds,rows,clock.now,clock.pause)
                self.assertLessEqual(clock.t,seconds,seconds)
                if q['startup_closed']:self.assertLess(clock.t,seconds)
                else:self.assertEqual(q['reason_code'],'startup_busy:bootstrap_lock_replaced')
    def test_a_state_directory_absent_during_settling_is_waited_for(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-absent-') as d:
            (Path(d)/'state').mkdir();root,m=self.checkpoint_dir(Path(d)/'state');clock=Clock();calls=[0];moved=Path(d)/'moved'
            def rows():
                calls[0]+=1
                if calls[0]==1:self.replace(root/'bootstrap.lock')
                return proc_fixture()
            def pause(x):
                clock.pause(x)
                if 1<=clock.t<3 and root.exists():root.rename(moved)
                if clock.t>=3 and moved.exists():moved.rename(root)
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),30,rows,clock.now,pause)
            self.assertTrue(q['startup_closed'],q);self.assertGreater(clock.t,3+OBSERVER['SETTLE_SECONDS'])
            moved=Path(d)/'gone';root.rename(moved)
            self.assertIsNone(OBSERVER['state_snapshot'](root))
            self.assertFalse(OBSERVER['settle'](root,clock.now,clock.pause,clock.t+10))
    def test_an_absent_directory_at_both_snapshots_is_not_closure(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-absent2-') as d:
            root,m=self.checkpoint_dir(d);clock=Clock();real=OBSERVER['state_snapshot'];expected=digest(m.read_bytes())
            # The directory is absent at both snapshot reads of every attempt.
            with patch.dict(OBSERVER,{'state_snapshot':lambda state:None}):
                q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,expected,10,proc_fixture_reader(),clock.now,clock.pause)
            self.assertFalse(q['startup_closed']);self.assertEqual(q['reason_code'],'startup_busy:state_restore_in_progress')
    def test_manager_and_slow_reads_stay_inside_the_deadline(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-slow-') as d:
            root,m=self.checkpoint_dir(d);clock=Clock();asked=[]
            def manager(rows,left):
                asked.append(left);clock.pause(min(10,left));return None
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),1,proc_fixture_reader(),clock.now,clock.pause,manager)
            self.assertLessEqual(clock.t,1);self.assertTrue(all(0<x<=1 for x in asked),asked);self.assertFalse(q['startup_closed'])
            calls=[]
            with patch.dict(OBSERVER,{'command':lambda argv,seconds=15:(calls.append(seconds),b'MainPID=1\n')[1]}):
                OBSERVER['user_manager'](observed_session_rows(),1000,3.5);OBSERVER['user_manager'](observed_session_rows(),1000,0)
            self.assertEqual(calls,[3.5])
    def test_retained_phase_directories_do_not_count_toward_the_bound(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-retained-') as d:
            root,m=self.checkpoint_dir(d)
            for n in range(200):(root/('qualification-op-%d-startup'%n)).mkdir()
            self.assertEqual(set(OBSERVER['state_snapshot'](root)),{'bootstrap.lock','toolchain.json'})
            clock=Clock();q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),5,proc_fixture_reader(),clock.now,clock.pause)
            self.assertTrue(q['startup_closed'])
            for n in range(OBSERVER['STATE_ENTRIES']+1):(root/('entry-%d'%n)).touch()
            with self.assertRaises(OBSERVER['Refusal']):OBSERVER['state_snapshot'](root)
    def test_restore_that_never_settles_refuses_at_the_deadline_with_its_code(self):
        for target,code in [('tools.toml','startup_busy:state_restore_in_progress'),('bootstrap.lock','startup_busy:bootstrap_lock_replaced')]:
            with tempfile.TemporaryDirectory(prefix='boat-v2-restore-') as d:
                root,m=self.checkpoint_dir(d);clock=Clock();(root/'tools.toml').write_text('x');(root/'tools.toml').chmod(0o600)
                n=[0]
                def pause(x):
                    clock.pause(x);n[0]+=1;self.replace(root/target,str(n[0]))
                q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),30,proc_fixture_reader(),clock.now,pause)
                self.assertFalse(q['startup_closed']);self.assertEqual(q['reason_code'],code)
                self.assertGreaterEqual(clock.t,30)
    def test_startup_phase_more_than900_and_finite1950(self):
        r=fixture();io=PhaseIO(r);intent={'mono':100,'wall':io.wall()};io.delay=1000
        self.assertTrue(io.startup(intent,True));self.assertGreater(io.mono(),1000);self.assertEqual(io.read('startup-closed.json')['bootstrap_occurrence'],'unknown')
        io=PhaseIO(r);io.wait(1951)
        with self.assertRaises(Refusal):io.startup(intent,True)
        self.assertEqual(io.phase_calls,[])
    def test_pending_startup_receipt_never_duplicate_start_or_qualify(self):
        io=PhaseIO(fixture());io.pending=True;intent={'mono':100,'wall':io.wall()}
        self.assertFalse(io.startup(intent,True));self.assertFalse(io.exists('startup-closed.json'));first=io.read('startup-started.json')
        io.pending=False;self.assertTrue(io.startup(intent,False));self.assertEqual(io.read('startup-started.json'),first)
    def test_allowed_bootstrap_changed_manifest_blocks_qualification_but_closes(self):
        io=PhaseIO(fixture());io.bad_manifest=True
        with self.assertRaises(Refusal):io.startup({'mono':100,'wall':io.wall()},True)
        self.assertTrue(io.exists('startup-closed.json'));self.assertFalse(io.read('startup-closed.json')['qualification_eligible'])
    def test_reason_code_is_bounded_and_literal_only_for_refusals(self):
        self.assertEqual(reason_code(Refusal('startup unit identity')),'refusal:startup unit identity')
        code=reason_code(Refusal('x\n\x00'+'y'*400));self.assertTrue(code.startswith('refusal:x??'));self.assertLessEqual(len(code),REASON_MAX)
        try:{}['secret-looking-key']
        except KeyError as error:code=reason_code(error)
        self.assertRegex(code,r'^unexpected:KeyError:test_reason_code_is_bounded_and_literal_only_for_refusals:[0-9]+$');self.assertNotIn('secret',code)
        try:raise OSError(2,'/home/worker/private path')
        except OSError as error:code=reason_code(error)
        self.assertNotIn('private',code);self.assertRegex(code,'^unexpected:(OSError|FileNotFoundError):')
    def test_phase_entry_records_reason_code_for_every_refusal(self):
        case=self
        for failure,expected in [('refusal','refusal:startup cgroup'),(FileNotFoundError(2,'/sys/fs/cgroup/x/memory.swap.peak'),'unexpected:FileNotFoundError:'),(None,'startup_busy:unallowed_processes')]:
            class Capture(InertIO):
                phase=c.RealIO.phase
                def boat(self,args,seconds,input_bytes,json_mode=False):
                    tree=ast.parse(input_bytes.decode());values={}
                    for node in tree.body:
                        if isinstance(node,ast.Assign)and isinstance(node.targets[0],ast.Name)and node.targets[0].id in ('PAYLOAD','CODE'):values[node.targets[0].id]=ast.literal_eval(node.value)
                    import base64
                    program=base64.b64decode(values['CODE']).decode();payload=json.loads(values['PAYLOAD']);parsed=ast.parse(program)
                    ns={'__name__':'inert_generated_definitions'};exec(compile(program,'actual-generated-program','exec'),ns)
                    with tempfile.TemporaryDirectory(prefix='boat-v2-phase-entry-') as d:
                        payload.update(result_directory=d,program_sha256=digest(program.encode()),launcher_identity={'pid':11,'start':1})
                        def observe(x):
                            if failure=='refusal':ns['need'](False,'startup cgroup')
                            if failure is not None:raise failure
                            return {'startup_closed':False,'installer_quiescent':False,'qualification_eligible':False,'reason_code':'startup_busy:unallowed_processes','unallowed':[],'unallowed_count':0,'engine_or_job_started':False}
                        ns['observe_startup']=observe
                        ns['sys']=type('Args',(),{'argv':['inert',json.dumps(payload)]})()
                        ns['__name__']='__main__'
                        with patch('builtins.print'):
                            with case.assertRaises(SystemExit)as status:exec(compile(ast.Module(body=[parsed.body[-1]],type_ignores=[]),'actual-generated-entry','exec'),ns)
                        case.assertEqual(status.exception.code,1);result=load(Path(d)/'probe-result.json')
                        case.assertFalse(result['startup_closed']);case.assertTrue(result['reason_code'].startswith(expected),result['reason_code'])
                        case.assertNotIn('memory.swap.peak',json.dumps(result));case.assertIn('elapsed_seconds',result)
                    return {'exit':0,'error':None,'body':result}
            io=Capture(fixture());self.assertTrue(io.phase('startup',10)['reason_code'].startswith(expected))
    def test_launcher_refusal_prints_bounded_reason_code(self):
        case=self
        class Capture(InertIO):
            phase=c.RealIO.phase
            def boat(self,args,seconds,input_bytes,json_mode=False):
                printed=[]
                with patch('builtins.print',side_effect=lambda x:printed.append(x)):
                    exec(compile(input_bytes.decode(),'actual-generated-launcher','exec'),{'__name__':'__main__'})
                body=json.loads(printed[-1]);case.assertEqual(body['reason_code'],'launcher:refusal:remote user');case.assertFalse(body['startup_closed'])
                return {'exit':0,'error':None,'body':body}
        io=Capture(fixture());self.assertEqual(io.phase('startup',10)['reason_code'],'launcher:refusal:remote user')
    def test_startup_refusal_receipt_and_failed_qualification_name_the_reason(self):
        io=PhaseIO(fixture());intent={'mono':100,'wall':io.wall()}
        io.refusal_body={'startup_closed':False,'phase_closed':True,'qualification_eligible':False,'reason_code':'startup_busy:unallowed_processes','unallowed':[{'pid':8008,'comm':'dbus-run-sessio','uid':1000,'ppid':8001,'parent_comm':None,'parent_uid':None}],'unallowed_count':1,'elapsed_seconds':25.1,'terminal_properties':{'ExecMainStatus':'1','Result':'exit-code','ControlGroup':'/x'}}
        self.assertFalse(io.startup(intent,True));q=io.read('startup-refused.json')
        self.assertEqual(q['reason_code'],'startup_busy:unallowed_processes');self.assertEqual(q['unallowed'][0]['comm'],'dbus-run-sessio');self.assertEqual(q['terminal'],{'ExecMainStatus':'1','Result':'exit-code'})
        self.assertFalse(io.startup(intent,False));self.assertEqual(io.read('startup-refused.json'),q)
        io.refusal_body={'startup_closed':False,'phase_closed':True};self.assertFalse(PhaseIO.startup(io,intent,False))
        fresh=PhaseIO(fixture());fresh.refusal_body={'startup_closed':False,'phase_closed':False,'failure':'arbitrary provider text'};self.assertFalse(fresh.startup(intent,True))
        self.assertEqual(fresh.read('startup-refused.json')['reason_code'],'unreported');self.assertNotIn('arbitrary',json.dumps(fresh.read('startup-refused.json')))
        for code,kept in [('/private/provider-token=SECRET',False),('refusal:startup cgroup',True),('launcher:refusal:remote user',True),('refusal:invented secret text',False),
                          ('unexpected:KeyError:observe:12',True),('unexpected:Key Error',False),('unit_unhealthy:exit-code:1',True),('startup_busy:unallowed_processes',True),(['x'],False),({'a':1},False)]:
            io=PhaseIO(fixture());io.refusal_body={'startup_closed':False,'phase_closed':True,'reason_code':code,
                'unallowed':[{'pid':1,'comm':{'secret':'SECRET'},'uid':1000,'ppid':0,'parent_comm':['SECRET'],'parent_uid':'0'},{'pid':2,'comm':'x'*40,'uid':1000,'ppid':1,'parent_comm':'ok','parent_uid':0}],
                'terminal_properties':{'Result':'exit-code','ExecMainStatus':'/SECRET'}}
            io.startup(intent,False);q=io.read('startup-refused.json')
            self.assertEqual(q['reason_code'],code if kept else 'malformed',code);self.assertNotIn('SECRET',json.dumps(q))
            self.assertEqual([(x['comm'],x['parent_comm'],x['parent_uid']) for x in q['unallowed']],[(None,None,None),(None,'ok',0)]);self.assertEqual(q['terminal'],{'Result':'exit-code'})
        io=InertIO(fixture());io.startup_bad=True
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertEqual(io.read('qualification-failed.json')['reason_code'],'refusal:startup still busy or unknown')
    def test_ready_before_hook_completion_never_qualifies(self):
        io=InertIO(fixture());io.startup_bad=True
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertFalse(io.exists('qualification.json'));self.assertTrue(io.exists('stop-trigger.json'))
    def test_unknown_startup_closure_at_disconnect_stays_unresolved(self):
        io=InertIO(fixture());io.startup_bad=True;intent={'mono':100,'wall':io.wall(),'deadline_mono':io.mono(),'deadline_wall':io.wall()};io.record('controller-intent.json',intent);io.record('resume-intent.json',{})
        self.assertFalse(c.guard_loop(io.r,io,intent));self.assertTrue(io.exists('unresolved.json'));self.assertEqual(io.calls,[])
    def test_old_snapshot_refused_even_with_fresh_stop_times(self):
        r=fixture();x=info(r);x['stoppedAt']=iso(1800000000);x['stop'].update(endedAt=iso(1800000000),forced=False)
        with self.assertRaises(Refusal):stop_complete(x,'stop_old',1800000000,r['snapshot_continuity']['snapshot_completed_at'],'documented_normal_stop_final_snapshot')
    def test_new_snapshot_tied_normal_stop_documented_guarantee(self):
        r=fixture();x=info(r);x['snapshotCompletedAt']=iso(1800000000-30);x['stoppedAt']=iso(1800000000);x['stop'].update(endedAt=iso(1800000000),forced=False)
        stop_complete(x,'stop_old',1800000000,r['snapshot_continuity']['snapshot_completed_at'],'documented_normal_stop_final_snapshot')
        with self.assertRaises(Refusal):stop_complete(x,'stop_old',1800000000,r['snapshot_continuity']['snapshot_completed_at'],'fresh_since_request_and_newer_than_pre_resume')
    def test_real_phase_program_composition_and_entry_receipt(self):
        case=self
        class Capture(InertIO):
            phase=c.RealIO.phase
            def boat(self,args,seconds,input_bytes,json_mode=False):
                tree=ast.parse(input_bytes.decode());values={}
                for node in tree.body:
                    if isinstance(node,ast.Assign)and isinstance(node.targets[0],ast.Name)and node.targets[0].id in ('PAYLOAD','CODE'):values[node.targets[0].id]=ast.literal_eval(node.value)
                import base64
                program=base64.b64decode(values['CODE']).decode();payload=json.loads(values['PAYLOAD']);parsed=ast.parse(program)
                # Execute actual generated definitions and entry, mock only the
                # host/manager boundary. The entry uses the real atomic writer.
                ns={'__name__':'inert_generated_definitions'};exec(compile(program,'actual-generated-program','exec'),ns)
                with tempfile.TemporaryDirectory(prefix='boat-v2-phase-entry-') as d:
                    payload.update(result_directory=d,program_sha256=digest(program.encode()),launcher_identity={'pid':11,'start':1})
                    ns['observe_startup']=lambda x:{'startup_closed':True,'qualification_eligible':True,'engine_or_job_started':False}
                    ns['sys']=type('Args',(),{'argv':['inert',json.dumps(payload)]})()
                    ns['__name__']='__main__'
                    entry=parsed.body[-1]
                    with patch('builtins.print'):
                        with case.assertRaises(SystemExit)as status:exec(compile(ast.Module(body=[entry],type_ignores=[]),'actual-generated-entry','exec'),ns)
                    case.assertEqual(status.exception.code,0);result=load(Path(d)/'probe-result.json');case.assertEqual(result['phase'],'startup');case.assertTrue(result['startup_closed'])
                    case.assertIn(result['phase_memory']['source'],('cgroup','unavailable'))
                return {'exit':0,'error':None,'body':{'startup_closed':True,'phase_closed':True}}
        io=Capture(fixture());self.assertTrue(io.phase('startup',10)['startup_closed'])
    def test_live_limits_actual_projection_and_negatives(self):
        facts=limits_fixture()['safe_body']
        class LimitIO(InertIO):
            limits=c.RealIO.limits
            def boat(self,*a,**k):return {'exit':0,'error':None,'body':self.body}
        r=fixture();r['start_ledger']['starts_and_reservations']=5;io=LimitIO(r);io.body=copy.deepcopy(facts)
        q=io.limits();self.assertEqual(q['day']['used'],4);self.assertTrue(io.exists('account-starts-before-resume.json'))
        for change in ['day','hour','minute','large','ledger']:
            r=fixture();r['start_ledger']['starts_and_reservations']=5;io=LimitIO(r);io.body=copy.deepcopy(facts)
            if change in ['day','hour','minute']:io.body['starts'][change]['remaining']=0
            elif change=='large':io.body['machineTypes']=[{'type':'large','available':False}]
            else:r['start_ledger']['starts_and_reservations']=4
            with self.assertRaises(Refusal):io.limits()
    def test_stop_requires_current_quiescence_not_old_startup_only(self):
        io=InertIO(fixture());io.record('startup-closed.json',{'startup_closed':True})
        io.stop_quiescent=lambda:False
        self.assertFalse(c.reconcile_stop(io.r,io));self.assertTrue(io.exists('unresolved.json'));self.assertEqual(io.calls,[])
    def test_stopcheck_real_adapter_and_wrong_current_closure(self):
        class StopIO(InertIO):
            stop_quiescent=c.RealIO.stop_quiescent
            def phase(self,phase,seconds):
                self.asserted=(phase,seconds);return self.response
        io=StopIO(fixture());io.response={'startup_closed':True,'phase_closed':True};self.assertTrue(io.stop_quiescent());self.assertEqual(io.asserted,('stopcheck',5));self.assertTrue(io.exists('stop-startup-closed.json'))
        io=StopIO(fixture());io.response={'startup_closed':False,'phase_closed':True}
        with self.assertRaises(Refusal):io.stop_quiescent()
    def test_export_consumer_rejects_expiry_boot_invocation_or_envfile(self):
        r=fixture();r['useful_job_ready']=True;r['separate_Root_handoff']={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':600};io=InertIO(r);c.drive(r,io)
        from qualified_worker_admission import validate_admission
        admission=io.read('qualified-worker-admission.json');guard=io.read('guard-admission.json');validate_admission(admission,guard['boot_id'],guard['loaded'],io.wall(),profile())
        with self.assertRaises(ValueError):validate_admission(admission,'other-boot',guard['loaded'],io.wall(),profile())
        wrong=dict(guard['loaded'],InvocationID='d'*32)
        with self.assertRaises(ValueError):validate_admission(admission,guard['boot_id'],wrong,io.wall(),profile())
        with self.assertRaises(ValueError):validate_admission(admission,guard['boot_id'],guard['loaded'],io.wall()+10800,profile())
        other=dict(admission,runtime_env_file={'path':'/invented','sha256':'a'*64})
        with self.assertRaises(ValueError):validate_admission(other,guard['boot_id'],guard['loaded'],io.wall(),profile())
    def test_no_admission_on_unreflected_Root_handoff_or_qualification(self):
        from qualified_worker_admission import make_admission
        r=fixture();io=InertIO(r);intent={'worker':worker_id(),'wall':io.wall(),'deadline_wall':io.wall()+10500,'deadline_mono':io.mono()+10500}
        with self.assertRaises(ValueError):make_admission(r,intent,{}, {}, {}, {},io.wall())
    def test_explicit_profile_false_and_unknown_fields_refuse(self):
        from config_schema import validate_profile
        bad=json.loads(Path(__file__).with_name('profile.FALSE.json').read_text())
        with self.assertRaises(ValueError):validate_profile(bad)
        good=synthetic_profile();good['unexpected_credential']='NOT_A_REAL_SECRET'
        with self.assertRaises(ValueError):validate_profile(good)
        bad=synthetic_profile();bad['profile_review_sha256']=None
        with self.assertRaises(ValueError):validate_profile(bad)
    def test_bad_profile_clears_previous_process_binding(self):
        bind_profile(synthetic_profile());self.assertEqual(worker_id(),'bx_fixture')
        bad=synthetic_profile();bad['admitted_by_Root']=False
        with self.assertRaises(ValueError):bind_profile(bad)
        self.assertIsNone(worker_id())
        with self.assertRaises(ValueError):profile()
    def test_profile_hash_and_owned_worker_release_binding(self):
        r=fixture();c.bind_release_profile(r)
        wrong=copy.deepcopy(r);wrong['profile_sha256']='0'*64
        with self.assertRaises(Refusal):c.bind_release_profile(wrong)
        wrong=copy.deepcopy(r);wrong['worker']='bx_otherfixture'
        with self.assertRaises(Refusal):c.bind_release_profile(wrong)
    def test_second_explicit_profile_reaches_CLI_and_admission(self):
        r=fixture();p=synthetic_profile('bx_secondfixture');p['worker_home']='/home/second-fixture';p['startup_repositories']={'default':'/home/second-fixture/default','retained':'/home/second-fixture/retained'};p['future_environment'].update(id='second-environment-id',name='second-environment',version=11,version_id='second-version');p['saved_toolchain_manifest_sha256']='f'*64
        bind_profile(p);OBSERVER['bind_profile'](p);raw=json.dumps(p,sort_keys=True)
        r.update(worker=p['worker'],profile_sha256=digest(raw.encode()),profile_review_sha256=p['profile_review_sha256'],_profile_json=raw,useful_job_ready=True,separate_Root_handoff={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':600})
        r['environment'].update(environment_id=p['future_environment']['id'],name=p['future_environment']['name'],version=11,latest_version_id='second-version',hook_sha256=p['future_environment']['hook_sha256'])
        r['snapshot_continuity'].update(pre_resume_environment_version=10,manifest_sha256=p['saved_toolchain_manifest_sha256'],startup_repo=p['startup_repositories']['default'],token_path=p['worker_home']+'/.config/mbx/remote-token')
        io=InertIO(r);self.assertEqual(c.drive(r,io),'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE')
        self.assertEqual(io.calls[0][1],'bx_secondfixture');self.assertIn('second-environment',io.calls[0]);admission=io.read('qualified-worker-admission.json')
        self.assertEqual(admission['worker'],'bx_secondfixture');self.assertEqual(admission['environment']['version'],11);self.assertEqual(admission['toolchain_manifest']['sha256'],'f'*64)
    def test_profile_source_paths_and_machine_caps_not_auto_relaxed(self):
        from config_schema import validate_profile
        for change in ['machine','path','tool','hook']:
            p=synthetic_profile()
            if change=='machine':p['machine']['memoryGB']=32
            elif change=='path':p['worker_home']='/home/../escape'
            elif change=='tool':p['tool_versions']['mbx']='different'
            else:p['future_environment']['hook_sha256']=None
            with self.assertRaises(ValueError):validate_profile(p)
    def test_stable_main_requires_all_explicit_bindings(self):
        for argv in [['controller.py','run'],['controller.py','run','--owned-worker','bx_fixture','--release','/tmp/absent']]:
            with patch.object(sys,'argv',argv),patch('builtins.print'),patch.object(c,'verify_cli',side_effect=AssertionError('no CLI binding before inputs')):
                self.assertEqual(c.main(),1)
    def test_maintained_defaults_do_not_select_a_worker_or_environment(self):
        default=json.loads(Path(__file__).with_name('profile.FALSE.json').read_text())
        self.assertIsNone(default['worker']);self.assertIsNone(default['future_environment']['id']);self.assertIsNone(default['saved_toolchain_manifest_sha256']);self.assertFalse(default['admitted_by_Root'])
        release=json.loads(Path(__file__).with_name('release.FALSE.json').read_text());self.assertFalse(release['authorized_by_Root']);self.assertIsNone(release['worker'])
    def test_timeout_kills_owned_group_even_after_its_leader_exited(self):
        class Child:
            pid=123
            stdout=object()
            def __enter__(self):return self
            def __exit__(self,*a):pass
            def poll(self):return 0
            def wait(self,**k):return 0
        class Select:
            def register(self,*a):pass
            def get_map(self):return True
            def close(self):pass
        signals=[];ticks=iter([0,2])
        def kill(pid,sig):
            if sig==0:raise ProcessLookupError()
            signals.append((pid,sig))
        with patch.object(c.subprocess,'Popen',return_value=Child()),patch.object(c.selectors,'DefaultSelector',return_value=Select()),patch.object(c.time,'monotonic',side_effect=lambda:next(ticks,2)),patch.object(c.os,'killpg',side_effect=kill):
            result=c.bounded(['inert'],1)
        self.assertEqual(result['error'],'timeout')
        self.assertEqual(signals,[(123,signal.SIGTERM),(123,signal.SIGKILL)])

    def test_job_environment_is_only_literal_nonsecret_paths(self):
        from job_environment import environment,environment_body,expected_environment
        p=synthetic_profile();values=environment(p)
        self.assertEqual(set(values),{'PATH','RUSTUP_TOOLCHAIN','RUSTUP_AUTO_INSTALL','MBX_REMOTE_TOKEN_FILE'})
        self.assertEqual(values['RUSTUP_TOOLCHAIN'],p['tool_versions']['rust']);self.assertEqual(values['RUSTUP_AUTO_INSTALL'],'0')
        self.assertIn(p['worker_home']+'/.local/share/mise/installs/mr-boxington/'+p['tool_versions']['mbx'],values['PATH'].split(':'))
        self.assertEqual(values['MBX_REMOTE_TOKEN_FILE'],p['worker_home']+'/.config/mbx/remote-token')
        q={'safe_owner':{'operation_id':'boat-owned-warm-qual-inert'},'local_guard':{'unit':'boat-owned-warm-qual-inert.service'}}
        row=expected_environment(p,q)
        self.assertEqual(row['sha256'],digest(environment_body(p)));self.assertEqual(row['mode'],0o600)
        self.assertEqual(set(row),{'path','sha256','bytes','uid','mode'})
    def test_job_environment_false_profile_refuses_before_any_child(self):
        from job_environment import prepare
        with tempfile.TemporaryDirectory()as d:
            with patch.object(c,'bounded',side_effect=AssertionError('NO CHILD')):
                with self.assertRaises(ValueError):prepare(Path(__file__).with_name('profile.FALSE.json'),Path(d),'bx_fixture',Path(d)/'admission.json')
    def test_cache_profile_rejects_credentials_or_implicit_write_scope(self):
        from config_schema import validate_profile
        for row in [{'url':'https://user:password@cache.invalid','namespace':'fixture/dev','mode':'read-only'},{'url':'https://cache.invalid','namespace':'fixture/dev','mode':'write'}]:
            p=synthetic_profile();p['mbx_remote_config']=row
            with self.assertRaises(ValueError):validate_profile(p)

    # The startup unit's properties exactly as the worker's systemd 255 reported them
    # after op-3's probe exited (receipts/probe-op3.json): the transient unit was unloaded.
    OP3_UNLOADED="MainPID=0\nResult=success\nExecMainStatus=0\nMemoryCurrent=[not set]\nMemoryPeak=[not set]\nMemorySwapPeak=[not set]\nMemoryAccounting=yes\nLoadState=not-found\nActiveState=inactive\nSubState=dead\nActiveEnterTimestampMonotonic=0\nInactiveEnterTimestampMonotonic=0\n"
    def run_generated_launcher(self,phase,phase_memory,summary=b'',run_exit=0,shown=None,recover=False):
        """Run the actual generated launcher for one phase with the host boundary faked."""
        case=self;out={}
        class Capture(InertIO):
            phase_fn=c.RealIO.phase
            def boat(self,args,seconds,input_bytes,json_mode=False):
                with tempfile.TemporaryDirectory(prefix='boat-launcher-') as d:
                    home=Path(d).resolve()/'home';(home/'.local/state/sparkles-boat').mkdir(parents=True)
                    source=input_bytes.decode().replace('/home/worker-fixture',str(home))
                    calls=[]
                    class Child:
                        # Real pipes carry the faked output through the launcher's bounded reader.
                        def __init__(self,argv,stdin=None,stdout=None,stderr=None):
                            calls.append(argv[0])
                            if argv[0]=='systemctl':
                                text='LoadState=not-found\nActiveState=inactive\nMainPID=0\n' if calls.count('systemctl')==1 and not recover else (shown if shown is not None else case.OP3_UNLOADED)
                                out,err,self.code=text.encode(),b'',0
                            else:
                                case.assertEqual(argv[0],'systemd-run');payload=json.loads(argv[-1])
                                body={'program_sha256':payload['program_sha256'],'phase':payload['phase'],'unit':payload['operation_tag']+'-'+payload['phase']+'.service','startup_closed':True,'qualified':True,'engine_or_job_started':False}
                                if phase_memory is not None:body['phase_memory']=phase_memory
                                write_new(Path(payload['result_directory']),'probe-result.json',body);out,err,self.code=b'{}',summary,run_exit
                            self.stdout=self.pipe(out);self.stderr=self.pipe(err) if stderr==__import__('subprocess').PIPE else None;self.returncode=None
                        def pipe(self,data):
                            r,w=os.pipe();os.write(w,data);os.close(w);return os.fdopen(r,'rb')
                        def wait(self,timeout=None):self.returncode=self.code;return self.code
                        def kill(self):pass
                    printed=[]
                    with patch('subprocess.Popen',Child),patch('os.getuid',return_value=1000),patch('pathlib.Path.home',return_value=home),patch('builtins.print',side_effect=lambda x:printed.append(x)):
                        if recover:
                            tag=json.loads(ast.literal_eval([n.value for n in ast.parse(source).body if isinstance(n,ast.Assign) and n.targets[0].id=='PAYLOAD'][0]))['operation_tag']
                            op=home/'.local/state/sparkles-boat'/('qualification-'+tag+'-'+phase);op.mkdir(mode=0o700)
                            program=__import__('base64').b64decode(ast.literal_eval([n.value for n in ast.parse(source).body if isinstance(n,ast.Assign) and n.targets[0].id=='CODE'][0]))
                            write_new(op,'probe-result.json',{'program_sha256':digest(program),'phase':phase,'unit':tag+'-'+phase+'.service','startup_closed':True,'qualified':True,'phase_memory':phase_memory})
                        exec(compile(source,'actual-generated-launcher','exec'),{'__name__':'__main__'})
                    out['body']=json.loads(printed[-1]);out['calls']=calls
                    return {'exit':0,'error':None,'body':out['body']}
        io=Capture(fixture());io.phase_fn('startup',10) if phase=='startup' else io.phase_fn(phase)
        return out
    def test_op3_unloaded_unit_with_cgroup_peaks_is_healthy_in_every_phase(self):
        for phase in ('startup','metadata','stopcheck'):
            tag='boat-owned-warm-qual-inert-'+phase+'.service'
            body=self.run_generated_launcher(phase,{'source':'cgroup','cgroup':'/user.slice/app.slice/'+tag,'memory_peak':23278387,'swap_peak':0})['body']
            self.assertNotIn('reason_code',body,phase);self.assertTrue(body['startup_closed'],phase);self.assertTrue(body['phase_closed'],phase)
            self.assertEqual(body['terminal_evidence'],{'source':'cgroup','memory_peak':23278387,'swap_peak':0,'terminal_source':'systemd_run_exit'},phase)
    def test_op3_unloaded_unit_falls_back_to_the_systemd_run_summary(self):
        summary=b'          Finished with result: success\nMain processes terminated with: code=exited, status=0/SUCCESS\n               Service runtime: 11.786s\n                   Memory peak: 22.2M (swap: 0B)\n'
        for phase in ('startup','metadata','stopcheck'):
            body=self.run_generated_launcher(phase,{'source':'unavailable','reason_code':'refusal:phase unit cgroup'},summary)['body']
            self.assertNotIn('reason_code',body,phase);self.assertEqual(body['terminal_evidence']['source'],'systemd_run_summary')
            self.assertEqual(body['terminal_evidence']['memory_peak'],223*1024*1024//10)
    def test_loaded_unit_with_empty_or_missing_cgroup_does_not_take_a_record(self):
        good={'source':'cgroup','cgroup':'/x/boat-owned-warm-qual-inert-startup.service','memory_peak':1,'swap_peak':0}
        for shown in ['LoadState=loaded\nActiveState=inactive\nMainPID=0\nControlGroup=\nResult=success\nExecMainStatus=0\nMemoryPeak=[not set]\nMemorySwapPeak=[not set]\n',
                      'LoadState=loaded\nActiveState=inactive\nMainPID=0\nResult=success\nExecMainStatus=0\nMemoryPeak=[not set]\nMemorySwapPeak=[not set]\n']:
            body=self.run_generated_launcher('startup',good,shown=shown)['body'];self.assertEqual(body['reason_code'],'unit_unhealthy:memory_peak_unavailable')
    def test_op3_unloaded_unit_without_either_source_still_refuses(self):
        for phase in ('startup','metadata','stopcheck'):
            for memory,summary in [(None,b''),({'source':'unavailable','reason_code':'refusal:phase memory peaks'},b'Memory peak: 22.2M\n'),({'source':'cgroup','cgroup':'/x','memory_peak':'1','swap_peak':0},b'')]:
                body=self.run_generated_launcher(phase,memory,summary)['body']
                self.assertEqual(body['reason_code'],'unit_unhealthy:memory_peak_unavailable',phase);self.assertFalse(body['startup_closed']);self.assertFalse(body.get('qualified',False))
                self.assertEqual(c.remote_reason(body['reason_code']),body['reason_code'])
    def test_unloaded_unit_status_comes_only_from_systemd_run(self):
        good={'source':'cgroup','cgroup':'/x/boat-owned-warm-qual-inert-startup.service','memory_peak':1,'swap_peak':0}
        body=self.run_generated_launcher('startup',good,run_exit=1)['body'];self.assertEqual(body['reason_code'],'unit_unhealthy:unloaded:exit-1')
        body=self.run_generated_launcher('startup',good,recover=True)['body'];self.assertEqual(body['reason_code'],'unit_unhealthy:unloaded:status_unknown')
        for code in ('unit_unhealthy:unloaded:exit-1','unit_unhealthy:unloaded:status_unknown','unit_unhealthy:swap_used','unit_unhealthy:memory_peak_over_cap'):self.assertEqual(c.remote_reason(code),code)
    def test_terminal_health_peaks_bounds_and_loaded_units(self):
        unloaded=c.props(self.OP3_UNLOADED);cg={'source':'cgroup','cgroup':'/a/u.service','memory_peak':MEMORY,'swap_peak':0}
        real=terminal_health
        def health(result,*rest):return real(dict(result,unit='u.service'),*rest)
        self.assertTrue(health({'phase_memory':cg},unloaded,True,0)[0])
        self.assertEqual(health({'phase_memory':dict(cg,memory_peak=MEMORY+1)},unloaded,True,0)[1],'unit_unhealthy:memory_peak_over_cap')
        self.assertEqual(health({'phase_memory':dict(cg,swap_peak=4096)},unloaded,True,0)[1],'unit_unhealthy:swap_used')
        self.assertEqual(health({'phase_memory':cg},unloaded,False,0)[1],'unit_unhealthy:open')
        # systemd truncates to one decimal, so 512.0M may stand for more than the cap.
        self.assertEqual(health({},unloaded,True,0,'Memory peak: 512.0M (swap: 0B)')[1],'unit_unhealthy:memory_peak_over_cap')
        self.assertTrue(health({},unloaded,True,0,'Memory peak: 511.9M (swap: 0B)')[0])
        self.assertEqual(health({},unloaded,True,0,'Memory peak: 1.0M (swap: 0.1K)')[1],'unit_unhealthy:swap_used')
        self.assertEqual(health({},unloaded,True,0,'Memory peak: 3.5B (swap: 0B)')[1],'unit_unhealthy:memory_peak_unavailable')
        # The last summary line counts, and a cgroup read wins over the summary.
        self.assertEqual(health({},unloaded,True,0,'Memory peak: 1.0M (swap: 0B)\nMemory peak: 600.0M (swap: 0B)')[1],'unit_unhealthy:memory_peak_over_cap')
        self.assertTrue(health({'phase_memory':dict(cg,memory_peak=1)},unloaded,True,0,'Memory peak: 600.0M (swap: 0B)')[0])
        loaded={'LoadState':'loaded','ActiveState':'inactive','MainPID':'0','ControlGroup':'/a/u.service','Result':'success','ExecMainStatus':'0','MemoryPeak':'1000','MemorySwapPeak':'0'}
        ok,code,evidence=health({'phase_memory':dict(cg,memory_peak=10)},loaded,True,None);self.assertTrue(ok);self.assertEqual(evidence['terminal_source'],'unit')
        self.assertEqual(health({'phase_memory':dict(cg,memory_peak=10)},dict(loaded,MemoryPeak=str(MEMORY+1)),True,None)[1],'unit_unhealthy:memory_peak_over_cap')
        self.assertEqual(health({'phase_memory':dict(cg,memory_peak=10)},dict(loaded,MemorySwapPeak='1'),True,None)[1],'unit_unhealthy:swap_used')
        self.assertEqual(health({'phase_memory':cg},dict(loaded,Result='exit-code',ExecMainStatus='1'),True,0)[1],'unit_unhealthy:exit-code:1')
        # A cgroup read bound to another cgroup than the loaded unit's does not count,
        # nor does one when the loaded unit reports no cgroup, nor one naming another unit.
        for row,shown in [(dict(cg,cgroup='/other.service'),dict(loaded,MemoryPeak='[not set]')),(dict(cg,cgroup='/another-unit.service'),dict(loaded,ControlGroup='',MemoryPeak='[not set]')),
                          (cg,{k:v for k,v in loaded.items() if k not in ('ControlGroup','MemoryPeak')}),(dict(cg,cgroup='/a/other.service'),unloaded),(dict(cg,cgroup=None),unloaded)]:
            self.assertEqual(health({'phase_memory':row},shown,True,0)[1],'unit_unhealthy:memory_peak_unavailable',row)
        self.assertEqual(real({'phase_memory':cg},unloaded,True,0)[1],'unit_unhealthy:memory_peak_unavailable')
    def test_own_cgroup_memory_reads_the_phase_unit_only(self):
        f=OBSERVER['own_cgroup_memory']
        with tempfile.TemporaryDirectory() as d:
            root=Path(d)/'cg';unit=root/'user.slice/x-startup.service';unit.mkdir(parents=True);proc=Path(d)/'cgroup'
            proc.write_text('0::/user.slice/x-startup.service\n');(unit/'memory.peak').write_text('23278387\n');(unit/'memory.swap.peak').write_text('0\n')
            self.assertEqual(f('x-startup.service',proc,root),{'source':'cgroup','cgroup':'/user.slice/x-startup.service','memory_peak':23278387,'swap_peak':0})
            self.assertEqual(f('y-startup.service',proc,root),{'source':'unavailable','reason_code':'refusal:phase unit cgroup'})
            (unit/'memory.swap.peak').write_text('max\n');self.assertEqual(f('x-startup.service',proc,root)['reason_code'],'refusal:phase memory peaks')
            (unit/'memory.swap.peak').unlink();self.assertRegex(f('x-startup.service',proc,root)['reason_code'],'^unexpected:FileNotFoundError:')
            proc.write_text('1:name=x:/\n');self.assertEqual(f('x-startup.service',proc,root)['reason_code'],'refusal:phase cgroup')

    # Shared refusal records and the diagnostic hold.
    def metadata_io(self,body):
        class MetaIO(PhaseIO):
            qualification=c.RealIO.qualification
            def __init__(self,r):super().__init__(r);self.diagnosed=[]
            def phase(self,phase,seconds=0):
                self.phase_calls.append((phase,seconds))
                if phase!='metadata':return {'startup_closed':True,'phase_closed':True,'qualification_eligible':True,'bootstrap_occurrence':'unknown'}
                if isinstance(body,Exception):raise body
                return copy.deepcopy(body)
            def diagnose(self,seconds):
                self.diagnosed.append((self.mono(),seconds));return {'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        return MetaIO(fixture())
    OP4_LIKE={'qualified':False,'startup_closed':False,'metadata_closed':True,'phase_closed':True,'engine_or_job_started':False,'reason_code':'refusal:insufficient host headroom for proposed12GiB job',
              'elapsed_seconds':4.2,'terminal_properties':{'Result':'success','ExecMainStatus':'0','LoadState':'not-found','ActiveState':'inactive','MemoryPeak':'[not set]'},
              'terminal_evidence':{'source':'cgroup','memory_peak':60000000,'swap_peak':0,'terminal_source':'systemd_run_exit'},'phase_memory':{'source':'cgroup','cgroup':'/a/x.service','memory_peak':60000000,'swap_peak':0},
              'memory_events':{'max':'0','oom':'0'},'failure':'bounded phase refused; no arbitrary error content'}
    def test_a_metadata_refusal_names_every_failed_check_and_the_remote_reason(self):
        io=self.metadata_io(self.OP4_LIKE);io.record('startup-closed.json',{});intent={'deadline_mono':io.mono()+10000,'deadline_wall':io.wall()+10000}
        with self.assertRaises(Refusal) as caught:io.qualification(intent)
        self.assertEqual(str(caught.exception),'metadata refused: probe_not_qualified')
        q=io.read('metadata-refused.json')
        self.assertEqual(q['reason_code'],'refusal:metadata refused: probe_not_qualified');self.assertEqual(q['remote_reason_code'],'refusal:insufficient host headroom for proposed12GiB job')
        self.assertEqual(q['failed_checks'],['probe_not_qualified','startup_not_closed','memory_cap','swap_cap','manifest_changed','worker_boot_id'])
        self.assertEqual(q['terminal'],{'Result':'success','ExecMainStatus':'0','LoadState':'not-found','ActiveState':'inactive'})
        self.assertEqual(q['evidence']['terminal_evidence'],{'memory_peak':60000000,'swap_peak':0,'source':'cgroup','terminal_source':'systemd_run_exit'})
        self.assertNotIn('failure',json.dumps(q));self.assertTrue(io.exists('qualification-closed.json'));self.assertFalse(io.exists('qualification.json'))
        # A failed remote invocation is recorded too, with no checks to name.
        io=self.metadata_io(Refusal('remote phase invocation failed'));io.record('startup-closed.json',{})
        with self.assertRaises(Refusal):io.qualification(intent)
        q=io.read('metadata-refused.json');self.assertEqual((q['reason_code'],q['remote_reason_code'],q['failed_checks']),('refusal:remote phase invocation failed',None,[]))
        # A passing result records no refusal.
        good=dict(self.OP4_LIKE,qualified=True,startup_closed=True,memory_max=str(MEMORY),swap_max='0',manifest_unchanged=True,worker_boot_id='87654321-4321-4321-4321-210987654321')
        io=self.metadata_io(good);io.record('startup-closed.json',{});io.qualification(intent);self.assertFalse(io.exists('metadata-refused.json'));self.assertTrue(io.exists('qualification.json'))
    def test_a_refused_qualification_holds_its_stop_to_read_the_worker(self):
        io=self.metadata_io(self.OP4_LIKE)
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertEqual(io.read('qualification-failed.json')['reason_code'],'refusal:metadata refused: probe_not_qualified')
        names=list(io.records)
        self.assertTrue(io.exists('diagnostics.json'));self.assertEqual(len(io.diagnosed),1)
        self.assertLess(names.index('metadata-refused.json'),names.index('diagnostics.json'));self.assertLess(names.index('diagnostics.json'),names.index('stop-trigger.json'))
        hold=io.read('diagnostic-hold.json');self.assertLessEqual(hold['seconds'],c.HOLD_MAX)
        self.assertTrue(io.read('diagnostic-hold-ended.json')['captured'])
    def test_the_hold_is_bounded_and_always_ends_in_the_trigger(self):
        for case in ('fails','raises_base'):
            io=self.metadata_io(self.OP4_LIKE)
            def diagnose(seconds,io=io):
                io.diagnosed.append((io.mono(),seconds));io.wait(seconds)
                raise Refusal('diagnostics unavailable')
            io.diagnose=diagnose
            with self.assertRaises(Refusal):c.drive(io.r,io)
            hold=io.read('diagnostic-hold.json');self.assertEqual(hold['seconds'],c.HOLD_MAX)
            self.assertTrue(all(t+s+c.DIAGNOSE_CLEANUP<=hold['end_boottime'] for t,s in io.diagnosed));self.assertGreater(len(io.diagnosed),1)
            self.assertLessEqual(io.read('diagnostic-hold-ended.json')['ended_boottime'],hold['end_boottime']+c.DIAGNOSE_RETRY)
            self.assertFalse(io.read('diagnostic-hold-ended.json')['captured']);self.assertTrue(io.exists('stop-trigger.json'))
            self.assertTrue(io.exists('diagnostics-failed-0001.json'))
        # Late in the controller's life the hold ends before the controller's own limit.
        io=self.metadata_io(self.OP4_LIKE);io.diagnose=lambda seconds:(_ for _ in ()).throw(Refusal('diagnostics unavailable'))
        real_phase=io.phase
        def late(phase,seconds=0):
            io.t+=c.CONTROLLER_RUNTIME-c.HOLD_RESERVE-100;return real_phase(phase,seconds)
        io.phase=late
        with self.assertRaises(Refusal):c.drive(io.r,io)
        intent=io.read('controller-intent.json');hold=io.read('diagnostic-hold.json')
        self.assertEqual(hold['end_boottime'],intent['mono']+c.CONTROLLER_RUNTIME-c.HOLD_RESERVE);self.assertLess(hold['seconds'],c.HOLD_MAX)
        # An error inside the hold itself still leaves the trigger.
        io=self.metadata_io(self.OP4_LIKE);io.names=lambda:(_ for _ in ()).throw(OSError(5,'x'))
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertTrue(io.exists('stop-trigger.json'));self.assertFalse(io.exists('diagnostic-hold.json'))
    def test_no_hold_without_a_running_worker_or_after_a_handoff(self):
        for mode in ('malformed','timeout'):
            io=self.metadata_io(self.OP4_LIKE);io.mode=mode
            with self.assertRaises(Refusal):c.drive(io.r,io)
            self.assertFalse(io.exists('diagnostic-hold.json'),mode);self.assertTrue(io.exists('stop-trigger.json'))
        io=self.metadata_io(self.OP4_LIKE);io.startup_bad=True;io.started_never=True
        real=io.info
        io.info=lambda:dict(real(),state='stopped') if io.started else real()
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertFalse(io.exists('diagnostic-hold.json'))
        # A handoff whose admission failed is withdrawn, so the refusal holds and stops.
        r=fixture();r.update(useful_job_ready=True,separate_Root_handoff={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':7200})
        io=InertIO(r);io.diagnose=lambda s:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        io.guard_for_admission=lambda:(_ for _ in ()).throw(Refusal('guard not live at handoff'))
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertTrue(io.exists('root-handoff-withdrawn.json'));self.assertTrue(io.exists('diagnostics.json'));self.assertTrue(io.exists('stop-trigger.json'))
        q=io.read('admission-refused.json');self.assertEqual((q['phase'],q['reason_code']),('admission','refusal:guard not live at handoff'))
        # A standing handoff, with its admission receipt, never holds.
        io=InertIO(r);io.diagnose=lambda s:self.fail('hold after handoff');io.guard_for_admission=lambda:InertIO.guard_for_admission(io)
        self.assertEqual(c.drive(r,io),'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE')
        c.diagnostic_hold(io,io.read('controller-intent.json'));self.assertFalse(io.exists('diagnostic-hold.json'))
        io.record('root-handoff-withdrawn.json',{});c.diagnostic_hold(io,io.read('controller-intent.json'));self.assertFalse(io.exists('diagnostic-hold.json'))
        # The hold has one call site, the controller's qualification.
        callers=[f.name for f in ast.walk(ast.parse(Path(c.__file__).read_text())) if isinstance(f,ast.FunctionDef) and any(isinstance(n,ast.Call) and getattr(n.func,'id',None)=='diagnostic_hold' for n in ast.walk(f))]
        self.assertEqual(callers,['drive'])
    def test_an_unbound_handoff_records_an_admission_refusal(self):
        r=fixture();r.update(useful_job_ready=True,separate_Root_handoff={})
        io=InertIO(r);io.diagnose=lambda s:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        with self.assertRaises(Refusal):c.drive(r,io)
        self.assertEqual(io.read('admission-refused.json')['reason_code'],'refusal:unbound useful job handoff');self.assertTrue(io.exists('diagnostics.json'))
    def test_a_stopcheck_refusal_keeps_its_remote_reason(self):
        class StopIO(PhaseIO):
            stop_quiescent=c.RealIO.stop_quiescent
        io=StopIO(fixture());io.refusal_body={'startup_closed':False,'phase_closed':True,'reason_code':'unit_unhealthy:memory_peak_unavailable','terminal_properties':{'LoadState':'not-found','Result':'success','ExecMainStatus':'0'}}
        with self.assertRaises(Refusal):io.stop_quiescent()
        q=io.read('stopcheck-refused.json')
        self.assertEqual((q['reason_code'],q['remote_reason_code']),('refusal:current startup/installer closure unavailable before stop','unit_unhealthy:memory_peak_unavailable'))
        self.assertEqual(q['terminal']['LoadState'],'not-found')
        # The first refusal of a phase is kept, and recording never raises.
        io.refusal_body=dict(io.refusal_body,reason_code='unreported')
        with self.assertRaises(Refusal):io.stop_quiescent()
        self.assertEqual(io.read('stopcheck-refused.json'),q)
        broken=StopIO(fixture());broken.record=lambda n,v:(_ for _ in ()).throw(OSError(28,'full'))
        c.record_refusal(broken,'stopcheck',Refusal('x'));c.record_refusal(io,'unknown',Refusal('x'));self.assertFalse(io.exists('unknown-refused.json'))
    def test_the_diagnostic_program_reads_only_its_own_results(self):
        spec=importlib.util.spec_from_file_location('diagnostic_probe',Path(__file__).with_name('diagnostic-probe.py'));d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
        with tempfile.TemporaryDirectory() as t:
            home=Path(t);state=home/'.local/state/sparkles-boat';(home/'.config/mbx').mkdir(parents=True);state.mkdir(parents=True)
            token=home/'.config/mbx/remote-token';token.write_text('SECRET-TOKEN-CONTENT');os.chmod(token,0o600)
            (state/'toolchain.json').write_text('{"tool":"SECRET-TOOL-CONTENT"}')
            def phase(name,body,mode=0o600):
                body=dict(body,phase='metadata',program_sha256='a'*64)
                (state/name).mkdir();f=state/name/'probe-result.json';f.write_text(json.dumps(body));os.chmod(f,mode)
            phase('qualification-a-metadata',{'qualified':False,'reason_code':'refusal:insufficient host headroom for proposed12GiB job'})
            phase('qualification-a-startup',{'startup_closed':True},0o644)
            phase('qualification-b-startup',{'x':'y'*(d.RESULT_MAX+10)})
            (state/'qualification-c-stopcheck').mkdir()
            with patch.object(d,'units',return_value={'rows':[]}):body=d.collect(home,os.getuid())
            text=json.dumps(body);self.assertNotIn('SECRET',text);self.assertLess(len(text),d.OUTPUT_MAX)
            phases=body['state']['phases']
            self.assertEqual(phases['qualification-a-metadata']['result']['reason_code'],'refusal:insufficient host headroom for proposed12GiB job')
            self.assertEqual(phases['qualification-a-startup']['unread'],'not a private regular file');self.assertEqual(phases['qualification-b-startup']['unread'],'over the result bound')
            self.assertEqual(phases['qualification-c-stopcheck'],{'entries':[]})
            self.assertIn('toolchain.json',[e['name'] for e in body['state']['entries']]);self.assertIn('MemTotal',body['memory_bytes'])
            self.assertTrue(any(r['pid']==os.getpid() for r in body['processes']['rows']))
            # Results are dropped, largest first, until the output fits.
            for n in range(20):phase('qualification-z%02d-metadata'%n,{'x':'y'*60000})
            with patch.object(d,'units',return_value={'rows':[]}):body=d.collect(home,os.getuid())
            self.assertLessEqual(len(json.dumps(body)),d.OUTPUT_MAX);self.assertIn('result',body['state']['phases']['qualification-a-metadata'])
            self.assertTrue(any(v.get('unread')=='dropped to fit the output bound' for v in body['state']['phases'].values()))

    def test_diagnostics_never_follow_a_redirected_state_root_or_result(self):
        spec=importlib.util.spec_from_file_location('diagnostic_probe',Path(__file__).with_name('diagnostic-probe.py'));d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
        body={'phase':'metadata','program_sha256':'a'*64,'secret':'REPOSITORY-CONTENT'}
        with tempfile.TemporaryDirectory() as t:
            home=Path(t)/'home';repo=Path(t)/'repo';(home/'.local/state').mkdir(parents=True);(repo/'qualification-x-metadata').mkdir(parents=True)
            f=repo/'qualification-x-metadata/probe-result.json';f.write_text(json.dumps(body));os.chmod(f,0o600)
            (home/'.local/state/sparkles-boat').symlink_to(repo)
            with patch.object(d,'units',return_value={'rows':[]}):out=d.collect(home,os.getuid())
            self.assertFalse(out['state']['present']);self.assertNotIn('REPOSITORY',json.dumps(out))
            # A phase directory or a result that is a link is not followed either.
            (home/'.local/state/sparkles-boat').unlink();state=home/'.local/state/sparkles-boat';state.mkdir()
            (state/'qualification-y-metadata').symlink_to(repo/'qualification-x-metadata')
            (state/'qualification-z-metadata').mkdir();(state/'qualification-z-metadata/probe-result.json').symlink_to(f)
            (state/'qualification-w-metadata').mkdir();g=state/'qualification-w-metadata/probe-result.json';g.write_text(json.dumps({'not':'a phase result'}));os.chmod(g,0o600)
            with patch.object(d,'units',return_value={'rows':[]}):out=d.collect(home,os.getuid())
            phases=out['state']['phases'];self.assertNotIn('REPOSITORY',json.dumps(out))
            self.assertIn('unread',phases['qualification-y-metadata']);self.assertIn('unread',phases['qualification-z-metadata']);self.assertEqual(phases['qualification-w-metadata']['unread'],'not a phase result')
            # A world-writable state directory is refused.
            os.chmod(state,0o777)
            with patch.object(d,'units',return_value={'rows':[]}):out=d.collect(home,os.getuid())
            self.assertFalse(out['state']['present'])
    def test_a_failed_directory_inspection_closes_its_descriptor(self):
        spec=importlib.util.spec_from_file_location('diagnostic_probe',Path(__file__).with_name('diagnostic-probe.py'));d=importlib.util.module_from_spec(spec);spec.loader.exec_module(d)
        with tempfile.TemporaryDirectory() as t:
            (Path(t)/'x').mkdir();parent=os.open(t,os.O_RDONLY|os.O_DIRECTORY);opened=[];real_open=os.open
            def tracked(*a,**k):
                fd=real_open(*a,**k);opened.append(fd);return fd
            try:
                with patch.object(d.os,'open',side_effect=tracked),patch.object(d.os,'fstat',side_effect=OSError(5,'io')):
                    with self.assertRaises(OSError):d.owned_directory('x',parent,os.getuid())
                self.assertEqual(len(opened),1)
                with self.assertRaises(OSError):os.fstat(opened[0])
            finally:os.close(parent)

    def test_diagnostics_receipt_fits_its_bound_and_trimming_ends(self):
        result={'phase':'metadata','program_sha256':'a'*64,'directories':[{'path':'/home/user/x','uid':1000,'mode':448,'device':1,'inode':n} for n in range(8)]}
        body={'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{'qualification-%04d'%n:{'entries':['probe-result.json'],'result':copy.deepcopy(result)} for n in range(1200)}}}
        fitted=c.fit_diagnostics(copy.deepcopy(body));self.assertLessEqual(len(json.dumps(fitted,sort_keys=True,indent=2)),c.RECEIPT_MAX)
        self.assertTrue(any('result' in v for v in fitted['state']['phases'].values()))
        # A body with nothing left to trim refuses instead of looping.
        huge={'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}},'units':{'rows':[{'unit':'x'*128}]*20000}}
        with self.assertRaises(Refusal):c.fit_diagnostics(huge)
        with self.assertRaises(Refusal):c.fit_diagnostics({'schema':1})
        io=self.metadata_io(self.OP4_LIKE);io.diagnose=lambda s:copy.deepcopy(body)
        with self.assertRaises(Refusal):c.drive(io.r,io)
        self.assertLessEqual(len(json.dumps(io.read('diagnostics.json'),sort_keys=True,indent=2)),1024*1024)
    def test_every_startup_refusal_uses_the_shared_record(self):
        intent={'mono':100,'wall':1800000000.}
        io=PhaseIO(fixture());io.phase=lambda phase,seconds=0:(_ for _ in ()).throw(Refusal('remote phase invocation failed'))
        with self.assertRaises(Refusal):io.startup(intent,True)
        self.assertEqual(io.read('startup-refused.json')['reason_code'],'refusal:remote phase invocation failed')
        io=PhaseIO(fixture());io.bad_manifest=True
        with self.assertRaises(Refusal):io.startup(intent,True)
        q=io.read('startup-refused.json');self.assertEqual(q['reason_code'],'refusal:allowed startup produced unadopted manifest');self.assertTrue(q['phase_closed'])
    def test_refusal_evidence_keeps_the_units_own_peaks(self):
        q=c.phase_evidence({'terminal_evidence':{'source':'cgroup','memory_peak':1,'swap_peak':0,'unit_MemoryPeak':MEMORY+1,'unit_MemorySwapPeak':0,'terminal_source':'unit'}})
        self.assertEqual(q['evidence']['terminal_evidence'],{'source':'cgroup','memory_peak':1,'swap_peak':0,'unit_MemoryPeak':MEMORY+1,'unit_MemorySwapPeak':0,'terminal_source':'unit'})
        self.assertIsNone(c.phase_evidence({'terminal_evidence':{'unit_MemoryPeak':'SECRET'}})['evidence']['terminal_evidence']['unit_MemoryPeak'])

    def test_final_runtime_source_map(self):
        path=Path(__file__).with_name('runtime-sources.json')
        self.assertTrue(path.exists());c.verify_packet(digest(path.read_bytes()))
        with self.assertRaises(Refusal):c.verify_packet('0'*64)
    def test_no_forbidden_provider_commands_in_driver(self):
        r=fixture();io=InertIO(r);c.drive(r,io);c.guard_loop(r,io,io.read('controller-intent.json'))
        self.assertTrue(all(x[0] in ['resume','stop'] for x in io.calls));self.assertTrue(all('--no-auto-stop' not in x and '--force' not in x for x in io.calls))

if __name__=='__main__':
    sys.addaudithook(lambda event,args: (_ for _ in ()).throw(RuntimeError('NO CHILD/NETWORK')) if event in ('subprocess.Popen','os.system','os.posix_spawn','socket.connect','socket.bind','socket.getaddrinfo') else None)
    resource.setrlimit(resource.RLIMIT_AS,(512*1024*1024,512*1024*1024));resource.setrlimit(resource.RLIMIT_CPU,(40,40));resource.setrlimit(resource.RLIMIT_FSIZE,(2*1024*1024,2*1024*1024));signal.alarm(45)
    with patch.object(c.subprocess,'Popen',side_effect=AssertionError('NO REAL CHILD/PROVIDER/MANAGER')):
        suite=unittest.defaultTestLoader.loadTestsFromTestCase(Controls);result=unittest.TextTestRunner(verbosity=2).run(suite)
    report={'tests':result.testsRun,'failures':len(result.failures),'errors':len(result.errors),'subprocess_provider_manager_network_secretcontent_agent_actions':0,'fixtures':'temporary private ASCII metadata only; no real credential/source/runtime fixtures','owned_children_started':0,'resource_limits':{'AS':536870912,'CPU_seconds':40,'wall_seconds':45},'all_tests_successful':result.wasSuccessful()}
    print(json.dumps(report));raise SystemExit(0 if result.wasSuccessful() else 1)
