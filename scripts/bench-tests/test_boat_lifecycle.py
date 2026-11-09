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
from boat_fixtures import fixture,ExtendIO,environment_fact,WORKER_BOOT_ID
import controls
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
        self.assertEqual(set(env.environment(p)),{'PATH','RUSTUP_TOOLCHAIN','RUSTUP_AUTO_INSTALL','MBX_REMOTE_URL','MBX_GC_MAX_TOTAL_SIZE','MBX_GC_MIN_FREE_SIZE'})

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
        # The test's own 'left' comes from the microsecond-rounded expires_at.
        self.assertLessEqual(wire.call_args.args[1],10+1e-3)
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


class LifetimeExtension(unittest.TestCase):
    """An explicit, receipted extension of one running qualified worker."""
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.f=fixture(self.root);controller.bind_release_profile(self.f['release'])
        import qualified_worker_admission as qwa;self.qwa=qwa
        self.op=self.f['operation'];self.intent=json.loads((self.op/'controller-intent.json').read_text())
        self.original=qwa.operation_lifetime(self.op,self.f['qualified'])
        self.left=self.original['wall_end']-self.f['now']

    def io(self,elapsed=0,wall_shift=0):
        return ExtendIO(self.f,self.f['now']+elapsed+wall_shift,self.f['boot_now']+elapsed)

    def extend(self,io,seconds=10500,accept=True,**fact):
        path=environment_fact(self.f,self.root/'environment-fact.json',io.wall(),**fact)
        with patch.object(self.qwa,'current_boot_id',return_value=io.local_boot):
            receipt=controller.extend_operation(self.f['release'],io,seconds,path,'second lane on the warm worker')
        # The running guard accepts the receipt on its next pass at the same instant.
        if accept:controller.guard_deadlines(ExtendIO(self.f,io.wall(),io.mono()),self.intent)
        return receipt

    def lifetime(self,boot_id=None):
        with patch.object(self.qwa,'current_boot_id',return_value=boot_id or self.f['boot_id']):
            return self.qwa.operation_lifetime(self.op,self.f['qualified'])

    def receipts(self):return sorted(n for n in os.listdir(self.op) if n.startswith('lifetime-extension-0') and not n.endswith('-accepted.json'))

    def test_a_withdrawn_handoff_is_never_extended(self):
        w=self.op/'root-handoff-withdrawn.json';w.write_text('{}\n');w.chmod(0o600)
        io=self.io(self.left-600)
        with self.assertRaises(Exception) as caught:self.extend(io,3600,accept=False)
        self.assertIn('no qualified Root handoff to extend',str(caught.exception));self.assertEqual(io.calls,[])

    def test_extension_is_a_receipted_act_with_a_new_bounded_end(self):
        io=self.io(self.left-600)
        receipt=self.extend(io,3600)
        self.assertEqual(io.calls,[['extend',self.f['profile']['worker'],'--ttl','3900']])
        self.assertEqual(self.receipts(),['lifetime-extension-0001.json'])
        self.assertEqual((receipt['schema'],receipt['version'],receipt['sequence']),('sparkles.boat.lifetime-extension',1,1))
        self.assertEqual(receipt['wall_end'],io.wall()+3600);self.assertEqual(receipt['boot_end'],io.mono()+3600)
        self.assertEqual(receipt['adoption']['worker_boot_id'],WORKER_BOOT_ID)
        life=self.lifetime()
        self.assertEqual((life['wall_end'],life['boot_end']),(receipt['wall_end'],receipt['boot_end']))
        # The extended end is the act's own observation plus its seconds, not the old end plus them.
        self.assertLess(life['boot_end'],self.original['boot_end']+3600)
        # Re-export after the original end reproduces the original admission bytes.
        later=self.f['now']+self.left+100
        with patch.object(self.qwa,'boottime',return_value=self.f['boot_now']+self.left+100):
            data=self.qwa.export_operation(self.op,later,self.f['profile']['worker'],self.f['profile'])
        self.assertEqual(data,self.f['qualified'])
        with patch.object(self.qwa,'boottime',return_value=self.f['boot_now']+self.left+100):
            loaded=env.load_job_admission(self.f['admission'],self.f['profile_path'],self.f['profile']['worker'],later,lambda unit:self.f['guard'])
        self.assertEqual(loaded[4]['boot_end'],receipt['boot_end'])

    def test_a_second_extension_chains_and_the_total_is_capped(self):
        first=self.extend(self.io(self.left-600),10500)
        second=self.extend(self.io(self.left+9000),10500)
        self.assertEqual(second['sequence'],2);self.assertEqual(self.receipts(),['lifetime-extension-0001.json','lifetime-extension-0002.json'])
        self.assertEqual(self.lifetime()['boot_end'],second['boot_end'])
        third=self.extend(self.io(self.left+18000),10500)
        # The cap on the total lifetime refuses an extension past 12 hours after the intent.
        io=self.io(self.left+27000)
        with self.assertRaisesRegex(controller.Refusal,'total lifetime cap'):self.extend(io,10500)
        self.assertEqual(io.calls,[]);self.assertEqual(len(self.receipts()),3)
        self.assertLessEqual(third['boot_end'],self.intent['mono']+43200)
        self.assertEqual(self.lifetime()['boot_end'],third['boot_end'])

    def test_unreceipted_or_malformed_extensions_are_not_honoured(self):
        # A provider TTL change made outside the act leaves the original end.
        self.assertEqual(self.lifetime(),self.original)
        io=self.io(self.left-600);receipt=self.extend(io,3600)
        good=(self.op/'lifetime-extension-0001.json').read_text()
        cases={'unknown version':dict(receipt,version=2),'edited end':dict(receipt,wall_end=receipt['wall_end']+1000),
               'other guard':dict(receipt,guard=dict(receipt['guard'],invocation_id='d'*32)),
               'missing provider confirmation':{k:v for k,v in receipt.items() if k!='provider'},
               'provider short':dict(receipt,provider=dict(receipt['provider'],archive_after_unix=receipt['wall_end'])),
               'other worker boot':dict(receipt,adoption=dict(receipt['adoption'],worker_boot_id='0'*8+'-0000-0000-0000-'+'0'*12)),
               'recorded after the end':dict(receipt,observed_boottime=self.original['boot_end']+1,boot_end=self.original['boot_end']+3601)}
        for name,value in cases.items():
            with self.subTest(name):
                path=self.op/'lifetime-extension-0001.json';path.write_text(json.dumps(value));path.chmod(0o600)
                with self.assertRaises(ValueError):self.lifetime()
        path=self.op/'lifetime-extension-0001.json';path.write_text(good);path.chmod(0o600)
        for name in ('lifetime-extension-0003.json','lifetime-extension-1.json'):
            with self.subTest(name):
                (self.op/name).write_text(good);(self.op/name).chmod(0o600)
                with self.assertRaises(ValueError):self.lifetime()
                (self.op/name).unlink()
        self.assertEqual(self.lifetime()['boot_end'],receipt['boot_end'])

    def test_adoption_refuses_another_boot_environment_guard_or_a_stopping_operation(self):
        changes={'worker rebooted':lambda io:setattr(io,'worker_boot','11111111-1111-1111-1111-111111111111'),
                 'local boot':lambda io:setattr(io,'local_boot','22222222-2222-2222-2222-222222222222'),
                 'guard replaced':lambda io:io.live.update(InvocationID='d'*32),
                 'guard pid':lambda io:io.live.update(MainPID='999'),
                 'guard inactive':lambda io:io.live.update(ActiveState='failed'),
                 'environment version':lambda io:setattr(io,'env_version',9)}
        for name,change in changes.items():
            with self.subTest(name):
                io=self.io(100);change(io)
                with self.assertRaises((controller.Refusal,ValueError)):self.extend(io)
                self.assertEqual(io.calls,[]);self.assertEqual(self.receipts(),[])
        for name,fact in {'hook':dict(hook_sha256='e'*64),'version':dict(latestVersionNumber=9),'stale':dict(observed_unix=self.f['now']-1000),'unverified':dict(verified_by_Root=False)}.items():
            with self.subTest(name):
                io=self.io(100)
                with self.assertRaises(controller.Refusal):self.extend(io,**fact)
                self.assertEqual(io.calls,[]);self.assertEqual(self.receipts(),[])
        io=self.io(100);io.record('stop-trigger.json',{'requested_wall':io.wall()})
        with self.assertRaisesRegex(controller.Refusal,'stopping or closed'):self.extend(io)
        self.assertEqual(io.calls,[])

    def test_bounds_expiry_and_provider_uncertainty_refuse_without_a_receipt(self):
        for seconds in (0,-1,10501,3600.5,True):
            with self.subTest(seconds=seconds):
                io=self.io(100)
                with self.assertRaises(controller.Refusal):self.extend(io,seconds)
                self.assertEqual(io.calls,[])
        io=self.io(self.left-10)
        with self.assertRaisesRegex(controller.Refusal,'closure reserve'):self.extend(io)
        self.assertEqual(io.calls,[])
        io=self.io(100);io.extend_exit=1
        with self.assertRaisesRegex(controller.Refusal,'provider'):self.extend(io)
        io=self.io(100);io.archive_shortfall=150
        with self.assertRaisesRegex(controller.Refusal,'provider'):self.extend(io)
        self.assertEqual(self.receipts(),[])
        attempts=[n for n in os.listdir(self.op) if n.startswith('lifetime-extension-attempt-')]
        self.assertEqual(len([n for n in attempts if n.endswith('-failed.json')]),2)
        self.assertEqual(self.lifetime(),self.original)

    def test_an_act_slowed_past_the_previous_end_publishes_nothing(self):
        # The last guard query takes 40 seconds, which crosses the closure reserve.
        io=self.io(self.left-60);io.show_delay=40
        with self.assertRaisesRegex(controller.Refusal,'previous end'):self.extend(io)
        self.assertEqual(self.receipts(),[]);self.assertEqual(self.lifetime(),self.original)
        # A receipt whose publication time is after the previous end is refused.
        receipt=self.extend(self.io(self.left-600),3600)
        late=dict(receipt,published_boottime=self.original['boot_end']+1)
        path=self.op/'lifetime-extension-0001.json';path.write_text(json.dumps(late));path.chmod(0o600)
        with self.assertRaises(ValueError):self.lifetime()

    def test_fabricated_or_out_of_order_receipts_are_refused(self):
        receipt=self.extend(self.io(self.left-600),3600)
        good=(self.op/'lifetime-extension-0001.json').read_text()
        cases={'partial adoption':dict(receipt,adoption={'worker_boot_id':receipt['adoption']['worker_boot_id']}),
               'adoption other environment':dict(receipt,adoption=dict(receipt['adoption'],hook_sha256='e'*64)),
               'adoption other local boot':dict(receipt,adoption=dict(receipt['adoption'],local_boot_id='22222222-2222-2222-2222-222222222222')),
               'adoption other guard pid':dict(receipt,adoption=dict(receipt['adoption'],guard_pid=999)),
               'archive not a time':dict(receipt,provider=dict(receipt['provider'],archive_after='NOT A TIMESTAMP')),
               'archive disagrees':dict(receipt,provider=dict(receipt['provider'],archive_after_unix=receipt['provider']['archive_after_unix']+5000)),
               'null reason':dict(receipt,reason=None),
               'published before observed':dict(receipt,published_boottime=receipt['observed_boottime']-1)}
        for name,value in cases.items():
            with self.subTest(name):
                path=self.op/'lifetime-extension-0001.json';path.write_text(json.dumps(value));path.chmod(0o600)
                with self.assertRaises(ValueError):self.lifetime()
        path=self.op/'lifetime-extension-0001.json';path.write_text(good);path.chmod(0o600)
        second=self.extend(self.io(self.left+2000),10500)
        earlier=dict(second,observed_boottime=receipt['observed_boottime']-100,boot_end=receipt['observed_boottime']-100+10500,
                     published_boottime=receipt['observed_boottime']-99)
        path=self.op/'lifetime-extension-0002.json';path.write_text(json.dumps(earlier));path.chmod(0o600)
        with self.assertRaises(ValueError):self.lifetime()

    def test_stop_decisions_freeze_extensions_and_refuse_admission(self):
        # An explicit stop trigger takes the extension lock before the guard stops.
        io=self.io(100);io.record('stop-trigger.json',{'requested_wall':io.wall()});frozen=[]
        io.freeze_extensions=lambda:frozen.append(io.mono()) or True
        with patch.object(controller,'reconcile_stop',return_value=True):
            self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
        self.assertTrue(frozen)
        # A deadline stop publishes the stopping state while it holds the lock.
        (self.op/'stop-trigger.json').unlink()
        io=self.io(self.left+60)
        with patch.object(controller,'reconcile_stop',return_value=True):
            self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
        self.assertTrue((self.op/'stop-trigger.json').exists())
        # Admission refuses an operation that is stopping, extended or not.
        with self.assertRaises(ValueError):
            env.load_job_admission(self.f['admission'],self.f['profile_path'],self.f['profile']['worker'],self.f['now'],lambda unit:self.f['guard'])

    def test_guard_fallback_never_restores_a_later_end(self):
        # A forward wall jump at handoff makes the conservative original end earlier than the intent's.
        path=self.op/'guard-admission.json';guard=json.loads(path.read_text());guard['observed_wall']+=2000
        path.write_text(json.dumps(guard));path.chmod(0o600)
        wall,boot=self.qwa.original_ends(self.intent,guard,json.loads((self.op/'root-handoff.json').read_text()))
        self.assertLess(boot,self.intent['deadline_mono'])
        io=self.io(0)
        self.assertEqual(controller.guard_deadlines(io,self.intent),(boot,wall))
        bad=self.op/'lifetime-extension-0001.json';bad.write_text('{"schema":"sparkles.boat.lifetime-extension","version":2}');bad.chmod(0o600)
        self.assertEqual(controller.guard_deadlines(io,self.intent),(boot,wall))

    def test_only_a_receipt_the_guard_accepts_in_time_grants_time(self):
        # Published in time, but the guard first reads it after the previous end.
        io=self.io(self.left-600);receipt=self.extend(io,3600,accept=False)
        self.assertEqual(self.lifetime(),self.original)
        late=ExtendIO(self.f,self.f['now']+self.left+1,self.f['boot_now']+self.left+1)
        self.assertEqual(controller.guard_deadlines(late,self.intent),(self.original['boot_end'],self.original['wall_end']))
        self.assertFalse((self.op/'lifetime-extension-0001-accepted.json').exists())
        self.assertEqual(self.lifetime(),self.original)
        # Another act refuses while a previous receipt is unaccepted.
        with self.assertRaisesRegex(controller.Refusal,'not accepted'):self.extend(self.io(100))
        # An edited acceptance or one for another receipt refuses the chain.
        (self.op/'lifetime-extension-0001.json').unlink()
        receipt=self.extend(self.io(self.left-600),3600)
        path=self.op/'lifetime-extension-0001-accepted.json';good=json.loads(path.read_text())
        for name,value in {'late':dict(good,accepted_boottime=self.original['boot_end']+1),'other receipt':dict(good,receipt_sha256='0'*64)}.items():
            with self.subTest(name):
                path.write_text(json.dumps(value));path.chmod(0o600)
                with self.assertRaises(ValueError):self.lifetime()

    def test_an_acceptance_written_late_grants_nothing(self):
        io=self.io(self.left-600);self.extend(io,3600,accept=False)
        # The guard samples its clocks in time, but the write ends after the previous end.
        guard=ExtendIO(self.f,self.f['now']+self.left-40,self.f['boot_now']+self.left-40);real=guard.record
        def slow(name,value):
            real(name,value)
            if name.endswith('-accepted.json'):guard.t+=41;guard.w+=41
        guard.record=slow
        self.assertEqual(controller.guard_deadlines(guard,self.intent),(self.original['boot_end'],self.original['wall_end']))
        self.assertEqual(controller.guard_deadlines(guard,self.intent),(self.original['boot_end'],self.original['wall_end']))
        # The guard's veto record makes every other reader ignore that acceptance too.
        self.assertTrue((self.op/'lifetime-extension-0001-vetoed.json').exists())
        self.assertEqual(self.lifetime(),self.original)
        # Too close to the end, the guard does not accept at all.
        (self.op/'lifetime-extension-0001-accepted.json').unlink();(self.op/'lifetime-extension-0001-vetoed.json').unlink()
        near=ExtendIO(self.f,self.f['now']+self.left-20,self.f['boot_now']+self.left-20)
        self.assertEqual(controller.guard_deadlines(near,self.intent),(self.original['boot_end'],self.original['wall_end']))
        self.assertFalse((self.op/'lifetime-extension-0001-accepted.json').exists())

    def test_an_acceptance_write_error_after_publication_is_vetoed(self):
        self.extend(self.io(self.left-600),3600,accept=False)
        guard=ExtendIO(self.f,self.f['now']+self.left-40,self.f['boot_now']+self.left-40);real=guard.record
        def failing(name,value):
            real(name,value)
            if name.endswith('-accepted.json'):guard.t+=41;guard.w+=41;raise controller.Refusal('directory fsync failed')
        guard.record=failing
        for _ in range(2):
            self.assertEqual(controller.guard_deadlines(guard,self.intent),(self.original['boot_end'],self.original['wall_end']))
        self.assertTrue((self.op/'lifetime-extension-0001-vetoed.json').exists())
        self.assertEqual(self.lifetime(),self.original)
        # The guard loop stops at the original end and publishes the stop trigger.
        with patch.object(controller,'reconcile_stop',return_value=True):
            self.assertTrue(controller.guard_loop(self.f['release'],guard,self.intent))
        self.assertTrue((self.op/'stop-trigger.json').exists())

    def test_a_clock_error_after_the_acceptance_write_is_vetoed(self):
        self.extend(self.io(self.left-600),3600,accept=False)
        guard=ExtendIO(self.f,self.f['now']+self.left-40,self.f['boot_now']+self.left-40);real=guard.record;clock=guard.mono;state={'fail':False}
        def record(name,value):
            real(name,value)
            # Only BOOTTIME crossed the end, so the check after the write reads that clock.
            if name.endswith('-accepted.json'):state['fail']=True;guard.t+=41
        def mono():
            if state['fail']:state['fail']=False;raise OSError('clock unavailable')
            return clock()
        guard.record=record;guard.mono=mono
        controller.guard_deadlines(guard,self.intent)
        self.assertEqual(controller.guard_deadlines(guard,self.intent),(self.original['boot_end'],self.original['wall_end']))
        self.assertEqual(self.lifetime(),self.original)

    def test_stop_transition_errors_still_reach_reconciliation(self):
        def broken(*a):raise OSError('too many open files')
        for name,setup in {'deadline freeze':lambda io:setattr(io,'freeze_extensions',broken),
                           'deadline trigger':lambda io:setattr(io,'trigger',broken),
                           'explicit stop freeze':lambda io:(setattr(io,'freeze_extensions',broken),io.record('stop-trigger.json',{}))}.items():
            with self.subTest(name):
                trigger=self.op/'stop-trigger.json'
                if trigger.exists():trigger.unlink()
                io=self.io(self.left+60 if 'deadline' in name else 100);setup(io);seen=[]
                with patch.object(controller,'reconcile_stop',side_effect=lambda r,io:seen.append(1) or True):
                    self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
                self.assertEqual(seen,[1])
        # A failed thaw after a valid extension keeps the guard running to the new end.
        if (self.op/'stop-trigger.json').exists():(self.op/'stop-trigger.json').unlink()
        receipt=self.extend(self.io(self.left-600),3600)
        io=self.io(self.left+60);io.thaw_extensions=broken;seen=[]
        with patch.object(controller,'reconcile_stop',side_effect=lambda r,io:seen.append(io.mono()) or True):
            self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
        self.assertGreaterEqual(seen[0],receipt['boot_end'])

    def test_acceptances_follow_each_other_in_order(self):
        self.extend(self.io(self.left-600),3600);second=self.extend(self.io(self.left-300),10500)
        path=self.op/'lifetime-extension-0001-accepted.json';first=json.loads(path.read_text())
        # Acceptance one now claims a time after act two was observed, still before its own end.
        moved=dict(first,accepted_boottime=second['observed_boottime']+100,accepted_wall=second['observed_wall']+100)
        self.assertLess(moved['accepted_boottime']+30,self.original['boot_end'])
        path.write_text(json.dumps(moved));path.chmod(0o600)
        with self.assertRaises(ValueError):self.lifetime()

    def test_concurrent_stop_trigger_does_not_end_the_guard_before_reconciliation(self):
        guard=controller.RealIO(self.f['release'],self.op);real=guard.exists
        def racing(name):
            if name=='stop-trigger.json' and not real(name):
                # finish publishes its trigger between the guard's check and its write.
                controller.RealIO(self.f['release'],self.op).record('stop-trigger.json',{'requested_wall':0});return False
            return real(name)
        guard.exists=racing;guard.trigger()
        self.assertTrue((self.op/'stop-trigger.json').exists())

    def test_the_conservative_base_survives_lost_handoff_receipts(self):
        path=self.op/'guard-admission.json';guard=json.loads(path.read_text());guard['observed_wall']+=2000
        path.write_text(json.dumps(guard));path.chmod(0o600)
        io=self.io(0);first=controller.guard_deadlines(io,self.intent)
        self.assertLess(first[0],self.intent['deadline_mono'])
        path.unlink();self.assertEqual(controller.guard_deadlines(io,self.intent),first)
        (self.op/'root-handoff.json').unlink();self.assertEqual(controller.guard_deadlines(io,self.intent),first)

    def test_the_guard_lock_excludes_an_extension_act(self):
        guard=controller.RealIO(self.f['release'],self.op)
        self.assertTrue(guard.freeze_extensions())
        try:
            with self.assertRaises(BlockingIOError):
                controller.extend_entry(self.op,3600,self.root/'environment-fact.json','blocked by the stopping guard')
        finally:guard.thaw_extensions()
        self.assertEqual(self.receipts(),[])

    def test_wall_changes_at_the_act_are_bounded_by_boottime(self):
        # A forward wall jump makes a later wall end, but BOOTTIME still bounds the lifetime.
        io=self.io(100,wall_shift=3000);receipt=self.extend(io,10500)
        life=self.lifetime();self.assertEqual(life['wall_end']-(io.wall()-3000),13500)
        self.assertEqual(self.qwa.lifetime_left(life,io.wall()-3000,io.mono(),self.f['boot_id']),10500)
        # A wall rollback that would end earlier than the current end is refused, not stretched.
        io=self.io(200,wall_shift=-9000)
        with self.assertRaisesRegex(controller.Refusal,'shorten'):self.extend(io,3600)
        self.assertEqual(io.calls,[]);self.assertEqual(self.receipts(),['lifetime-extension-0001.json'])

    def test_guard_honours_only_valid_receipts(self):
        io=self.io(0)
        # The conservative base may end a clock tick before the intent, never after it.
        boot,wall=controller.guard_deadlines(io,self.intent)
        self.assertLessEqual((boot,wall),(self.intent['deadline_mono'],self.intent['deadline_wall']))
        self.assertLessEqual(boot,self.intent['deadline_mono']);self.assertLessEqual(wall,self.intent['deadline_wall'])
        self.assertAlmostEqual(boot,self.intent['deadline_mono'],delta=1e-3);self.assertAlmostEqual(wall,self.intent['deadline_wall'],delta=1e-3)
        receipt=self.extend(self.io(self.left-600),3600)
        self.assertEqual(controller.guard_deadlines(io,self.intent),(receipt['boot_end'],receipt['wall_end']))
        # The guard loop stops at the extended end, not the original one.
        seen=[]
        io=self.io(self.left+60)
        with patch.object(controller,'reconcile_stop',side_effect=lambda r,io:seen.append((io.mono(),io.wall())) or True):
            self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
        self.assertGreaterEqual(seen[0][0],receipt['boot_end']);self.assertLess(seen[0][0],receipt['boot_end']+2)
        # That stop published the stopping state. Remove it to exercise a second run.
        self.assertTrue((self.op/'stop-trigger.json').exists());(self.op/'stop-trigger.json').unlink()
        # An act that publishes while the guard waits for the lock keeps the worker.
        receipt_two={}
        io=self.io(self.left+3000-30);later=self.io(self.left+3000-60)
        def freeze():
            receipt_two.update(self.extend(later,10500));return True
        io.freeze_extensions=freeze;seen.clear()
        with patch.object(controller,'reconcile_stop',side_effect=lambda r,io:seen.append(io.mono()) or True):
            self.assertTrue(controller.guard_loop(self.f['release'],io,self.intent))
        self.assertGreaterEqual(seen[0],receipt_two['boot_end'])
        (self.op/'lifetime-extension-0002.json').unlink()
        # A malformed receipt falls back to the original deadline, or the conservative
        # base a clock tick before it.
        path=self.op/'lifetime-extension-0001.json';path.write_text(json.dumps(dict(receipt,version=2)));path.chmod(0o600)
        boot,wall=controller.guard_deadlines(io,self.intent)
        self.assertLessEqual(boot,self.intent['deadline_mono']);self.assertLessEqual(wall,self.intent['deadline_wall'])
        self.assertAlmostEqual(boot,self.intent['deadline_mono'],delta=1e-3);self.assertAlmostEqual(wall,self.intent['deadline_wall'],delta=1e-3)


