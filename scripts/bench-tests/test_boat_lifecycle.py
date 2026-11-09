"""Finite source integration controls; no provider or manager operation runs."""
from pathlib import Path
import copy
import ctypes
import hashlib
import json
import os
import sys
import tempfile
import unittest
from unittest.mock import patch
from boat_fixtures import fixture
sys.path.insert(0,str(Path(__file__).resolve().parents[1]/'boat'))
import controller
import job_environment as env
from config_schema import validate_profile


class EnvironmentAdmission(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.f=fixture(self.root)
        import qualified_worker_admission as qwa
        # The fixture's BOOTTIME observation stands for the current boot clock.
        boot=patch.object(qwa,'boottime',return_value=self.f['boot_now']);boot.start();self.addCleanup(boot.stop)

    def load(self):
        return env.load_job_admission(self.f['admission'],self.f['profile_path'],self.f['profile']['worker'],self.f['now'],lambda unit:self.f['guard'])

    def test_exact_original_qualification_profile_source_job_and_environment(self):
        p,q,file,cli,life=self.load()
        self.assertEqual(q,self.f['qualified']);self.assertEqual(file,env.expected_environment(p,q))
        self.assertEqual(cli,self.f['release']['boat'])
        self.assertEqual(life['boot_id'],self.f['boot_id']);self.assertEqual(life['invocation_id'],self.f['qualified']['local_guard']['invocation_id'])
        original=self.f['qualified_path'].read_bytes()
        self.assertEqual(hashlib.sha256(original).hexdigest(),self.f['job']['qualification']['sha256'])
        self.assertIsNone(json.loads(original)['runtime_env_file'])
        self.assertEqual(set(env.environment(p)),{'PATH','RUSTUP_TOOLCHAIN','RUSTUP_AUTO_INSTALL','MBX_REMOTE_TOKEN_FILE'})

    def test_changed_qualification_bytes_or_env_proof_does_not_grant_authority(self):
        original=self.f['qualified_path'].read_bytes()
        self.f['qualified_path'].write_bytes(original+b' ')
        with self.assertRaises(ValueError):self.load()
        self.f['qualified_path'].write_bytes(original)
        changed=copy.deepcopy(self.f['job']);changed['runtime_env_file']['sha256']='0'*64
        self.f['admission'].write_text(json.dumps(changed))
        with self.assertRaises(ValueError):self.load()

    def test_profile_cache_bindings_are_private_explicit_and_nonsecret(self):
        p=copy.deepcopy(self.f['profile'])
        for cache in ({'url':'https://user:secret@cache.invalid','namespace':'fixture/dev','mode':'read-only'},
                      {'url':'https://cache.invalid','namespace':'fixture/dev','mode':'write'},
                      {'url':None,'namespace':None,'mode':'read-only'}):
            p['mbx_remote_config']=cache
            with self.assertRaises(ValueError):validate_profile(p)

    def test_prepare_preserves_original_bytes_and_checks_again_after_transport(self):
        original=self.f['qualified_path'].read_bytes();out=self.f['operation']/'prepared.json'
        expected=env.expected_environment(self.f['profile'],self.f['qualified'])
        real_context=env.context
        def context(*args,**kwargs):
            kwargs['guard_reader']=lambda unit:self.f['guard']
            return real_context(*args,**kwargs)
        with patch.object(env,'context',side_effect=context)as check,patch.object(env.time,'time',return_value=self.f['now']),patch.object(controller,'bounded',return_value={'exit':0,'body':expected})as wire:
            env.prepare(self.f['profile_path'],self.f['operation'],self.f['profile']['worker'],out)
            self.assertEqual(check.call_count,2);self.assertEqual(wire.call_count,1)
        self.assertEqual(self.f['qualified_path'].read_bytes(),original)
        record=json.loads(out.read_bytes());self.assertEqual(record['qualification']['sha256'],hashlib.sha256(original).hexdigest())
        self.assertEqual(out.stat().st_mode&0o777,0o600)

    def test_prepare_window_uses_original_boottime_after_wall_rollback(self):
        import qualified_worker_admission as qwa
        from datetime import datetime
        out=self.f['operation']/'prepared-rollback.json';original=self.f['qualified_path'].read_bytes()
        left=datetime.fromisoformat(self.f['qualified']['expires_at'].replace('Z','+00:00')).timestamp()-self.f['now']
        expected=env.expected_environment(self.f['profile'],self.f['qualified'])
        real_context=env.context
        def context(*args,**kwargs):
            kwargs['guard_reader']=lambda unit:self.f['guard'];return real_context(*args,**kwargs)
        # Forty original seconds remain by BOOTTIME although wall time moved back.
        with patch.object(env,'context',side_effect=context),patch.object(env.time,'time',return_value=self.f['now']-7200),\
             patch.object(qwa,'boottime',return_value=self.f['boot_now']+left-40,create=True),\
             patch.object(qwa,'current_boot_id',return_value=self.f['boot_id'],create=True),\
             patch.object(controller,'bounded',return_value={'exit':0,'body':expected})as wire:
            env.prepare(self.f['profile_path'],self.f['operation'],self.f['profile']['worker'],out)
        self.assertLessEqual(wire.call_args.args[1],10)
        self.assertEqual(self.f['qualified_path'].read_bytes(),original)
        with patch.object(env,'context',side_effect=context),patch.object(env.time,'time',return_value=self.f['now']-7200),\
             patch.object(qwa,'boottime',return_value=self.f['boot_now']+left-20,create=True),\
             patch.object(qwa,'current_boot_id',return_value=self.f['boot_id'],create=True),\
             patch.object(controller,'bounded',side_effect=AssertionError('no transport')):
            with self.assertRaises(ValueError):
                env.prepare(self.f['profile_path'],self.f['operation'],self.f['profile']['worker'],self.f['operation']/'late.json')

    def test_preparation_transport_stops_when_suspend_consumes_the_original_end(self):
        import qualified_worker_admission as qwa,sys,time
        from datetime import datetime
        left=datetime.fromisoformat(self.f['qualified']['expires_at'].replace('Z','+00:00')).timestamp()-self.f['now']
        real_context=env.context;real_bounded=controller.bounded;boot=[self.f['boot_now']]
        def context(*args,**kwargs):
            kwargs['guard_reader']=lambda unit:self.f['guard'];return real_context(*args,**kwargs)
        def transport(argv,seconds,*rest,**kwargs):
            # The suspend happens once the preparation is in flight.
            boot[0]=self.f['boot_now']+left-25
            return real_bounded([sys.executable,'-c','import time;time.sleep(20)'],seconds,None,**kwargs)
        start=time.monotonic()
        with patch.object(env,'context',side_effect=context),patch.object(env.time,'time',return_value=self.f['now']),\
             patch.object(qwa,'boottime',side_effect=lambda:boot[0]),patch.object(qwa,'current_boot_id',return_value=self.f['boot_id'],create=True),\
             patch.object(controller,'bounded',side_effect=transport):
            with self.assertRaises(ValueError):
                env.prepare(self.f['profile_path'],self.f['operation'],self.f['profile']['worker'],self.f['operation']/'suspended.json')
        self.assertLess(time.monotonic()-start,8)

    def test_preparation_refuses_publication_inside_the_reserve(self):
        import qualified_worker_admission as qwa
        from datetime import datetime
        left=datetime.fromisoformat(self.f['qualified']['expires_at'].replace('Z','+00:00')).timestamp()-self.f['now']
        expected=env.expected_environment(self.f['profile'],self.f['qualified'])
        real_context=env.context;boot=[self.f['boot_now']];out=self.f['operation']/'late-publication.json'
        def context(*args,**kwargs):
            kwargs['guard_reader']=lambda unit:self.f['guard'];return real_context(*args,**kwargs)
        def transport(*args,**kwargs):
            boot[0]=self.f['boot_now']+left-25;return {'exit':0,'error':None,'body':expected}
        with patch.object(env,'context',side_effect=context),patch.object(env.time,'time',return_value=self.f['now']),\
             patch.object(qwa,'boottime',side_effect=lambda:boot[0]),patch.object(controller,'bounded',side_effect=transport):
            with self.assertRaises(ValueError):env.prepare(self.f['profile_path'],self.f['operation'],self.f['profile']['worker'],out)
        self.assertFalse(out.exists())

    def test_reviewed_rollback_scenario_leaves_1570_seconds(self):
        import qualified_worker_admission as qwa
        intent={'deadline_wall':11500,'deadline_mono':10600};handoff={'deadline_wall':11500}
        guard={'observed_wall':1100,'observed_boottime':200}
        wall,boot=qwa.original_ends(intent,guard,handoff);self.assertEqual((wall,boot),(11500,10600))
        life={'wall_end':wall,'boot_end':boot,'boot_id':'b','invocation_id':'a'*32}
        self.assertEqual(qwa.lifetime_left(life,2700,9000,'b')-qwa.CLOSURE_RESERVE,1570)
        self.assertEqual(qwa.lifetime_left(life,9900,9000,'b')-qwa.CLOSURE_RESERVE,1570)
        with self.assertRaises(ValueError):qwa.lifetime_left(life,2700,9000,'other boot')

    def test_remote_environment_creation_and_check_never_open_token_content(self):
        home=self.root/'worker';state=home/'.local/state/sparkles-boat';state.mkdir(parents=True)
        token=home/'.config/mbx/remote-token';token.parent.mkdir(parents=True);token.write_bytes(b'INERT ASCII METADATA FIXTURE');token.chmod(0o600)
        p=copy.deepcopy(self.f['profile']);p['worker_home']=str(home)
        q=self.f['qualified'];file=env.expected_environment(p,q)
        payload={'home':str(home),'uid':os.getuid(),'file':file,'body':env.environment_body(p).decode(),'check':False}
        real_open=os.open;opened=[]
        def safe_open(path,*args,**kwargs):
            self.assertNotEqual(Path(path),token,'token content open attempted')
            opened.append(str(path));return real_open(path,*args,**kwargs)
        with patch.object(Path,'home',return_value=home),patch.object(sys,'argv',['fixture',json.dumps(payload)]),patch.object(os,'open',side_effect=safe_open),patch('builtins.print'):
            exec(compile(env.REMOTE,'inert-job-environment','exec'),{'__name__':'inert'})
        path=Path(file['path']);self.assertEqual(path.read_bytes(),env.environment_body(p));self.assertEqual(path.stat().st_mode&0o777,0o600)
        before=path.stat().st_ino;payload['check']=True
        with patch.object(Path,'home',return_value=home),patch.object(sys,'argv',['fixture',json.dumps(payload)]),patch.object(os,'open',side_effect=safe_open),patch('builtins.print'):
            exec(compile(env.REMOTE,'inert-job-environment-check','exec'),{'__name__':'inert'})
        self.assertEqual(path.stat().st_ino,before);self.assertNotIn(str(token),opened)

    def test_false_profile_refuses_before_transport(self):
        false=Path(env.__file__).with_name('profile.FALSE.json')
        with patch.object(controller,'bounded',side_effect=AssertionError('no child/provider/manager')):
            with self.assertRaises(ValueError):env.prepare(false,self.f['operation'],'bx_fixture',self.root/'none.json')


class ControllerTransportCleanup(unittest.TestCase):
    def test_owned_local_timeout_stops_descendant_after_leader_exit(self):
        import signal,time
        libc=ctypes.CDLL(None);previous=ctypes.c_int();self.assertEqual(libc.prctl(37,ctypes.byref(previous),0,0,0),0);self.assertEqual(libc.prctl(36,1,0,0,0),0)
        self.addCleanup(lambda:libc.prctl(36,previous.value,0,0,0))
        with tempfile.TemporaryDirectory()as tmp:
            pidfile=Path(tmp)/'child.pid';created=[];actual=controller.subprocess.Popen
            def launch(*a,**kw):
                p=actual(*a,**kw);created.append(p);return p
            try:
                with patch.object(controller.subprocess,'Popen',side_effect=launch):
                    result=controller.bounded(['bash','-c',"(trap '' TERM; exec sleep 5) & echo $! > "+str(pidfile)+"; exit 0"],0.04)
                self.assertEqual(result['error'],'timeout');child=int(pidfile.read_text());self.assertFalse(Path('/proc',str(child)).exists());end=time.monotonic()+1
                while Path('/proc',str(child)).exists():
                    try:state=Path('/proc',str(child),'stat').read_text().split(') ',1)[1].split()[0]
                    except (FileNotFoundError,ProcessLookupError):break
                    if state=='Z':break
                    self.assertLess(time.monotonic(),end);time.sleep(0.01)
            finally:
                if created:
                    try:os.killpg(created[0].pid,signal.SIGKILL)
                    except ProcessLookupError:pass

    def test_limit_exhausted_by_completion_is_not_success(self):
        import sys
        with tempfile.TemporaryDirectory()as tmp:
            marker=Path(tmp)/'done'
            # Output closes first, so the outer end passes during the final leader wait.
            child="import os,time,pathlib;os.close(1);time.sleep(0.3);pathlib.Path("+repr(str(marker))+").write_text('1')"
            result=controller.bounded([sys.executable,'-c',child],5,None,limit=lambda:-1 if marker.exists() else 5)
            self.assertEqual(result,{'exit':None,'error':'timeout','body':None})
            result=controller.bounded([sys.executable,'-c','print(1)'],5,None,limit=lambda:-1)
            self.assertEqual(result['error'],'timeout')

    def test_success_and_failure_retire_silent_descendants(self):
        import signal,sys
        libc=ctypes.CDLL(None);previous=ctypes.c_int()
        self.assertEqual(libc.prctl(37,ctypes.byref(previous),0,0,0),0)
        self.assertEqual(libc.prctl(36,1,0,0,0),0)
        self.addCleanup(lambda:libc.prctl(36,previous.value,0,0,0))
        with tempfile.TemporaryDirectory()as tmp:
            for code in (0,17):
                path=Path(tmp)/str(code)
                script="import os,signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);open("+repr(str(path))+",'w').write(str(os.getpid()));time.sleep(5)"
                parent="import subprocess,time,sys,pathlib;p=subprocess.Popen([sys.executable,'-c',"+repr(script)+"],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);f=pathlib.Path("+repr(str(path))+");end=time.monotonic()+1\nwhile not f.exists() and time.monotonic()<end:time.sleep(.001)\nprint('{}');raise SystemExit("+str(code)+")"
                result=controller.bounded([sys.executable,'-c',parent],2)
                self.assertEqual(result,{'exit':code,'error':None,'body':{}})
                self.assertFalse(Path('/proc',path.read_text()).exists())


def metadata_probe():
    import importlib.util
    spec=importlib.util.spec_from_file_location('metadata_probe',Path(__file__).resolve().parents[1]/'boat'/'metadata-probe.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module


# Recorded on bx_gy2ytgh8 at 2026-11-09T03:12Z (qualification/facts-1.json, facts-2.txt).
# Only anon, file and inactive_file of user.slice were recorded. The other memory.stat
# fields are a conservative assumption: all remaining charge is non-reclaimable kernel.
OBSERVED_USER_SLICE_MAX=16230907904
OBSERVED_USER_SLICE_CURRENT=7504871424
OBSERVED_ANON=140820480
OBSERVED_FILE=6671233024
OBSERVED_INACTIVE_FILE=6345027584
OBSERVED_MEMINFO={'MemTotal':16375784,'MemFree':3600000,'MemAvailable':14003500,'Buffers':398148,'Cached':9667500,
                  'AnonPages':1700000,'Shmem':1200,'Unevictable':0,'KernelStack':20000,'PageTables':30000,
                  'SecPageTables':0,'Percpu':10000,'SReclaimable':400000,'SUnreclaim':150000,'SwapTotal':2097148,'SwapFree':2097148}


def meminfo_text(values):
    return ''.join(f'{k}: {v} kB\n' for k,v in values.items())


def cgroup_stat(anon,file,inactive_file,shmem=0,unevictable=0,kernel=None,slab_reclaimable=0,current=None):
    kernel=(current-anon-file) if kernel is None else kernel
    rows={'anon':anon,'file':file,'kernel':kernel,'shmem':shmem,'file_mapped':0,'inactive_file':inactive_file,
          'active_file':file-inactive_file,'unevictable':unevictable,'slab_reclaimable':slab_reclaimable,'slab_unreclaimable':kernel-slab_reclaimable}
    return '\n'.join(f'{k} {v}' for k,v in rows.items())


class MemoryAdmission(unittest.TestCase):
    def ancestors(self,**kw):
        current=kw.pop('current',OBSERVED_USER_SLICE_CURRENT)
        stat=cgroup_stat(kw.pop('anon',OBSERVED_ANON),kw.pop('file',OBSERVED_FILE),kw.pop('inactive_file',OBSERVED_INACTIVE_FILE),current=current,**kw)
        return [{'path':'/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service','memory.max':'max','memory.current':'122740736'},
                {'path':'/sys/fs/cgroup/user.slice/user-1000.slice','memory.max':'max','memory.current':'388714496'},
                {'path':'/sys/fs/cgroup/user.slice','memory.max':str(OBSERVED_USER_SLICE_MAX),'memory.current':str(current),'memory.stat':stat},
                {'path':'/sys/fs/cgroup'}]

    def test_observed_worker_with_page_cache_admits(self):
        mp=metadata_probe()
        memory=mp.memory_admission(meminfo_text(OBSERVED_MEMINFO),self.ancestors())
        self.assertEqual(memory['MemTotal'],OBSERVED_MEMINFO['MemTotal']*1024)

    def test_anon_full_worker_refuses(self):
        mp=metadata_probe();anon=8*2**30;file=500*2**20
        with self.assertRaises(mp.Refusal):
            mp.memory_admission(meminfo_text(OBSERVED_MEMINFO),self.ancestors(anon=anon,file=file,inactive_file=file,current=anon+file+200*2**20))
        host=dict(OBSERVED_MEMINFO,AnonPages=9*2**20,Cached=200000,MemAvailable=6000000)
        with self.assertRaises(mp.Refusal):mp.memory_admission(meminfo_text(host),self.ancestors())

    def test_shmem_unevictable_and_unreclaimable_kernel_count(self):
        mp=metadata_probe();gib=2**30
        # 2 GiB of tmpfs and 1 GiB of mlocked pages in the cache still leave too little room.
        for extra in ({'shmem':2*gib,'unevictable':0},{'shmem':0,'unevictable':2*gib},{'kernel':2*gib+100*2**20,'slab_reclaimable':0}):
            with self.subTest(extra=sorted(extra)),self.assertRaises(mp.Refusal):
                mp.memory_admission(meminfo_text(OBSERVED_MEMINFO),self.ancestors(anon=gib,**extra))
        # Reclaimable slab does not count.
        mp.memory_admission(meminfo_text(OBSERVED_MEMINFO),self.ancestors(kernel=2*gib,slab_reclaimable=2*gib-10*2**20))

    def test_missing_stat_on_a_limited_ancestor_refuses(self):
        mp=metadata_probe();rows=self.ancestors();rows[2].pop('memory.stat')
        with self.assertRaises(mp.Refusal):mp.memory_admission(meminfo_text(OBSERVED_MEMINFO),rows)


class Finish(unittest.TestCase):
    def test_finish_waits_for_original_guard_and_verified_stop_not_submission(self):
        with tempfile.TemporaryDirectory()as tmp:
            f=fixture(Path(tmp));r=f['release']
            worker=f['profile']['worker'];now=f['now']
            stop='stop_fixture'
            safe={'id':worker,'access':'owner','environment':f['profile']['future_environment']['name'],
                  'state':'stopped','type':'large','vcpu':8,'memoryGB':16,'snapshotAvailable':True,'lastSnapshotStatus':'completed',
                  'snapshotCompletedAt':'2027-01-15T08:01:00Z',
                  'stoppedAt':'2027-01-15T08:01:00Z','stop':{'id':stop,'status':'completed','error':None,'forced':False,'endedAt':'2027-01-15T08:01:00Z'}}
            # Keep source fixture clocks consistent; no real provider timestamp is asserted.
            from datetime import datetime,timezone
            iso=lambda t:datetime.fromtimestamp(t,timezone.utc).isoformat().replace('+00:00','Z')
            safe.update(snapshotCompletedAt=iso(now+2),stoppedAt=iso(now+2));safe['stop']['endedAt']=iso(now+2)
            closed={'worker':worker,'stop_operation':stop,'safe_info':safe,'stopped_confirmed':True,'owned_metadata_closed':True,'guard_may_exit':True}
            job={'observed_unix':now,'verified_by_Root':True,'receipt_sha256':'c'*64,'worker':worker,
                 'operation_id':r['guard_unit'],'local_guard':f['qualified']['local_guard'],
                 'job_release_sha256':'a'*64,'all_native_and_leases_closed':True}
            class IO:
                def __init__(self,*a):self.t=0;self.triggered=0;self.records={'root-handoff.json':{'separate_job_release':r['separate_Root_handoff']},'qualified-worker-admission.json':f['qualified'],'root-job-closed.json':job,'guard-loaded.json':{'boot_id':f['qualified']['local_guard']['boot_id'],'loaded':{'InvocationID':'a'*32}},'stop-intent.json':{'requested_wall':now+1}}
                def wall(self):return now
                def mono(self):return self.t
                def read(self,n):return self.records[n]
                def exists(self,n):return n in self.records
                def trigger(self):self.triggered+=1
                def wait(self,n):self.t+=1;self.records['closed.json']=closed
                def show(self,u):return {'Id':u,'InvocationID':'a'*32,'ActiveState':'inactive','MainPID':'0','ExecMainStatus':'0','ControlGroup':''}
            io=IO()
            with patch.object(controller,'RealIO',return_value=io):
                result=controller.finish_operation(f['operation'],seconds=3)
            self.assertTrue(result['stopped_confirmed']);self.assertTrue(result['guard_closed']);self.assertEqual(io.triggered,1)
            io=IO();io.records['guard-loaded.json']['boot_id']='replaced-boot'
            with patch.object(controller,'RealIO',return_value=io),self.assertRaises(controller.Refusal):
                controller.finish_operation(f['operation'],seconds=3)
            io=IO();io.wait=lambda n:setattr(io,'t',io.t+1)
            with patch.object(controller,'RealIO',return_value=io),self.assertRaises(controller.Refusal):
                controller.finish_operation(f['operation'],seconds=2)

class BoundedFinalizerInterrupt(unittest.TestCase):
    def test_real_interrupts_finish_owned_closure_before_propagating(self):
        from finalizer_interrupt import run
        for leader,phase,count in (('timeout','term_wait',1),('timeout','kill',2),('0','term',1),('17','probe',1)):
            with self.subTest(leader=leader,phase=phase,count=count):
                outcome=run('bounded',phase,count,leader)
                self.assertEqual(outcome['fired'],[phase],outcome)
                self.assertEqual(outcome['exception'],'KeyboardInterrupt',outcome)
                self.assertEqual(outcome['cleanup_interrupted'],count,outcome)
                self.assertFalse(outcome['ready_alive_after_return'],outcome)
                self.assertTrue(outcome['foreign_alive'],outcome)
                self.assertTrue(all(outcome['streams_closed']),outcome)

    def injected(self,stage,leader_code=0):
        """Inject one KeyboardInterrupt at one teardown stage of fake children."""
        import selectors,signal
        events=[]
        class Stream:
            closed=False
            def close(self):events.append('stdout_close');self.closed=True
        class Child:
            pid=123;stdin=None;stdout=Stream();returncode=leader_code
            def wait(self,timeout=None):
                events.append(('wait',timeout is not None and timeout>=1))
                if stage=='wait' and len([e for e in events if e[0]=='wait'])==2:raise KeyboardInterrupt()
                return leader_code
            def poll(self):return leader_code
        class Select:
            def register(self,*a):pass
            def get_map(self):return False
            def close(self):events.append('selector_close')
        def kill(pid,sig):
            events.append(('killpg',pid,int(sig)))
            if stage=='term' and sig==signal.SIGTERM and events.count(('killpg',pid,int(sig)))==1:raise KeyboardInterrupt()
            if stage=='kill' and sig==signal.SIGKILL and events.count(('killpg',pid,int(sig)))==1:raise KeyboardInterrupt()
            if sig==0:
                if stage=='probe' and events.count(('killpg',123,0))==1:raise KeyboardInterrupt()
                raise ProcessLookupError()
        def waitpid(pid,flags):
            events.append(('waitpid',pid));raise ChildProcessError()
        with patch.object(controller.subprocess,'Popen',return_value=Child()),patch.object(controller.selectors,'DefaultSelector',return_value=Select()),patch.object(controller.os,'killpg',side_effect=kill),patch.object(controller.os,'waitpid',side_effect=waitpid):
            with self.assertRaises(KeyboardInterrupt)as raised:controller.bounded(['inert'],1)
        return events,raised.exception

    def test_injected_interrupt_at_each_stage_still_reaches_kill_and_group_proof(self):
        import signal
        for stage in ('term','wait','kill','probe'):
            for code in (0,17):
                with self.subTest(stage=stage,code=code):
                    events,error=self.injected(stage,code)
                    self.assertIn(('killpg',123,int(signal.SIGTERM)),events)
                    self.assertIn(('killpg',123,int(signal.SIGKILL)),events)
                    self.assertIn(('killpg',123,0),events)
                    self.assertIn(('waitpid',-123),events)
                    self.assertIn('selector_close',events);self.assertIn('stdout_close',events)
                    self.assertEqual(error.cleanup_interrupted,1)

    def resistant(self):
        import subprocess,sys
        child=subprocess.Popen([sys.executable,'-c','import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print(1,flush=True);time.sleep(5)'],stdout=subprocess.PIPE,start_new_session=True)
        self.addCleanup(lambda:child.poll() is not None or (child.kill(),child.wait()))
        self.assertEqual(child.stdout.read(2),b'1\n');return child

    def test_failed_prelude_and_custom_handler_keep_closure_and_run_once_after(self):
        import signal,subprocess,owned_group
        child=self.resistant();seen=[]
        def failing():
            signal.raise_signal(signal.SIGINT);raise OSError('restore failed')
        previous=signal.signal(signal.SIGINT,lambda n,f:seen.append(n))
        try:
            cancel=owned_group.Cancellation().install()
            problems,interrupted=owned_group.finish(child,(child.stdout,),(failing,),subprocess.TimeoutExpired,cancel)
            self.assertEqual(seen,[])
            owned_group.conclude(cancel,None)
        finally:signal.signal(signal.SIGINT,previous)
        self.assertEqual(child.returncode,-signal.SIGKILL);self.assertTrue(child.stdout.closed)
        self.assertIn('prelude OSError',problems);self.assertEqual(interrupted,1);self.assertEqual(seen,[signal.SIGINT])

    def test_interrupted_kill_and_close_are_attempted_again(self):
        import signal,subprocess,owned_group
        child=self.resistant();real=os.killpg;raised=[]
        def kill(pid,sig):
            if sig==signal.SIGKILL and not raised:
                raised.append(sig);raise KeyboardInterrupt()
            return real(pid,sig)
        class Stream:
            closed=False;calls=0
            def close(self):
                self.calls+=1
                if self.calls==1:raise KeyboardInterrupt()
                self.closed=True
        stream=Stream();cancel=owned_group.Cancellation().install()
        with patch.object(owned_group.os,'killpg',side_effect=kill):
            problems,interrupted=owned_group.finish(child,(child.stdout,stream),(),subprocess.TimeoutExpired,cancel)
        self.assertEqual(child.returncode,-signal.SIGKILL);self.assertEqual(problems,[])
        self.assertTrue(stream.closed);self.assertTrue(child.stdout.closed);self.assertEqual(interrupted,2)
        with self.assertRaises(KeyboardInterrupt)as error:owned_group.conclude(cancel,None,native_exit=child.returncode)
        self.assertEqual(error.exception.cleanup_interrupted,2)

    def test_interrupt_while_disarming_cannot_leave_immediate_delivery_armed(self):
        import signal,sys,time,owned_group
        source=Path(owned_group.__file__).read_text().splitlines()
        start=next(i for i,l in enumerate(source) if 'def interruptible' in l)
        line=next(i for i in range(start,len(source)) if source[i].strip()=='self.blocking = False')+1
        fired=[]
        def trace(frame,event,arg):
            if frame.f_code.co_filename==owned_group.__file__:
                if event=='line' and frame.f_lineno==line and not fired:
                    fired.append(line);os.kill(os.getpid(),signal.SIGINT)
                return trace
            return None
        previous=signal.signal(signal.SIGINT,signal.default_int_handler)
        cancel=owned_group.Cancellation().install()
        try:
            try:
                sys.settrace(trace)
                with cancel.interruptible():time.sleep(0.01)
            except KeyboardInterrupt:pass
            finally:sys.settrace(None)
            self.assertEqual(fired,[line]);self.assertFalse(cancel.blocking)
            os.kill(os.getpid(),signal.SIGINT)
            self.assertEqual(cancel.pending,1)
        except KeyboardInterrupt:
            self.fail('SIGINT escaped after an interrupted disarm')
        finally:
            cancel.restore();signal.signal(signal.SIGINT,previous)

    def test_missing_group_and_dead_leader_return_without_interrupt(self):
        import sys
        result=controller.bounded([sys.executable,'-c','print("{}")'],2)
        self.assertEqual(result,{'exit':0,'error':None,'body':{}})


if __name__=='__main__':unittest.main()
