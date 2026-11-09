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
    def __init__(self,r):super().__init__(r);self.phase_calls=[];self.pending=False;self.bad_manifest=False;self.delay=0
    def phase(self,phase,seconds=0):
        self.phase_calls.append((phase,seconds))
        if self.delay:self.wait(self.delay)
        return {'startup_closed':not self.pending,'phase_closed':not self.pending,'qualification_eligible':not self.bad_manifest,'bootstrap_occurrence':'unknown'}

class Clock:
    def __init__(self):self.t=0
    def now(self):return self.t
    def pause(self,n):self.t+=n

def proc_fixture():
    return {1:{'pid':1,'ppid':0,'uid':0,'comm':'systemd','start':1,'group':1},10:{'pid':10,'ppid':1,'uid':0,'comm':'sshd','start':1,'group':10},11:{'pid':11,'ppid':10,'uid':1000,'comm':'python3','start':1,'group':11},12:{'pid':12,'ppid':1,'uid':1000,'comm':'python3','start':1,'group':12},13:{'pid':13,'ppid':11,'uid':1000,'comm':'systemd-run','start':1,'group':11}}

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
    def test_snapshot_failure_never_shutdown_success_or_retry(self):
        r=fixture();io=InertIO(r);io.started=True;io.stopped_bad=True
        self.assertFalse(c.reconcile_stop(r,io));self.assertFalse(io.exists('closed.json'));self.assertTrue(io.exists('unresolved.json'));self.assertEqual([x[0] for x in io.calls],['stop'])
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
    def test_live_installer_never_checkpoint_success(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-live-installer-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600);rows=proc_fixture();rows[20]={'pid':20,'ppid':1,'uid':1000,'comm':'bash','start':1,'group':20};clock=Clock()
            q=OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,lambda:rows,clock.now,clock.pause)
            self.assertFalse(q['startup_closed']);self.assertFalse(q['qualification_eligible'])
    def test_manifest_replaced_during_checkpoint_refused(self):
        with tempfile.TemporaryDirectory(prefix='boat-v2-manifest-race-') as d:
            root=Path(d);(root/'bootstrap.lock').touch(mode=0o600);m=root/'toolchain.json';m.write_text('ASCII');m.chmod(0o600);clock=Clock();calls=[0]
            def rows():
                calls[0]+=1
                if calls[0]==2:m.unlink();m.write_text('CHANGED');m.chmod(0o600)
                return proc_fixture()
            with self.assertRaises(OBSERVER['Refusal']):OBSERVER['checkpoint_at'](root,os.getuid(),(-1,-1),{'pid':11,'start':1},12,digest(m.read_bytes()),2,rows,clock.now,clock.pause)
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