class RemoteTransport(unittest.TestCase):
    """Remote programs run whether the provider passes the words after ``--`` as arguments, as observed, or joins them."""
    def run_remote(self,method,*args):
        import shlex,subprocess
        root=Path(tempfile.mkdtemp(dir=self.tmp.name));f=fixture(root);controller.bind_release_profile(f['release'])
        shim=root/'bin';shim.mkdir();fake=shim/'systemctl'
        fake.write_text('#!/bin/sh\nfor a in "$@"; do case "$a" in show) echo LoadState=not-found; echo MainPID=0;; esac; done\n');fake.chmod(0o700)
        calls=[];state=root/'state';state.mkdir();checkout=root/'checkout';checkout.mkdir()
        class Capture(controller.RealIO):
            def boat(self,argv,seconds=20,input_bytes=None,json_mode=True):
                calls.append(argv);words=argv[argv.index('--')+1:]
                # What the worker's shell would run after joining the words.
                self_test.assertEqual(shlex.split(' '.join(words)),words)
                # The session starts from the worker's state directory, here a local
                # stand-in, while the provider lands it in the checkout.
                self_test.assertEqual(words[:3],['/usr/bin/env','-C',controller.profile()['worker_home']])
                # This host has no /usr/bin/python3, so the local interpreter stands in.
                self_test.assertEqual(words[3],'/usr/bin/python3')
                words=[*words[:2],str(state),sys.executable,*words[4:]]
                # An inert systemctl answers in place of the host's managers.
                r=subprocess.run(words,input=input_bytes,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,timeout=30,env={'PATH':str(shim)},cwd=checkout)
                return {'exit':r.returncode,'error':None,'body':json.loads(r.stdout)}
        self_test=self;io=Capture(f['release'],f['operation'])
        try:return getattr(io,method)(*args),calls
        finally:self.assertEqual(sorted(os.listdir(checkout)),[],'a session wrote into the checkout')

    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)

    def test_remote_programs_survive_shell_joining_and_run(self):
        boot,_=self.run_remote('worker_boot_id')
        self.assertEqual(boot,Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        check,calls=self.run_remote('job_closure',())
        self.assertEqual(calls[0][-4:],['/usr/bin/python3','-I','-B','-'])
        self.assertEqual(check['list_exit'],0);self.assertIsInstance(check['units'],list);self.assertIsInstance(check['processes'],list)
        self.assertEqual(check['boot_id'],boot)
        with self.assertRaises(controller.Refusal):self.run_remote('metadata_closed')


class JobClosureProgram(unittest.TestCase):
    """The maintained worker job-closure program and its bounds, without a worker."""
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.f=fixture(Path(self.tmp.name));controller.bind_release_profile(self.f['release'])

    def run_program(self,rows,units):
        import io as stdio,contextlib,types
        common,definitions,observer=controller.observer_sources()
        ns={'__name__':'job-closure-definitions'}
        exec(compile(common+'\n'+definitions+'\n'+observer,'job-closure','exec'),ns)
        me=os.getpid();rows=dict(rows);rows[me]={'pid':me,'ppid':1,'uid':os.getuid(),'comm':'python3','start':7,'group':me}
        listing=''.join(u+' loaded active running x\n' for u in units).encode()
        fake=types.SimpleNamespace(run=lambda *a,**k:types.SimpleNamespace(returncode=0,stdout=listing),DEVNULL=-3,PIPE=-1)
        ns.update(process_rows=lambda:rows,subprocess=fake,command=lambda argv,seconds=15:b'MainPID=1220\n')
        ns['sys']=types.SimpleNamespace(argv=['remote',json.dumps(controller.profile()),json.dumps(list(controller.JOB_UNIT_PREFIXES))])
        ns['__name__']='__main__';out=stdio.StringIO()
        with contextlib.redirect_stdout(out):exec(compile(controller.JOB_CLOSURE_ENTRY,'job-closure-entry','exec'),ns)
        return json.loads(out.getvalue())

    def session(self):
        uid=os.getuid()
        return {1:{'pid':1,'ppid':0,'uid':0,'comm':'systemd','start':1,'group':1},
                1220:{'pid':1220,'ppid':1,'uid':uid,'comm':'systemd','start':5,'group':1220},
                1264:{'pid':1264,'ppid':1220,'uid':uid,'comm':'pipewire','start':5,'group':1264},
                976:{'pid':976,'ppid':1,'uid':uid,'comm':'pulseaudio','start':5,'group':976}}

    def test_a_quiet_worker_is_closed_and_detached_work_is_not(self):
        q=self.run_program(self.session(),['dbus.service'])
        self.assertEqual((q['units'],q['unit_count'],q['processes'],q['process_count']),([],0,[],0))
        uid=os.getuid();rows=self.session()
        # A partition share under setsid, a gate unit and a timeout wrapper.
        rows.update({40:{'pid':40,'ppid':1,'uid':uid,'comm':'bash','start':9,'group':40},41:{'pid':41,'ppid':40,'uid':uid,'comm':'cargo-nextest','start':9,'group':40},
                     42:{'pid':42,'ppid':41,'uid':uid,'comm':'store_tests-1a','start':9,'group':40},50:{'pid':50,'ppid':1,'uid':uid,'comm':'timeout','start':9,'group':50}})
        q=self.run_program(rows,['gate-batch.service','sparkles-screen-run-a.service','nsc-build-b.scope','dbus.service'])
        self.assertEqual(q['unit_count'],3);self.assertEqual({x['pid'] for x in q['processes']},{40,41,42,50})
        self.assertNotIn('python3',[x['comm'] for x in q['processes']])

    def test_one_bound_covers_the_worker_and_local_checks(self):
        class Timed(controller.RealIO):
            def __init__(self,r,op):super().__init__(r,op);self.t=1000.0;self.spent=[]
            def mono(self):return self.t
            def remote_python(self,program,args,seconds):
                # A call that runs to its timeout and then spends two seconds in teardown.
                self.spent.append(('remote',seconds));self.t+=seconds+2
                return {'exit':0,'error':None,'body':{'list_exit':0,'units':[],'unit_count':0,'processes':[],'process_count':0,'boot_id':WORKER_BOOT_ID}}
            def system(self,args,seconds=15):
                self.spent.append(('local',seconds));self.t+=seconds+1
                return '\n\n'.join('Id=%s\nActiveState=inactive\nMainPID=0'%u for u in args[3:-1])
        io=Timed(self.f['release'],self.f['operation'])
        units=tuple('lane-%d.service'%n for n in range(8))
        # Both calls overrun their timeouts and still end inside the bound.
        q=io.job_closure(units,60);self.assertEqual(q['local_units_closed'],{u:True for u in units})
        self.assertEqual(io.spent,[('remote',38),('local',14)]);self.assertLessEqual(io.t,1060)
        io=Timed(self.f['release'],self.f['operation']);io.remote_python=lambda p,a,s:(setattr(io,'t',io.t+s-12),{'exit':0,'error':None,'body':{}})[1]
        io.system=lambda args,seconds=15:(setattr(io,'t',io.t+1),'\n\n'.join('Id=%s\nActiveState=inactive\nMainPID=0'%u for u in args[3:-1]))[1]
        self.assertEqual(io.job_closure(units,60)['local_units_closed'],{u:True for u in units})
        with self.assertRaises(controller.Refusal):io.job_closure(units,14)

    def test_teardown_stays_inside_the_bound_with_and_without_local_units(self):
        class Slow(controller.RealIO):
            def __init__(self,r,op):super().__init__(r,op);self.t=0.0
            def mono(self):return self.t
            # Every bounded call runs to its timeout and then uses its full teardown window.
            def remote_python(self,program,args,seconds):self.t+=seconds+controller.owned_group.WINDOW;return {'exit':None,'error':'timeout','body':None}
            def system(self,args,seconds=15):self.t+=seconds+controller.owned_group.WINDOW;raise controller.Refusal('systemd command failed')
        for units in [(),('job.service',)]:
            io=Slow(self.f['release'],self.f['operation'])
            with self.assertRaises(controller.Refusal):io.job_closure(units,90)
            self.assertLessEqual(io.t,90,units)
        class Quiet(Slow):
            def remote_python(self,program,args,seconds):
                self.t+=seconds+controller.owned_group.WINDOW;return {'exit':0,'error':None,'body':{}}
        io=Quiet(self.f['release'],self.f['operation'])
        with self.assertRaises(controller.Refusal):io.job_closure(('job.service',),90)
        self.assertLessEqual(io.t,90)

    def test_local_unit_names_resolve_like_systemctl(self):
        class Local(controller.RealIO):
            def remote_python(self,program,args,seconds):return {'exit':0,'error':None,'body':{}}
            def system(self,args,seconds=15):
                self.asked=args[3:-1];return '\n\n'.join('Id=%s\nActiveState=inactive\nMainPID=0'%u for u in self.asked)
        io=Local(self.f['release'],self.f['operation'])
        q=io.job_closure(('job','job.v1','build.scope','lane.service'),60)
        self.assertEqual(io.asked,['job.service','job.v1.service','build.scope','lane.service'])
        self.assertEqual(q['local_units_closed'],{'job':True,'job.v1':True,'build.scope':True,'lane.service':True})

    def test_lock_files_of_another_mode_link_or_kind_refuse(self):
        op=self.f['operation'];io=controller.RealIO(self.f['release'],op);lock=op/controller.JOB_LOCK
        self.assertTrue(io.lock_jobs());io.unlock_jobs()
        for change in ['mode','hardlink','symlink']:
            with self.subTest(change=change):
                lock.unlink(missing_ok=True);other=op.parent/('other-'+change)
                if change=='mode':lock.touch();lock.chmod(0o666)
                if change=='hardlink':other.touch();other.chmod(0o600);os.link(other,lock)
                if change=='symlink':other.touch();other.chmod(0o600);lock.symlink_to(other)
                with self.assertRaises((controller.Refusal,OSError)):io.lock_jobs()
                with self.assertRaises((controller.Refusal,OSError)):env.hold_job_lock(op,0.1)
                self.assertIsNone(getattr(io,'_jobs',None))
        lock.unlink();env.release_job_locks()


class BoundedCommand(unittest.TestCase):
    """The launcher's command reader with real local children."""
    def test_output_over_the_cap_is_refused_before_the_child_finishes(self):
        import common,subprocess,time
        with tempfile.TemporaryDirectory() as d:
            done=Path(d)/'finished'
            script="import os,sys\nfor _ in range(256):os.write(2,b'x'*65536)\nopen(%r,'w').close()"%str(done)
            started=time.monotonic()
            with self.assertRaises(common.Refusal) as caught:common.bounded_command([sys.executable,'-I','-c',script],30,True)
            self.assertEqual(str(caught.exception),'output bound');self.assertLess(time.monotonic()-started,15)
            time.sleep(0.2);self.assertFalse(done.exists())
            with self.assertRaises(common.Refusal):common.bounded_command([sys.executable,'-I','-c',"import os\nfor _ in range(32):os.write(1,b'y'*65536)"],30)
    def test_a_selector_failure_kills_and_reaps_the_child(self):
        import common,subprocess
        children=[];actual=subprocess.Popen
        def launch(*a,**k):
            child=actual(*a,**k);children.append(child);return child
        with patch.object(common.subprocess,'Popen',side_effect=launch),patch.object(common.selectors,'DefaultSelector',side_effect=OSError(12,'no memory')):
            with self.assertRaises(OSError):common.bounded_command([sys.executable,'-I','-c','import time;time.sleep(30)'],30,True)
        self.assertEqual(len(children),1);self.assertIsNotNone(children[0].returncode)
        self.assertTrue(children[0].stdout.closed and children[0].stderr.closed)
    def test_a_failure_while_listing_the_pipes_kills_and_reaps_the_child(self):
        import common,subprocess
        children=[];actual=subprocess.Popen
        class Child:
            # The first read of stdout fails, as an allocation failure during setup would.
            def __init__(self,child):self.child=child;self.reads=0
            def __getattr__(self,name):return getattr(self.child,name)
            @property
            def stdout(self):
                self.reads+=1
                if self.reads==1:raise MemoryError()
                return self.child.stdout
        def launch(*a,**k):
            child=Child(actual(*a,**k));children.append(child);return child
        with patch.object(common.subprocess,'Popen',side_effect=launch):
            with self.assertRaises(MemoryError):common.bounded_command([sys.executable,'-I','-c','import time;time.sleep(30)'],30,True)
        self.assertEqual(len(children),1);self.assertIsNotNone(children[0].child.returncode)
        self.assertTrue(children[0].child.stdout.closed and children[0].child.stderr.closed)
    def test_each_cleanup_step_runs_when_another_fails(self):
        import common,subprocess,selectors
        children=[];actual=subprocess.Popen;real_selector=selectors.DefaultSelector
        class Pipe:
            def __init__(self,pipe):self.pipe=pipe
            def __getattr__(self,name):return getattr(self.pipe,name)
            def close(self):self.pipe.close();raise OSError(5,'close failed')
        class Selector:
            def __init__(self):self.inner=real_selector()
            def __getattr__(self,name):return getattr(self.inner,name)
            def close(self):self.inner.close();raise OSError(5,'close failed')
        def launch(*a,**k):
            child=actual(*a,**k);child.stdout=Pipe(child.stdout);children.append(child);return child
        with patch.object(common.subprocess,'Popen',side_effect=launch),patch.object(common.selectors,'DefaultSelector',Selector):
            with self.assertRaises(OSError):common.bounded_command([sys.executable,'-I','-c','import time;time.sleep(30)'],1,True)
        self.assertIsNotNone(children[0].returncode);self.assertTrue(children[0].stdout.pipe.closed and children[0].stderr.closed)
    def test_timeout_kills_the_child_and_output_is_returned(self):
        import common,subprocess,time
        started=time.monotonic()
        with self.assertRaises(subprocess.TimeoutExpired):common.bounded_command([sys.executable,'-I','-c','import time;time.sleep(30)'],1,True)
        self.assertLess(time.monotonic()-started,12)
        r=common.bounded_command([sys.executable,'-I','-c',"import sys;sys.stdout.write('out');sys.stderr.write('Memory peak: 2.2M (swap: 0B)');raise SystemExit(3)"],30,True)
        self.assertEqual((r.returncode,r.stdout,r.stderr),(3,b'out',b'Memory peak: 2.2M (swap: 0B)'))
        r=common.bounded_command([sys.executable,'-I','-c',"import sys;sys.stderr.write('ignored')"],30);self.assertIsNone(r.stderr)


from boat_fixtures import TickIO,OP5_CLOCKS


def handoff_release():
    r=controls.fixture()
    r.update(useful_job_ready=True,separate_Root_handoff={'job_release_sha256':'a'*64,'different_job_review_sha256':'b'*64,'Root_owns_job_and_deadline_closure':True,'finite_job_and_closure_seconds':600})
    return r


def guard_row(io):
    loaded={'Id':io.r['guard_unit']+'.service','ActiveState':'active','MainPID':'123','InvocationID':'a'*32}
    row={'pid':123,'uid':os.getuid(),'observed_wall':io.wall(),'observed_boottime':io.mono(),'boot_id':Path('/proc/sys/kernel/random/boot_id').read_text().strip(),'loaded':loaded}
    io.record('guard-admission.json',row);return row


class AdmissionClocks(unittest.TestCase):
    """Admission against the clock receipts op-5 wrote on bx_gy2ytgh8, and clocks that move."""
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup);self.f=fixture(Path(self.tmp.name))
        self.op5=json.loads((Path(__file__).parent/'fixtures/boat-admission-clocks-op5.json').read_text())

    def inputs(self):
        op=self.f['operation'];read=lambda n:json.loads((op/n).read_text())
        intent=read('controller-intent.json');guard=read('guard-admission.json');handoff=read('root-handoff.json')
        intent.update(self.op5['intent']);guard.update(self.op5['guard']);handoff['deadline_wall']=self.op5['handoff_deadline_wall']
        return self.f['release'],intent,read('qualification.json'),read('startup-closed.json'),guard,handoff

    def test_admission_accepts_the_op5_clock_receipts(self):
        r,intent,q,startup,guard,handoff=self.inputs()
        row=controller.make_admission(r,intent,q,startup,guard,handoff,guard['observed_wall']+1)
        wall,_=controller.lifetime.original_ends(intent,guard,handoff)
        self.assertGreater(wall,intent['wall']+10500);self.assertLessEqual(wall,intent['deadline_wall'])
        self.assertTrue(row['expires_at'].startswith('2026-11-09T11:57:00'))
        # A deadline later than the reading by more than the clock skew still refuses.
        intent['deadline_wall']=intent['wall']+10501;handoff['deadline_wall']=intent['deadline_wall']
        with self.assertRaises(ValueError):controller.make_admission(r,intent,q,startup,guard,handoff,guard['observed_wall']+1)

    def test_the_controller_reads_each_clock_once_for_its_deadlines(self):
        r=handoff_release();io=TickIO(r);io.guard_for_admission=lambda:guard_row(io)
        self.assertEqual(controller.drive(r,io),'QUALIFIED_ROOT_HANDOFF_GUARD_ACTIVE')
        intent=io.read('controller-intent.json')
        self.assertEqual((intent['deadline_wall'],intent['deadline_mono']),(intent['wall']+10500,intent['mono']+10500))
        admission=io.read('qualified-worker-admission.json');self.assertEqual(admission['worker'],controller.worker_id())

    def test_a_failed_admission_after_the_handoff_stops_the_worker_normally(self):
        r=handoff_release();io=controls.InertIO(r)
        def broken():raise ValueError('earlier effective deadline')
        io.guard_for_admission=broken
        io.diagnose=lambda seconds:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        with self.assertRaises(ValueError):controller.drive(r,io)
        # The handoff is published only after its admission, so none stands.
        self.assertFalse(io.exists('root-handoff.json'));self.assertFalse(io.exists('qualified-worker-admission.json'))
        self.assertTrue(io.exists('root-handoff-withdrawn.json'));self.assertTrue(io.exists('stop-trigger.json'))
        self.assertTrue(io.exists('diagnostics.json'))
        # Without the withdrawal the guard would wait for a job closure that can never
        # exist, record unresolved.json and leave the worker to the provider TTL.
        self.assertTrue(controller.guard_loop(r,io,io.read('controller-intent.json')))
        self.assertTrue(io.exists('closed.json'));self.assertFalse(io.exists('unresolved.json'))
        self.assertEqual([x[0] for x in io.calls],['resume','stop'])

    def failing_writes(self,io,names):
        record=io.record
        def write(name,value):
            if name in names:raise OSError('injected write failure: '+name)
            return record(name,value)
        io.record=write

    def stops_normally(self,r,io):
        self.assertTrue(io.exists('stop-trigger.json'));self.assertFalse(controller.handoff_active(io))
        self.assertTrue(controller.guard_loop(r,io,io.read('controller-intent.json')))
        self.assertTrue(io.exists('closed.json'));self.assertFalse(io.exists('unresolved.json'))
        self.assertEqual([x[0] for x in io.calls],['resume','stop'])

    def test_a_failed_withdrawal_write_still_stops_the_worker(self):
        r=handoff_release();io=controls.InertIO(r)
        def broken():raise ValueError('earlier effective deadline')
        io.guard_for_admission=broken;io.diagnose=lambda seconds:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        self.failing_writes(io,{'root-handoff-withdrawn.json'})
        with self.assertRaises(ValueError):controller.drive(r,io)
        self.assertFalse(io.exists('root-handoff-withdrawn.json'))
        self.stops_normally(r,io)

    def test_a_failed_handoff_write_after_the_admission_still_stops_the_worker(self):
        r=handoff_release();io=controls.InertIO(r);io.guard_for_admission=lambda:guard_row(io)
        io.diagnose=lambda seconds:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        self.failing_writes(io,{'root-handoff.json','root-handoff-withdrawn.json'})
        with self.assertRaises(OSError):controller.drive(r,io)
        self.assertTrue(io.exists('qualified-worker-admission.json'));self.assertFalse(io.exists('root-handoff.json'))
        self.stops_normally(r,io)

    def failing_after_publication(self,io,name):
        """The receipt becomes visible and the write then raises, as a failed directory fsync would."""
        record=io.record;self.under_lock=[]
        def write(n,value):
            self.under_lock.append((n,getattr(io,'jobs_locked',None)));record(n,value)
            if n==name:raise OSError('injected failure after publication: '+n)
        io.record=write

    def published_io(self):
        r=handoff_release();io=controls.InertIO(r);io.guard_for_admission=lambda:guard_row(io)
        io.diagnose=lambda seconds:self.fail('a published handoff never holds')
        return r,io

    def test_a_handoff_failing_after_publication_is_withdrawn_under_the_job_lock(self):
        r,io=self.published_io();self.failing_after_publication(io,'root-handoff.json')
        with self.assertRaises(OSError):controller.drive(r,io)
        # Publication and withdrawal both ran under the exclusive job lock, so no job
        # command was admitted between them and the stop needs no job closure.
        locked=dict(self.under_lock)
        for name in ('qualified-worker-admission.json','root-handoff.json','root-handoff-withdrawn.json'):self.assertIs(locked[name],True,name)
        self.assertFalse(io.jobs_locked)
        self.stops_normally(r,io)

    def test_a_failed_withdrawal_after_publication_unpublishes_the_handoff(self):
        r,io=self.published_io();self.failing_after_publication(io,'root-handoff.json')
        record=io.record
        def write(n,value):
            if n=='root-handoff-withdrawn.json':raise OSError('injected withdrawal failure')
            return record(n,value)
        io.record=write
        with self.assertRaises(OSError):controller.drive(r,io)
        self.assertFalse(io.exists('root-handoff.json'));self.assertTrue(io.exists('unpublished-root-handoff.json'))
        self.stops_normally(r,io)

    def test_an_unrevocable_published_handoff_keeps_its_closure_requirement(self):
        r,io=self.published_io();self.failing_after_publication(io,'root-handoff.json')
        record=io.record;attempts=[]
        def write(n,value):
            if n=='root-handoff-withdrawn.json':attempts.append(io.jobs_locked);raise OSError('injected withdrawal failure')
            return record(n,value)
        io.record=write
        def unpublish(name):attempts.append(io.jobs_locked);raise OSError('injected rename failure')
        io.unpublish=unpublish
        with self.assertRaises(OSError):controller.drive(r,io)
        # Revocation is tried only under the job lock. Once the lock is released with
        # the handoff standing, a job may have been admitted, so nothing withdraws it
        # later and the stop keeps requiring the job closure.
        self.assertEqual(attempts,[True,True]);self.assertTrue(controller.handoff_active(io))
        self.assertFalse(io.exists('stop-trigger.json'))
        io.trigger();self.assertFalse(controller.reconcile_stop(r,io));self.assertEqual([x[0] for x in io.calls],['resume'])

    def test_a_held_job_lock_publishes_no_handoff(self):
        r,io=self.published_io();io.diagnose=lambda seconds:{'schema':1,'processes':{'count':0,'rows':[]},'state':{'entries':[],'phases':{}}}
        io.live_jobs=True
        with self.assertRaises(Exception):controller.drive(r,io)
        self.assertFalse(io.exists('qualified-worker-admission.json'));self.assertFalse(io.exists('root-handoff.json'))
        self.stops_normally(r,io)

    def test_an_admission_without_its_handoff_admits_nothing(self):
        op=self.f['operation'];(op/'root-handoff.json').unlink()
        with self.assertRaises(FileNotFoundError):controller.lifetime.export_operation(op,self.f['now'],self.f['profile']['worker'],self.f['profile'])
        self.assertFalse(controller.handoff_active(controls.DiskIO(self.f['release'],op)))

    def test_a_withdrawn_handoff_admits_nothing(self):
        op=self.f['operation'];(op/'root-handoff-withdrawn.json').write_text('{}\n');(op/'root-handoff-withdrawn.json').chmod(0o600)
        with self.assertRaises(ValueError) as caught:controller.lifetime.export_operation(op,self.f['now'],self.f['profile']['worker'],self.f['profile'])
        self.assertEqual(str(caught.exception),'Root handoff withdrawn')
        # A withdrawal cancels the handoff even after its admission receipt, since
        # the controller writes one only when it did not return a standing handoff.
        io=controls.DiskIO(self.f['release'],op);self.assertFalse(controller.handoff_active(io))


class Op4PhaseReplay(unittest.TestCase):
    """Each phase's own checks against what the worker reported in op-3 and op-4."""
    def setUp(self):
        import controls
        self.O=controls.OBSERVER;self.O['bind_profile'](controls.synthetic_profile())
        self.e=json.loads((Path(__file__).parent/'fixtures/boat-phase-evidence-op4.json').read_text())

    def runtime(self,props,seconds):
        cg='0::'+props['ControlGroup'];self.O['validate_runtime'](props['ControlGroup'].rsplit('/',1)[-1],int(props['MainPID']),cg,props['MemoryMax'],props['MemorySwapMax'],props,seconds)

    def test_systemd_255_unit_properties_pass_every_phase_runtime_check(self):
        self.runtime(self.e['startup']['loaded_properties'],32*60+56);self.runtime(self.e['stopcheck']['loaded_properties'],20)
        # The metadata unit's 240 seconds, as systemd 255 formats them.
        metadata=dict(self.e['stopcheck']['loaded_properties'],RuntimeMaxUSec='4min',ControlGroup=self.e['stopcheck']['loaded_properties']['ControlGroup'].replace('stopcheck','metadata'))
        self.runtime(metadata,240)
        with self.assertRaises(Exception):self.runtime(dict(metadata,RuntimeMaxUSec='4min 1s'),240)

    def test_op4_startup_and_stopcheck_ends_are_healthy(self):
        import common
        for phase in ('startup','stopcheck'):
            x=self.e[phase];result={'unit':x['unit'],'phase_memory':x['phase_memory']}
            ok,code,evidence=common.terminal_health(result,x['terminal_properties'],True,0)
            self.assertTrue(ok,(phase,code));self.assertEqual(evidence,x['terminal_evidence'])
            self.O['validate_pressure'](x['memory_events'],'0')

    def test_the_op3_tree_has_no_startup_or_job_work(self):
        d=json.loads((Path(__file__).parent/'fixtures/boat-session-tree-op3.json').read_text())
        rows={r['pid']:dict(r,group=r['pid']) for r in d['rows']};me=d['own_session'][-1]
        manager=[(r['pid'],r['start']) for r in rows.values() if r['comm']=='systemd' and r['uid']==1000 and r['ppid']==1]
        self.assertEqual(len(manager),1)
        self.assertEqual(self.O['foreign_startup'](rows,{'pid':me,'start':rows[me]['start']},me,set(),manager[0]),[])


class ProviderSessionTrees(unittest.TestCase):
    """The session allowance against the trees observed on bx_gy2ytgh8."""
    def setUp(self):
        import controls
        self.O=controls.OBSERVER;self.controls=controls
        self.O['bind_profile'](controls.synthetic_profile())
        d=json.loads((Path(__file__).parent/'fixtures/boat-session-tree-op2.json').read_text())
        self.rows={r['pid']:dict(r,group=r['pid']) for r in d['rows']};self.me=d['own_session'][-1]
        self.manager=(1411,984)

    def busy(self,rows=None,manager='default',tracked=None):
        rows=self.rows if rows is None else rows;manager=self.manager if manager=='default' else manager
        return {x['pid'] for x in self.O['foreign_startup'](rows,{'pid':self.me,'start':rows[self.me]['start']},self.me,set() if tracked is None else tracked,manager)}

    def add(self,rows,pid,ppid,comm,uid=1000):
        rows[pid]={'pid':pid,'ppid':ppid,'uid':uid,'comm':comm,'start':50,'group':pid};return rows

    def test_the_op2_tree_is_all_provider_session(self):
        self.assertEqual(self.rows[self.manager[0]]['comm'],'systemd')
        self.assertEqual(self.busy(),set())

    def test_the_run1_listing_still_refuses_its_unparented_processes(self):
        rows=self.controls.observed_session_rows()
        busy={x['pid'] for x in self.O['foreign_startup'](rows,{'pid':11,'start':1},12,set(),(1220,5))}
        self.assertEqual(busy,self.controls.UNKNOWN_SESSION|set(self.controls.UNPARENTED))
        for pid in self.controls.UNKNOWN_SESSION:del rows[pid]
        for pid in self.controls.UNPARENTED:del rows[pid]
        self.assertEqual({x['pid'] for x in self.O['foreign_startup'](rows,{'pid':11,'start':1},12,set(),(1220,5))},set())

    def test_both_observed_parents_of_the_agent_service(self):
        rows=copy.deepcopy(self.rows);rows[16278]['ppid']=self.add(rows,16000,1,'ascii-agent-sup')[16000]['pid']
        self.assertEqual(self.busy(rows),set())
        rows[16000]['comm']='ascii-agentsup2';self.assertEqual(self.busy(rows),{16000,16278,16315,16321,19827})

    def test_the_agent_shell_and_sudo_are_bound_by_their_children(self):
        rows=self.add(copy.deepcopy(self.rows),30000,19827,'cargo')
        self.assertEqual(self.busy(rows),{19827,30000})
        rows=self.add(copy.deepcopy(self.rows),30000,16315,'apt-get',0)
        self.assertEqual(self.busy(rows),{16315,16321,30000})
        rows=copy.deepcopy(self.rows);rows[16321]['uid']=1000
        self.assertEqual(self.busy(rows),{16315,16321})
        # A shell once tracked stays tracked after its child ends.
        tracked=set();rows=self.add(copy.deepcopy(self.rows),30000,19827,'cargo')
        self.assertEqual(self.busy(rows,tracked=tracked),{19827,30000})
        del rows[30000];self.assertEqual(self.busy(rows,tracked=tracked),{19827})

    def test_each_chain_is_bound_by_name_uid_and_every_ancestor(self):
        cases=[('dbus-run-sessio under one lightdm',lambda r:r[20391].update(ppid=20136),{20391,20700,20734,20741}),
               ('lightdm of the worker uid',lambda r:r[20371].update(uid=1000),{20371,20391,20700,20734,20741}),
               ('fusermount3 under gvfsd',lambda r:r[14101].update(ppid=[p for p,x in r.items() if x['comm']=='gvfsd'][0]),{14101}),
               ('agent service under a shell',lambda r:r[16278].update(ppid=self.add(r,16000,1,'bash')[16000]['pid']),{16000,16278,16315,16321,19827}),
               ('sudo outside the agent service',lambda r:r[16315].update(ppid=1),{16315,16321}),
               ('bash under the user manager',lambda r:r[19827].update(ppid=self.manager[0]),{19827})]
        for name,change,expected in cases:
            with self.subTest(name):
                rows=copy.deepcopy(self.rows);change(rows);self.assertEqual(self.busy(rows),expected)
        # Without the system manager's own identity, its children are not allowed.
        self.assertIn(14086,self.busy(manager=None))

class WatchIO(ExtendIO):
    """The fixture's warm operation with a scripted worker job-closure check."""
    def __init__(self,f,wall=None,boot=None,checks=None):
        super().__init__(f,wall,boot);self.checks=list(checks or []);self.check_times=[];self.frozen_at=[];self.local_seen=[]
        self.frozen=False;self.freeze_delay=0;self.on_freeze=None;self.on_check=None;self.check_seconds=[]
    lock_jobs=controller.RealIO.lock_jobs
    unlock_jobs=controller.RealIO.unlock_jobs
    def job_closure(self,local_units,seconds=60):
        self.check_times.append((self.w,self.t,self.frozen));self.local_seen.append(tuple(local_units));self.check_seconds.append(seconds)
        if self.on_check:self.on_check(len(self.check_times))
        state=self.checks.pop(0) if self.checks else 'closed'
        if state=='error':raise OSError('provider said /home/private')
        row={'list_exit':0,'units':[],'unit_count':0,'processes':[],'process_count':0,'boot_id':WORKER_BOOT_ID,'local_units_closed':{u:True for u in local_units}}
        if state=='unit':row.update(units=['nsc-build-a1.scope'],unit_count=1)
        if state=='process':row.update(processes=[[4242,'rustc']],process_count=1)
        if state=='boot':row['boot_id']='11111111-2222-3333-4444-555555555555'
        if state=='local':row['local_units_closed']={u:False for u in local_units}
        return row
    def freeze_extensions(self):
        if self.freeze_delay:self.wait(self.freeze_delay)
        if self.on_freeze:self.on_freeze()
        self.frozen_at.append(self.t);self.frozen=True;return True
    def thaw_extensions(self):self.frozen=False


class ClosureWatch(unittest.TestCase):
    """A warm worker's scheduled job-closure check and finish before its deadline."""
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.f=fixture(self.root);controller.bind_release_profile(self.f['release'])
        import qualified_worker_admission as qwa;self.qwa=qwa
        self.op=self.f['operation'];self.intent=json.loads((self.op/'controller-intent.json').read_text())
        self.end=controller.watch_ends(self.f['release'],ExtendIO(self.f))
        self.observed=[];self.addCleanup(env.release_job_locks)

    def watch(self,io,lead=900,local_units=()):
        return controller.closure_watch(self.f['release'],io,lead,local_units,observe=lambda r,x:self.observed.append(x.exists('stop-trigger.json')))

    def read(self,name):return json.loads((self.op/name).read_text())
    def test_a_withdrawn_handoff_is_never_watched(self):
        w=self.op/'root-handoff-withdrawn.json';w.write_text('{}\n');w.chmod(0o600)
        with self.assertRaises(Exception) as caught:self.watch(ExtendIO(self.f))
        self.assertIn('no qualified Root handoff to watch',str(caught.exception))
    def left(self,w,t,end=None):
        wall_end,boot_end,_=end or self.end;return min(wall_end-w,boot_end-t)

    def extend(self,at_left,seconds=3600,accept=True):
        io=ExtendIO(self.f,self.f['now']+self.left(self.f['now'],self.f['boot_now'])-at_left,self.f['boot_now']+self.left(self.f['now'],self.f['boot_now'])-at_left)
        path=environment_fact(self.f,self.root/'environment-fact.json',io.wall())
        with patch.object(self.qwa,'current_boot_id',return_value=io.local_boot):
            receipt=controller.extend_operation(self.f['release'],io,seconds,path,'second lane on the warm worker')
        if accept:controller.guard_deadlines(ExtendIO(self.f,io.wall(),io.mono()),self.intent)
        return io,receipt

    def admit(self,io):
        with patch.object(self.qwa,'boottime',return_value=io.mono()):
            return env.load_job_admission(self.f['admission'],self.f['profile_path'],self.f['profile']['worker'],io.wall(),lambda unit:self.f['guard'])

    def test_closed_jobs_finish_at_the_lead_before_the_end(self):
        io=WatchIO(self.f)
        self.assertEqual(self.watch(io),'FINISHED')
        # A check outside the locks and the publishing check under them.
        self.assertEqual(len(io.check_times),2);w,t,_=io.check_times[0]
        self.assertLessEqual(self.left(w,t),900);self.assertGreater(self.left(w,t),900-31)
        self.assertEqual([x[2] for x in io.check_times],[False,True]);self.assertLessEqual(io.check_seconds[1],controller.WATCH_DECISION)
        closed=self.read('root-job-closed.json')
        self.assertEqual(closed['closure_check'],'closure-watch-check-0002.json');self.assertTrue(closed['all_native_and_leases_closed'])
        self.assertEqual(closed['receipt_sha256'],self.qwa.receipt_digest(self.read('closure-watch-check-0002.json')))
        controller.matching_root_job_closed(self.f['release'],io,closed)
        self.assertTrue(io.exists('stop-trigger.json'));self.assertEqual(self.observed,[True]);self.assertEqual(io.calls,[])
        self.assertEqual(len(io.frozen_at),1);self.assertFalse(io.frozen);self.assertIsNone(io._jobs)
        with self.assertRaisesRegex(Exception,'stopping or closed'):self.admit(io)

    def test_a_live_command_holds_off_closure(self):
        io=WatchIO(self.f,checks=['closed']*400)
        self.admit(io)
        self.assertEqual(self.watch(io),'UNRESOLVED_JOB_OPEN');self.assertEqual(self.read('closure-watch-unresolved.json')['reason_code'],'job_command_live')
        self.assertFalse(io.exists('root-job-closed.json'))
        env.release_job_locks()

    def test_a_command_waits_through_the_watch_hold_and_then_sees_the_stop(self):
        io=WatchIO(self.f);held=[]
        def during(n):
            if n==2:
                with self.assertRaisesRegex(Exception,'held by the closure watch'):env.hold_job_lock(self.op,0.3)
                held.append(True)
        io.on_check=during
        self.assertEqual(self.watch(io),'FINISHED');self.assertEqual(held,[True])
        env.hold_job_lock(self.op,0.3);env.release_job_locks()

    def test_a_change_during_the_final_check_prevents_publication(self):
        for change in ['unresolved','trigger','slow']:
            with self.subTest(change=change):
                self.setUp();io=WatchIO(self.f,checks=['closed']*400)
                def during(n,io=io,change=change):
                    if n==2:
                        if change=='unresolved':io.record('unresolved.json',{})
                        if change=='trigger':io.trigger()
                        if change=='slow':io.wait(controller.WATCH_DECISION+15)
                io.on_check=during
                result=self.watch(io)
                self.assertEqual(result,{'unresolved':'GUARD_UNRESOLVED','trigger':'UNRESOLVED_JOB_OPEN','slow':'FINISHED'}[change])
                if change=='slow':self.assertNotEqual(self.read('root-job-closed.json')['closure_check'],'closure-watch-check-0002.json')
                else:self.assertFalse(io.exists('root-job-closed.json'),change)

    def test_an_extension_accepted_during_the_final_check_prevents_publication(self):
        io=WatchIO(self.f,checks=['closed']*400)
        def during(n):
            if n==2:
                ext,receipt=self.extend(self.left(io.w,io.t),3600,accept=False)
                controller.guard_deadlines(ExtendIO(self.f,io.w,io.t),self.intent);self.receipt=receipt
        io.on_check=during
        self.assertEqual(self.watch(io),'FINISHED')
        self.assertNotEqual(self.read('root-job-closed.json')['closure_check'],'closure-watch-check-0002.json')
        self.assertLessEqual(self.receipt['boot_end']-io.check_times[-1][1],900)

    def test_a_job_started_while_the_lock_is_awaited_is_not_closed_over(self):
        io=WatchIO(self.f,checks=['closed','unit','closed','closed']);io.freeze_delay=90
        self.assertEqual(self.watch(io),'FINISHED')
        self.assertFalse(self.read('closure-watch-check-0002.json')['closed'])
        self.assertEqual(self.read('root-job-closed.json')['closure_check'],'closure-watch-check-0004.json')
        self.assertTrue(io.check_times[3][2])

    def test_open_jobs_are_checked_again_until_they_close(self):
        io=WatchIO(self.f,checks=['unit','process','error','closed','closed'])
        self.assertEqual(self.watch(io),'FINISHED')
        self.assertEqual(len(io.check_times),5);self.assertEqual(io.check_times[1][1]-io.check_times[0][1],30)
        self.assertFalse(self.read('closure-watch-check-0001.json')['closed']);self.assertFalse(self.read('closure-watch-check-0002.json')['closed'])
        self.assertFalse(io.exists('closure-watch-check-0003.json'));self.assertTrue(self.read('closure-watch-check-0004.json')['closed'])
        self.assertEqual(self.read('root-job-closed.json')['closure_check'],'closure-watch-check-0005.json')

    def test_a_job_that_never_closes_leaves_the_guard_stop_path(self):
        for state in ['unit','boot','local']:
            with self.subTest(state=state):
                self.setUp();io=WatchIO(self.f,checks=[state]*400)
                self.assertEqual(self.watch(io,local_units=('lane-screen.service',)),'UNRESOLVED_JOB_OPEN')
                self.assertFalse(io.exists('root-job-closed.json'));self.assertFalse(io.exists('stop-trigger.json'));self.assertEqual(self.observed,[])
                q=self.read('closure-watch-unresolved.json');self.assertEqual(q['reason_code'],'job_open')
                w,t,_=io.check_times[-1]
                self.assertLessEqual(self.left(w,t),controller.WATCH_FLOOR+30)
                self.assertEqual(io.local_seen[0],('lane-screen.service',))

    def test_an_error_on_every_check_is_coded_without_its_text(self):
        io=WatchIO(self.f,checks=['error']*400)
        self.assertEqual(self.watch(io),'UNRESOLVED_JOB_OPEN')
        code=self.read('closure-watch-unresolved.json')['reason_code']
        self.assertTrue(code.startswith('unexpected:OSError:'));self.assertNotIn('private',code)

    def test_an_accepted_extension_moves_the_check_to_its_new_end(self):
        io,receipt=self.extend(600,3600)
        watch=WatchIO(self.f,io.wall(),io.mono())
        self.assertEqual(self.watch(watch),'FINISHED')
        w,t,_=watch.check_times[0];wall_end,boot_end,_=self.end
        self.assertLessEqual(receipt['boot_end']-t,900);self.assertGreater(t,boot_end)

    def test_an_unaccepted_extension_is_awaited_then_grants_nothing(self):
        io,_=self.extend(600,3600,accept=False)
        watch=WatchIO(self.f,io.wall(),io.mono())
        self.assertTrue(controller.watch_ends(self.f['release'],watch)[2])
        self.assertEqual(self.watch(watch),'FINISHED')
        self.assertGreaterEqual(watch.check_times[-1][1]-watch.frozen_at[0],controller.WATCH_ACCEPT)

    def test_an_extension_accepted_while_waiting_keeps_the_worker(self):
        io,receipt=self.extend(600,3600,accept=False)
        watch=WatchIO(self.f,io.wall(),io.mono());f=self.f;intent=self.intent;original=watch.wait
        def wait(n):
            original(n)
            if watch.frozen and not watch.exists('lifetime-extension-0001-accepted.json'):controller.guard_deadlines(ExtendIO(f,watch.w,watch.t),intent)
        watch.wait=wait
        self.assertEqual(self.watch(watch),'FINISHED')
        self.assertEqual(len(watch.frozen_at),2);self.assertLessEqual(receipt['boot_end']-watch.check_times[-1][1],900)

    def test_real_one_second_wait_still_spaces_the_checks(self):
        io=WatchIO(self.f,checks=['unit','closed']);step=io.wait
        io.wait=lambda n:step(min(n,1))
        self.assertEqual(self.watch(io),'FINISHED');self.assertGreaterEqual(io.check_times[1][1]-io.check_times[0][1],30)

    def test_existing_closure_is_observed_and_terminal_states_end_the_watch(self):
        io=WatchIO(self.f);io.record('closed.json',{'worker':'x'})
        self.assertEqual(self.watch(io),'CLOSED');self.assertEqual(io.check_times,[])
        self.setUp();io=WatchIO(self.f);io.record('unresolved.json',{})
        self.assertEqual(self.watch(io),'GUARD_UNRESOLVED')
        self.setUp();io=WatchIO(self.f);io.trigger()
        self.assertEqual(self.watch(io),'UNRESOLVED_JOB_OPEN');self.assertEqual(self.read('closure-watch-unresolved.json')['reason_code'],'stop_triggered_without_job_closure')
        # The closure phase keeps its refusal through the shared record.
        q=self.read('closure-refused.json');self.assertEqual((q['phase'],q['reason_code']),('closure','closure_watch:stop_triggered_without_job_closure'))
        # A stop trigger that appears while the lock is awaited prevents publication.
        self.setUp();io=WatchIO(self.f);io.on_freeze=io.trigger
        self.assertEqual(self.watch(io),'UNRESOLVED_JOB_OPEN');self.assertFalse(io.exists('root-job-closed.json'))

    def test_one_watch_per_operation_and_bounded_arguments(self):
        io=WatchIO(self.f);self.watch(io)
        with self.assertRaises(FileExistsError):self.watch(WatchIO(self.f))
        for lead,units in [(299,()),(3601,()),('900',()),(900,('bad unit',))]:
            self.setUp()
            with self.assertRaises(controller.Refusal):self.watch(WatchIO(self.f),lead,units)


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

    def test_a_guard_collected_after_its_clean_exit_is_closed(self):
        # Item 776. The guard's transient unit is unloaded once it exits cleanly, so a
        # finish that reads it after the collection sees a not-found unit with no
        # invocation. That is the original guard's clean exit, not a replacement.
        with tempfile.TemporaryDirectory()as tmp:
            f=fixture(Path(tmp));r=f['release'];worker=f['profile']['worker'];now=f['now']
            from datetime import datetime,timezone
            iso=lambda t:datetime.fromtimestamp(t,timezone.utc).isoformat().replace('+00:00','Z')
            safe={'id':worker,'access':'owner','environment':f['profile']['future_environment']['name'],'state':'stopped','type':'large','vcpu':8,'memoryGB':16,
                  'snapshotAvailable':True,'lastSnapshotStatus':'completed','snapshotCompletedAt':iso(now+2),'stoppedAt':iso(now+2),
                  'stop':{'id':'stop_fixture','status':'completed','error':None,'forced':False,'endedAt':iso(now+2)}}
            closed={'worker':worker,'stop_operation':'stop_fixture','safe_info':safe,'stopped_confirmed':True,'owned_metadata_closed':True,'guard_may_exit':True}
            gone=str(Path(tmp)/'collected-guard-cgroup')
            def io_with(live):
                class IO:
                    def __init__(self):self.t=0;self.records={'closed.json':closed,'stop-intent.json':{'requested_wall':now+1},
                                                              'guard-loaded.json':{'boot_id':f['qualified']['local_guard']['boot_id'],'loaded':{'InvocationID':'a'*32,'ControlGroup':gone}}}
                    def mono(self):return self.t
                    def read(self,n):return self.records[n]
                    def exists(self,n):return n in self.records
                    def wait(self,n):self.t+=1
                    def show(self,u):return dict(live,Id=u)
                return IO()
            collected={'LoadState':'not-found','ActiveState':'inactive','SubState':'dead','MainPID':'0','ExecMainStatus':'0','InvocationID':'','ControlGroup':''}
            with patch.object(controller.Path,'read_text',return_value=f['qualified']['local_guard']['boot_id']+'\n'):
                result=controller.await_stop(r,io_with(collected),seconds=2)
                self.assertEqual(result,{'worker':worker,'stopped_confirmed':True,'guard_closed':True,'stop_operation':'stop_fixture'})
                # A loaded unit with another invocation is still a replacement.
                for live in (dict(collected,LoadState='loaded',InvocationID='b'*32),dict(collected,LoadState='loaded',ActiveState='active',SubState='running',MainPID='7',InvocationID='b'*32),
                             dict(collected,LoadState='not-found',ActiveState='active'),dict(collected,InvocationID='b'*32)):
                    with self.assertRaisesRegex(controller.Refusal,'guard replaced during finish|verified stop not observed'):controller.await_stop(r,io_with(live),seconds=2)

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


def op5_fixture(root):
    import boat_fixtures
    return boat_fixtures.fixture(root,OP5_CLOCKS)


class Op5Clocks:
    """Run a test class again on an operation driven with moving clocks at op-5's times."""
    def run(self,result=None):
        with patch.object(sys.modules[__name__],'fixture',op5_fixture):return super().run(result)


class Op5EnvironmentAdmission(Op5Clocks,EnvironmentAdmission):pass
class Op5LifetimeExtension(Op5Clocks,LifetimeExtension):pass
class Op5RemoteTransport(Op5Clocks,RemoteTransport):pass
class Op5JobClosureProgram(Op5Clocks,JobClosureProgram):pass
class Op5ClosureWatch(Op5Clocks,ClosureWatch):pass
class Op5Finish(Op5Clocks,Finish):pass


class SourceDriftRecord(unittest.TestCase):
    """A metadata source drift names each recorded field next to the observed one."""
    RECORDED={'head':'8'*40,'status_sha256':'e'*64,'tracked_diff_sha256':'e'*64,'inputs':{'mise.toml':'c'*64,'rust-toolchain.toml':'f'*64}}
    def setUp(self):
        self.O=controls.OBSERVER;self.O['bind_profile'](controls.synthetic_profile())

    def observed(self,**change):
        row=json.loads(json.dumps(self.RECORDED));row.update(change);return row

    def test_a_drift_names_exactly_the_differing_fields(self):
        error=self.O['SourceDrift']('existing source drift','/home/worker-fixture/sparkles',self.RECORDED,self.observed(status_sha256='1'*64,tracked_diff_sha256='2'*64))
        d=error.source_drift;self.assertEqual(str(error),'existing source drift')
        self.assertEqual(sorted(k for k,v in d['fields'].items() if v['differs']),['status_sha256','tracked_diff_sha256'])
        self.assertEqual(d['fields']['status_sha256'],{'recorded':'e'*64,'observed':'1'*64,'differs':True})
        self.assertEqual(d['fields']['head'],{'recorded':'8'*40,'observed':'8'*40,'differs':False})
        self.assertFalse(d['other_fields_differ'])
        # A row with a field outside the identity is flagged without its content.
        extra=self.O['SourceDrift']('existing source drift','/home/worker-fixture/sparkles',self.RECORDED,dict(self.RECORDED,note='DO_NOT_COPY')).source_drift
        self.assertTrue(extra['other_fields_differ']);self.assertNotIn('DO_NOT_COPY',json.dumps(extra))

    def test_the_phase_result_keeps_the_drift(self):
        def qualify(payload):raise self.O['SourceDrift']('existing source drift','/home/worker-fixture/sparkles',self.RECORDED,self.observed(head='9'*40))
        result,closed=self.O['run_phase']({'phase':'metadata'},None,qualify)
        self.assertFalse(closed);self.assertEqual(result['reason_code'],'refusal:existing source drift')
        self.assertTrue(result['source_drift']['fields']['head']['differs'])

    def test_the_metadata_refusal_records_the_drift_bounded(self):
        r=controls.fixture();io=controls.InertIO(r);controller.bind_release_profile(r)
        drift=self.O['SourceDrift']('existing source drift','/home/worker-fixture/sparkles',self.RECORDED,self.observed(head='9'*40)).source_drift
        drift['fields']['head']['observed']='$(rm -rf /)';drift['secret']='DO_NOT_COPY';drift['repository']='DO NOT COPY; $(x)'
        controller.record_refusal(io,'metadata',controller.Refusal('metadata refused: probe_not_qualified'),{'qualified':False,'reason_code':'refusal:existing source drift','source_drift':drift},failed=['probe_not_qualified'])
        row=io.read('metadata-refused.json');self.assertNotIn('DO_NOT_COPY',json.dumps(row))
        self.assertEqual(row['source_drift']['fields']['head'],{'recorded':'8'*40,'observed':None,'differs':True})
        self.assertIsNone(row['source_drift']['repository']);self.assertEqual(set(row['source_drift']),{'repository','fields','other_fields_differ'})

    def git(self,repo,*args):
        import subprocess as sp;return sp.run(['git','-C',str(repo),*args],check=True,capture_output=True,timeout=30).stdout

    def repository(self):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup);repo=Path(tmp.name)/'repo';repo.mkdir()
        self.git(repo,'init','-q');(repo/'run.sh').write_text('true\n');(repo/'run.sh').chmod(0o755);(repo/'a.txt').write_text('a\n')
        self.git(repo,'add','.');self.git(repo,'-c','user.name=t','-c','user.email=t@invalid','-c','commit.gpgsign=false','commit','-qm','x')
        return repo

    def test_new_inodes_and_mtimes_leave_the_content_identity_unchanged(self):
        # Every resume so far gave the home filesystem a new device and new inodes. A
        # copy with new inodes and mtimes leaves the checkout's content identity clean.
        import shutil as sh;repo=self.repository();copy=repo.parent/'copy';sh.copytree(repo,copy,copy_function=sh.copy)
        for path in copy.rglob('*'):
            if '.git' not in path.parts and path.is_file():os.utime(path,(1577836800,1577836800))
        self.assertNotEqual(os.stat(repo/'a.txt').st_ino,os.stat(copy/'a.txt').st_ino)
        self.assertEqual(self.git(copy,'status','--porcelain=v1','-z'),b'')
        self.assertEqual(self.git(repo,'status','--porcelain=v1','-z'),self.git(copy,'status','--porcelain=v1','-z'))
        self.assertEqual(self.git(repo,'diff','HEAD','--binary'),self.git(copy,'diff','HEAD','--binary'))


class SourceDriftBounds(SourceDriftRecord):
    """The drift summary keeps its reason code, its time and output bounds, and git's formats."""
    def test_both_drift_reason_codes_survive_into_the_receipt(self):
        for reason in ('existing source drift','existing source changed during qualification'):
            r=controls.fixture();io=controls.InertIO(r);controller.bind_release_profile(r)
            controller.record_refusal(io,'metadata',controller.Refusal('metadata refused: probe_not_qualified'),{'qualified':False,'reason_code':'refusal:'+reason},failed=['probe_not_qualified'])
            self.assertEqual(io.read('metadata-refused.json')['remote_reason_code'],'refusal:'+reason)


def diagnostic_module():
    import importlib.util as u
    spec=u.spec_from_file_location('diagnostic_probe',Path(__file__).resolve().parents[1]/'boat/diagnostic-probe.py');d=u.module_from_spec(spec);spec.loader.exec_module(d);return d


class UntrackedDiagnostics(unittest.TestCase):
    """The diagnostic hold names a startup checkout's untracked, unignored paths."""
    def git(self,repo,*args):
        import subprocess as sp;return sp.run(['git','-C',str(repo),*args],check=True,capture_output=True,timeout=30).stdout

    def repository(self):
        tmp=tempfile.TemporaryDirectory();self.addCleanup(tmp.cleanup);repo=Path(tmp.name)/'repo';repo.mkdir()
        self.git(repo,'init','-q');(repo/'.gitignore').write_text('target/\n*.log\n');(repo/'a.txt').write_text('a\n')
        self.git(repo,'add','.');self.git(repo,'-c','user.name=t','-c','user.email=t@invalid','commit','-qm','x')
        return repo

    def test_untracked_paths_are_listed_with_sizes_and_mtimes(self):
        d=diagnostic_module();repo=self.repository()
        (repo/'target').mkdir();(repo/'target/x').write_text('ignored');(repo/'run.log').write_text('ignored')
        (repo/'new.txt').write_text('12345');os.utime(repo/'new.txt',(1700000000,1700000000))
        (repo/'.mise').mkdir();(repo/'.mise/state').write_text('s')
        (repo/'.env').write_text('KEY=DO_NOT_COPY');(repo/'deploy-token.txt').write_text('DO_NOT_COPY');(repo/'id_ed25519').write_text('DO_NOT_COPY')
        row=d.untracked([str(repo)])[str(repo)]
        paths={p['path']:p for p in row['paths']}
        self.assertEqual(sorted(paths),['.mise/','new.txt'])
        self.assertEqual((paths['new.txt']['kind'],paths['new.txt']['bytes'],paths['new.txt']['mtime_ns']),('file',5,1700000000*10**9))
        self.assertEqual(paths['.mise/']['kind'],'directory')
        self.assertEqual((row['omitted_credential_like'],row['total'],row['truncated']),(3,5,False))
        self.assertNotIn('DO_NOT_COPY',json.dumps(row));self.assertNotIn('deploy-token',json.dumps(row))

    def test_the_listing_stops_at_its_bound(self):
        d=diagnostic_module();repo=self.repository()
        for n in range(d.UNTRACKED_MAX+50):(repo/('f%04d'%n)).write_text('x')
        row=d.untracked([str(repo)])[str(repo)]
        self.assertEqual((len(row['paths']),row['total'],row['truncated']),(d.UNTRACKED_MAX,d.UNTRACKED_MAX+50,True))

    def test_a_missing_checkout_is_reported_not_raised(self):
        d=diagnostic_module();row=d.untracked(['/nonexistent/checkout'])['/nonexistent/checkout']
        self.assertIn('unread',row);self.assertEqual(row['paths'],[])

    def test_collect_reports_the_untracked_paths_of_the_given_checkouts(self):
        d=diagnostic_module();repo=self.repository();(repo/'new.txt').write_text('n')
        with tempfile.TemporaryDirectory() as t:
            home=Path(t);(home/'.local/state/sparkles-boat').mkdir(parents=True)
            with patch.object(d,'units',return_value={'rows':[]}):body=d.collect(home,os.getuid(),[str(repo)])
        self.assertEqual([p['path'] for p in body['untracked'][str(repo)]['paths']],['new.txt'])

    def test_the_hold_passes_the_startup_checkouts(self):
        controller.bind_profile(controls.synthetic_profile());seen=[]
        class IO(controller.RealIO):
            def __init__(self):pass
            def remote_python(self,program,args,seconds):seen.append(args);return {'exit':0,'error':None,'body':{'schema':1}}
        IO().diagnose(30)
        self.assertEqual(seen,[sorted(set(controller.profile()['startup_repositories'].values()))])


class RemoteSessionDirectory(unittest.TestCase):
    """Every provider session starts its program from the worker's state directory, never a checkout."""
    def test_remote_python_starts_in_the_state_directory(self):
        p=controller.bind_profile(controls.synthetic_profile());seen=[]
        class IO(controller.RealIO):
            def __init__(self):pass
            def boat(self,args,seconds=20,input_bytes=None,json_mode=True):seen.append(args);return {'exit':0,'error':None,'body':{}}
        IO().remote_python('print(1)',[],10)
        self.assertEqual(seen[0][3:],['/usr/bin/env','-C',p['worker_home'],'/usr/bin/python3','-I','-B','-'])

    def test_no_session_starts_python_in_the_landing_directory(self):
        root=Path(__file__).resolve().parents[1]
        for name in ('boat/controller.py','boat/job_environment.py'):
            text=(root/name).read_text()
            self.assertNotRegex(text,r"'--',\s*'/usr/bin/python3'",name)


class UntrackedRoundOne(UntrackedDiagnostics):
    """Credential parents, the git output bound, unbound profiles and an absent state directory."""
    def test_a_credential_like_parent_hides_its_children(self):
        d=diagnostic_module();repo=self.repository()
        for parent in ('.ssh','secrets','config/tokens'):
            (repo/parent).mkdir(parents=True);(repo/parent/'tracked').write_text('t')
        self.git(repo,'add','-f','.');self.git(repo,'-c','user.name=t','-c','user.email=t@invalid','commit','-qm','y')
        for parent in ('.ssh','secrets','config/tokens'):(repo/parent/'DO_NOT_COPY_child').write_text('x')
        (repo/'plain.txt').write_text('p')
        row=d.untracked([str(repo)])[str(repo)]
        self.assertEqual([p['path'] for p in row['paths']],['plain.txt']);self.assertEqual(row['omitted_credential_like'],3)
        self.assertNotIn('DO_NOT_COPY',json.dumps(row))

    def test_git_output_is_read_within_a_byte_bound(self):
        import subprocess as sp,shutil as sh
        d=diagnostic_module();repo=self.repository();made=[];real=sp.Popen
        def popen(argv,*a,**k):
            child=real([sh.which('head'),'-c','50000000','/dev/zero'],*a,**k);made.append(child);return child
        with patch.object(d.subprocess,'Popen',popen),patch.object(d.subprocess,'run',side_effect=AssertionError('unbounded run')):
            row=d.untracked([str(repo)])[str(repo)]
        self.assertTrue(row['truncated']);self.assertEqual(len(made),1);self.assertIsNotNone(made[0].poll())

    def test_the_session_entry_needs_no_bound_profile(self):
        import common
        saved=common._PROFILE;common._PROFILE=None
        try:self.assertEqual(controller.remote_entry('/home/user'),['/usr/bin/env','-C','/home/user','/usr/bin/python3','-I','-B','-'])
        finally:common._PROFILE=saved
        self.assertIn("remote_entry(profile['worker_home'])",(Path(__file__).resolve().parents[1]/'boat/job_environment.py').read_text())


class UntrackedRoundTwo(unittest.TestCase):
    """Newlines in names, the exact byte cap and names at the cut."""
    def test_names_with_newlines_are_still_matched(self):
        d=diagnostic_module()
        for name in ('secret\nDO_NOT_COPY','config/tokens\n/DO_NOT_COPY','.env.prod\nDO_NOT_COPY'):
            self.assertTrue(d.credential_like(name),repr(name))
        self.assertFalse(d.credential_like('plain\nname.txt'))

    def test_the_cap_keeps_exit_status_and_whole_names(self):
        import shutil as sh
        d=diagnostic_module();sh_=sh.which('sh')
        run=lambda script,limit:d.bounded_git([sh_,'-c',script],limit,10)
        self.assertEqual(run("printf 'abcd'; exit 7",4),(b'abcd',False,7))
        self.assertEqual(run("printf 'abcd'",4),(b'abcd',False,0))
        self.assertEqual(run("printf 'abcde'",4),(b'abcd',True,None))
        self.assertEqual(run("printf 'ab'; exit 3",4),(b'ab',False,3))
        self.assertEqual(d.listed_names(b'a\0b\0',True),[b'a',b'b'])
        self.assertEqual(d.listed_names(b'a\0bc',True),[b'a'])
        self.assertEqual(d.listed_names(b'a\0b',False),[b'a',b'b'])


class UntrackedRoundThree(unittest.TestCase):
    """Backup and editor copies of credential names are omitted too."""
    def test_backup_suffixes_and_prefixes_are_matched(self):
        d=diagnostic_module()
        for name in ('.netrc.bak','.npmrc~','deploy.pem.bak','#.env#','.env.swp','old/.ssh.orig/x','id_ed25519.pub','server.key.old','.aws-backup/config','my-token-copy','secrets.tar.gz','.git-credentials~'):
            self.assertTrue(d.credential_like(name),repr(name))
        for name in ('plain.txt','.mise/state','src/main.rs','.venv/','__pycache__/','environment.md'):
            self.assertFalse(d.credential_like(name),repr(name))


class UntrackedRoundFour(UntrackedDiagnostics):
    """Every name that carries a credential dot-name anywhere is omitted and counted."""
    def test_env_names_anywhere_are_omitted(self):
        d=diagnostic_module()
        for name in ('.envrc','backup.env','backup.env.bak','x/.envrc/y','old.ssh/config','prod.aws.json'):
            self.assertTrue(d.credential_like(name),repr(name))
        repo=self.repository()
        for name in ('.envrc','backup.env','backup.env.bak','plain.txt'):(repo/name).write_text('DO_NOT_COPY')
        row=d.untracked([str(repo)])[str(repo)]
        self.assertEqual(([p['path'] for p in row['paths']],row['omitted_credential_like']),(['plain.txt'],3))
