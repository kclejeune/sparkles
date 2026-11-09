"""Provider contracts using bounded local fixtures, never provider qualification."""
import hashlib
import ctypes
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import sys
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch
from boat_fixtures import fixture as boat_fixture, ExtendIO, environment_fact

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('remote_executor', ROOT / 'scripts/remote-executor.py')
RX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RX)



REMOTE_CALLS={'rx_exec','rsh','screen_exec'}
WRAPPERS={'exec','timeout','nohup','env','setsid','taskset','chrt','nice','command','flock','systemd-run','xargs'}
def shell_builtins():
    import subprocess as sp
    return set(sp.run(['bash','-c','compgen -b; compgen -k'],capture_output=True,text=True,check=True).stdout.split())

def quoted_text(source,word):
    """The shell text inside one quoted word, unescaped as the outer shell would."""
    parts=word.get('Parts',[])
    if len(parts)!=1 or parts[0].get('Type') not in ('DblQuoted','SglQuoted'):return None
    raw=source.encode()[word['Pos']['Offset']+1:word['End']['Offset']-1].decode()
    if parts[0]['Type']=='DblQuoted':
        for a,b in (('\\"','"'),('\\$','$'),('\\`','`'),('\\\\','\\')):raw=raw.replace(a,b)
    return raw

def shell_commands(source,remote_only=False):
    """The literal external command names that a shell text runs.

    With remote_only, only the text passed to a remote call is read, recursively,
    including bash -c and sh -c arguments inside it."""
    import json as js,subprocess as sp
    builtins=shell_builtins();found=set();funcs=set()
    def parse(text):return js.loads(sp.run(['shfmt','--to-json'],input=text,capture_output=True,text=True,check=True).stdout)
    def literal(arg):
        parts=arg.get('Parts',[]);return parts[0].get('Value') if len(parts)==1 and parts[0].get('Type')=='Lit' else None
    def walk(node,text,local):
        if isinstance(node,dict):
            if node.get('Type')=='FuncDecl':funcs.add(node['Name']['Value'])
            # A heredoc fed to a shell, directly or through a wrapper such as
            # mbx sparkles-task, runs as shell code on the same host.
            cmd=node.get('Cmd')
            staged=isinstance(cmd,dict) and cmd.get('Type')=='CallExpr' and [literal(a) for a in cmd.get('Args',[])]==['cat'] and any(
                r.get('Op')=='>' and text.encode()[r['Word']['Pos']['Offset']:r['Word']['End']['Offset']].decode().rstrip('"').endswith('/remote.sh') for r in node.get('Redirs') or [])
            if (local or staged) and isinstance(cmd,dict) and cmd.get('Type')=='CallExpr' and (staged or {'bash','sh'}&{literal(a) for a in cmd.get('Args',[])}):
                for redirect in node.get('Redirs') or []:
                    body=redirect.get('Hdoc')
                    if redirect.get('Op') in ('<<','<<-') and body:
                        # The body's span ends with the delimiter line, which is not code.
                        lines=text.encode()[body['Pos']['Offset']:body['End']['Offset']].decode().splitlines()
                        delimiter=text.encode()[redirect['Word']['Pos']['Offset']:redirect['Word']['End']['Offset']].decode().strip('\'"')
                        if lines and lines[-1].strip()==delimiter:lines=lines[:-1]
                        visit('\n'.join(lines)+'\n',True)
            if node.get('Type')=='CallExpr' and node.get('Args'):
                args=node['Args'];words=[literal(a) for a in args];first=words[0]
                if first in REMOTE_CALLS:
                    inner=quoted_text(text,args[-1])
                    if inner is not None:visit(inner,True)
                if local:
                    i=0
                    while i<len(words) and words[i] is not None:
                        found.add(words[i])
                        if words[i] in WRAPPERS:
                            i+=1
                            # Options, their values and assignments; a computed command ends the scan.
                            while i<len(words) and (words[i] is None and words[i-1] is not None and words[i-1].startswith('-') or words[i] is not None and (words[i].startswith('-') or '=' in words[i] or words[i][:1].isdigit())):i+=1
                            continue
                        break
                    if first in ('bash','sh') and '-c' in words:
                        j=words.index('-c')
                        if j+1<len(args):
                            inner=quoted_text(text,args[j+1])
                            if inner is not None:visit(inner,True)
            for v in node.values():walk(v,text,local)
        elif isinstance(node,list):
            for v in node:walk(v,text,local)
    def visit(text,local):walk(parse(text),text,local)
    visit(source,not remote_only)
    return {w for w in found if w not in builtins and w not in funcs and w not in REMOTE_CALLS}

class Admission(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory();self.addCleanup(self.tmp.cleanup)
        self.root=Path(self.tmp.name);self.fixtures=boat_fixture(self.root)
        self.path=self.fixtures['admission'];self.value=self.fixtures['job']
        self.env=patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(self.path),SPARKLES_BOAT_PROFILE=str(self.fixtures['profile_path']))
        self.env.start();self.addCleanup(self.env.stop)
        self.clock=patch.object(RX.time,'time',return_value=self.fixtures['now'])
        self.clock.start();self.addCleanup(self.clock.stop)
        self.guard=patch.object(RX.boat_environment,'local_guard',return_value=self.fixtures['guard'])
        self.guard.start();self.addCleanup(self.guard.stop)
        # The fixture's BOOTTIME observation stands for the current boot clock.
        self.boot=patch.object(RX.boat_clock,'boottime',return_value=self.fixtures['boot_now'])
        self.boot.start();self.addCleanup(self.boot.stop)

    def test_exact_warm_identity_and_binary_safe_command(self):
        result=RX.command('boat:bx_fixture',"printf '%s' 'a;$(bad)' && cat")
        self.assertEqual(result[:4],[self.fixtures['release']['boat']['path'],'--no-update','ssh','bx_fixture'])
        # The provider passes each word after -- to the worker as one argument, quoting
        # it there (observed on bx_gy2ytgh8: `-- echo 'a  b' '$HOME'` printed `a  b $HOME`).
        # So bash -c receives the script itself, never a quoted copy of it.
        # Bash run over ssh reads ~/.bashrc even with -c, and the worker's prepends
        # /usr/local/cargo/bin to PATH (observed on bx_gy2ytgh8), so no shell reads it.
        # An inherited BASH_ENV or ENV would still run startup code, so both are cleared.
        words=result[result.index('--')+1:]
        self.assertEqual(words[:9],['/usr/bin/env','-u','BASH_ENV','-u','ENV','/bin/bash','--norc','--noprofile','-c']);self.assertEqual(len(words),10)
        script=words[9];self.assertFalse(script.startswith(("'",'"')),script[:20])
        self.assertIn('exec env -u BASH_ENV -u ENV ',script);self.assertIn(' /bin/bash --norc --noprofile -c ',script)
        import shutil as sh,subprocess,os,tempfile as tf
        with tf.TemporaryDirectory() as t:
            marker=Path(t)/'ran';startup=Path(t)/'startup';startup.write_text('touch '+shlex.quote(str(marker))+'\n')
            local=[sh.which('env'),*words[1:5],sh.which('bash'),*words[6:9],'echo ok']
            out=subprocess.run(local,env={'PATH':os.environ['PATH'],'BASH_ENV':str(startup),'ENV':str(startup)},capture_output=True,text=True,timeout=10)
            self.assertEqual(out.stdout,'ok\n');self.assertFalse(marker.exists())
        self.assertIn("a;$(bad)",script);self.assertNotIn('; . ',script)
        import subprocess as sp
        self.assertEqual(sp.run(['bash','-n','-c',script],capture_output=True,timeout=10).returncode,0)
        self.assertFalse(any(action in result for action in ('create','resume','stop','snapshot')))
        # The session leaves the provider's landing directory before it runs anything.
        self.assertTrue(script.startswith('set -e; cd '+shlex.quote(self.fixtures['profile']['worker_home'])+'; '),script[:80])

    def test_identity_expiry_unknown_schema_and_guard_replacement_refuse(self):
        for changed in (dict(worker='bx_other'),dict(version=2),dict(unexpected='NOT_A_SECRET')):
            self.path.write_text(json.dumps({**self.value,**changed}))
            with self.assertRaises(ValueError):RX.admission('boat:bx_fixture')
        self.path.write_text(json.dumps(self.value))
        with patch.object(RX.time,'time',return_value=self.fixtures['now']+10800),self.assertRaises(ValueError):
            RX.admission('boat:bx_fixture')
        with patch.object(RX.boat_environment,'local_guard',return_value={**self.fixtures['guard'],'InvocationID':'d'*32}),self.assertRaises(ValueError):
            RX.admission('boat:bx_fixture')

    def test_receipt_directory_selects_only_the_exact_worker(self):
        (self.root/'bx_fixture.json').write_text(self.path.read_text());(self.root/'bx_fixture.json').chmod(0o600)
        with patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(self.root)):
            self.assertEqual(RX.admission('boat:bx_fixture')['worker'],'bx_fixture')
            with self.assertRaises(OSError):RX.admission('boat:bx_other')

    def test_remaining_lifetime_is_a_job_constraint(self):
        with self.assertRaises(ValueError):RX.admission('boat:bx_fixture',10800)

    def clocks(self,wall,boot,boot_id=None):
        """Patch wall time, CLOCK_BOOTTIME and the boot id seen by admission."""
        import qualified_worker_admission as qwa
        stack=__import__('contextlib').ExitStack()
        stack.enter_context(patch.object(RX.time,'time',return_value=wall))
        stack.enter_context(patch.object(qwa,'boottime',return_value=boot,create=True))
        stack.enter_context(patch.object(qwa,'current_boot_id',return_value=boot_id or self.fixtures['boot_id'],create=True))
        return stack

    def original_left(self):
        from datetime import datetime
        return datetime.fromisoformat(self.fixtures['qualified']['expires_at'].replace('Z','+00:00')).timestamp()-self.fixtures['now']

    def test_wall_rollback_between_calls_cannot_extend_original_lifetime(self):
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        original=self.fixtures['qualified_path'].read_bytes()
        with self.clocks(now-7200,boot):
            with self.assertRaises(ValueError):RX.admission('boat:bx_fixture',left)
            data=RX.admission('boat:bx_fixture',left-31)
            self.assertLessEqual(data['expires_at']-(now-7200),left)
            deadline=RX.operation_deadline('boat:bx_fixture',seconds=10**6)
            self.assertLessEqual(RX.remaining(deadline),left-30)
        self.assertEqual(self.fixtures['qualified_path'].read_bytes(),original)
        self.assertIsNone(json.loads(original)['runtime_env_file'])

    def test_raw_exec_dispatch_uses_the_earlier_original_end(self):
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        seen=[]
        def spy(argv,output,**kw):seen.append(RX.remaining(kw['deadline']));return 0
        with self.clocks(now-7200,boot),patch.object(RX,'exchange',side_effect=spy),patch.object(sys,'argv',['remote-executor.py','exec','boat:bx_fixture','true']),patch.object(sys.stdin,'isatty',return_value=True):
            self.assertEqual(RX.main(),0)
        self.assertEqual(len(seen),1);self.assertLessEqual(seen[0],left-30)

    def test_one_warm_worker_across_many_calls_and_a_lane_boundary(self):
        """Successive invocations and a later lane share one original end that never grows."""
        import qualified_worker_admission as qwa
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        original=self.fixtures['qualified_path'].read_bytes()
        expected=RX.boat_environment.expected_environment(self.fixtures['profile'],self.fixtures['qualified'])
        # Lane two prepares its own job-admission record from the same qualification.
        lane_two=self.root/'lane-two-admission.json'
        with self.clocks(now-3600,boot+100),patch.object(RX.boat_environment,'local_guard',return_value=self.fixtures['guard']),\
             patch.object(RX.boat_environment.controller,'bounded',return_value={'exit':0,'body':expected}):
            RX.boat_environment.prepare(self.fixtures['profile_path'],self.fixtures['operation'],self.fixtures['profile']['worker'],lane_two)
        lane_two.chmod(0o600)
        elapsed=0
        for index in range(24):
            elapsed+=37
            # The wall clock wanders backward and forward between calls.
            wall=now+elapsed+(-5000 if index%3==0 else 2000 if index%3==1 else 0)
            record=self.path if index<12 else lane_two
            with self.subTest(index=index),self.clocks(wall,boot+elapsed),patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(record)):
                data=RX.admission('boat:bx_fixture')
                deadline=RX.operation_deadline('boat:bx_fixture',seconds=10**6)
                # Never more than the original end on BOOTTIME, whatever the wall says.
                self.assertLessEqual(RX.remaining(deadline),left-elapsed-30)
                self.assertLessEqual(data['expires_at']-wall,left-elapsed)
                self.assertEqual(data['_lifetime']['boot_end'],boot+left)
        self.assertEqual(self.fixtures['qualified_path'].read_bytes(),original)
        self.assertEqual(json.loads(lane_two.read_text())['qualification']['sha256'],hashlib.sha256(original).hexdigest())
        # Re-exporting after a rollback reproduces the same bytes, so it renews nothing.
        with self.clocks(now-7000,boot+elapsed):
            self.assertEqual(json.dumps(qwa.export_operation(self.fixtures['operation'],now-7000,'bx_fixture',self.fixtures['profile']),sort_keys=True),
                             json.dumps(json.loads(original),sort_keys=True))

    def test_adopt_after_lane_boundary_through_a_receipted_extension(self):
        """Lane one ends near the original end. Root extends the same running worker, and lane two adopts it."""
        import qualified_worker_admission as qwa
        controller=RX.boat_environment.controller
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        original=self.fixtures['qualified_path'].read_bytes()
        with self.clocks(now+left-900,boot+left-900):
            RX.admission('boat:bx_fixture')
        # Past the original end lane two cannot run on the worker without an extension.
        with self.clocks(now+left+60,boot+left+60),self.assertRaises(ValueError):RX.admission('boat:bx_fixture')
        io=ExtendIO(self.fixtures,now+left-600,boot+left-600)
        fact=environment_fact(self.fixtures,self.root/'environment-fact.json',io.wall())
        controller.bind_release_profile(self.fixtures['release'])
        with patch.object(qwa,'current_boot_id',return_value=self.fixtures['boot_id']):
            receipt=controller.extend_operation(self.fixtures['release'],io,7200,fact,'lane two')
        intent=json.loads((self.fixtures['operation']/'controller-intent.json').read_text())
        # Until the running guard accepts the receipt, lane two still sees the original end.
        with self.clocks(now+left+60,boot+left+60),self.assertRaises(ValueError):RX.admission('boat:bx_fixture')
        controller.guard_deadlines(ExtendIO(self.fixtures,io.wall(),io.mono()),intent)
        lane_two=self.root/'lane-two-admission.json'
        expected=RX.boat_environment.expected_environment(self.fixtures['profile'],self.fixtures['qualified'])
        with self.clocks(now+left+60,boot+left+60),patch.object(RX.boat_environment.controller,'bounded',return_value={'exit':0,'body':expected}):
            RX.boat_environment.prepare(self.fixtures['profile_path'],self.fixtures['operation'],self.fixtures['profile']['worker'],lane_two)
        lane_two.chmod(0o600)
        for elapsed in (60,1800,6500):
            with self.subTest(elapsed=elapsed),self.clocks(now+left+elapsed,boot+left+elapsed),patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(lane_two)):
                data=RX.admission('boat:bx_fixture')
                self.assertEqual(data['_lifetime']['boot_end'],receipt['boot_end'])
                self.assertLessEqual(RX.remaining(RX.operation_deadline('boat:bx_fixture',seconds=10**6)),receipt['boot_end']-(boot+left+elapsed)-30)
        # The extension ends too; nothing renews it silently.
        with self.clocks(now+left+7200,boot+left+7200),patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(lane_two)),self.assertRaises(ValueError):
            RX.admission('boat:bx_fixture')
        # A wall rollback after the extension cannot stretch it either.
        with self.clocks(now,boot+left+7200),patch.dict(os.environ,SPARKLES_BOAT_ADMISSION=str(lane_two)),self.assertRaises(ValueError):
            RX.admission('boat:bx_fixture')
        self.assertEqual(self.fixtures['qualified_path'].read_bytes(),original)

    def test_forward_wall_suspend_reboot_and_exact_margin(self):
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        # A forward wall jump shortens the remaining time through the wall bound.
        with self.clocks(now+3000,boot):
            with self.assertRaises(ValueError):RX.admission('boat:bx_fixture',left-3000-30)
            RX.admission('boat:bx_fixture',left-3000-31)
        # A suspend advances BOOTTIME. It counts even when the wall clock missed it.
        for wall in (now+3000,now):
            with self.subTest(wall=wall),self.clocks(wall,boot+3000):
                with self.assertRaises(ValueError):RX.admission('boat:bx_fixture',left-3000-30)
                RX.admission('boat:bx_fixture',left-3000-31)
        # The 30-second reserve is subtracted, and an exact margin refuses.
        with self.clocks(now,boot):
            with self.assertRaises(ValueError):RX.admission('boat:bx_fixture',left-30)
            RX.admission('boat:bx_fixture',left-31)
        # A reboot invalidates the original BOOTTIME end.
        with self.clocks(now,boot,'99999999-2222-3333-4444-555555555555'),self.assertRaises(ValueError):
            RX.admission('boat:bx_fixture')

    def test_in_flight_deadline_counts_suspend_and_never_lengthens(self):
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        with self.clocks(now,boot):deadline=RX.operation_deadline('boat:bx_fixture',seconds=10**6)
        with self.clocks(now-7200,boot):
            first=RX.remaining(deadline);self.assertLessEqual(first,left-30)
        with self.clocks(now-7200,boot+left-29),self.assertRaises(ValueError):RX.remaining(deadline)

    def observation(self,wall,boot):
        """Run admit with its probes stubbed and return the printed observation."""
        out=io.StringIO()
        with self.clocks(wall,boot),patch.object(RX.boat_environment.controller,'bounded',return_value={'exit':0,'error':None,'body':{'sandbox':{
                'id':'bx_fixture','environment':self.fixtures['qualified']['environment']['name'],
                'environmentVersion':self.fixtures['qualified']['environment']['version'],'type':'large','vcpu':8,'memoryGB':16,'state':'ready'}}}),\
             patch.object(RX,'exchange',return_value=0),patch('sys.stdout',out):
            RX.admit('boat:bx_fixture',60)
        return json.loads(out.getvalue())

    def runtime(self,path,wall,boot,boot_id=None):
        out=io.StringIO()
        with self.clocks(wall,boot,boot_id),patch.object(sys,'argv',['remote-executor.py','runtime',str(path),'--reserve','60']),patch('sys.stdout',out):
            code=RX.main()
        return code,out.getvalue().strip()

    def test_saved_observation_runtime_uses_both_original_ends(self):
        now,boot,left=self.fixtures['now'],self.fixtures['boot_now'],self.original_left()
        value=self.observation(now,boot);self.assertEqual(value['version'],2)
        path=self.root/'observation.json';path.write_text(json.dumps(value))
        # A wall rollback after the observation was saved cannot lengthen the runtime.
        code,text=self.runtime(path,now-7200,boot+100)
        self.assertEqual(code,0);self.assertLessEqual(int(text),left-100-60)
        # A suspend that the wall clock missed is counted too.
        code,text=self.runtime(path,now,boot+1000);self.assertLessEqual(int(text),left-1000-60)
        with self.assertRaises(ValueError):self.runtime(path,now,boot,'99999999-2222-3333-4444-555555555555')
        with self.assertRaises(ValueError):self.runtime(path,now,boot+left)
        for version in (1,3):
            path.write_text(json.dumps({**value,'version':version}))
            with self.subTest(version=version),self.assertRaises(ValueError):self.runtime(path,now,boot)

    def test_shell_runtime_consumers_use_the_observation_reader(self):
        for name in ('scripts/remote-screen.sh','scripts/gate/gate.sh'):
            text=(ROOT/name).read_text()
            with self.subTest(name=name):
                self.assertNotIn('["expires_at"]-time.time()',text);self.assertIn('rx_runtime',text)

    def fake_worker(self,mbx_version='1.29.1',rustc_version='1.99.1',install_mbx=True):
        """A fake worker home whose tools are tiny scripts, with the job environment's PATH."""
        profile=dict(self.fixtures['profile'],worker_home=str(self.root/'home'))
        home=Path(profile['worker_home']);cargo=home/'.cargo/bin';cargo.mkdir(parents=True)
        # rustup's proxy reports the pinned toolchain only when RUSTUP_TOOLCHAIN selects it.
        (cargo/'rustc').write_text('#!/bin/sh\nif [ "$RUSTUP_TOOLCHAIN" = 1.99.1 ]; then echo "rustc '+rustc_version+' (fixture)"; else echo "rustc 1.97.1 (fixture default)"; fi\n')
        (cargo/'rustc').chmod(0o755)
        if install_mbx:
            pinned=home/'.local/share/mise/installs/mr-boxington/1.29.1';pinned.mkdir(parents=True)
            # The fake answers `settings get` the way mbx does: an environment variable
            # wins over the worker's configuration, which names the read-only remote cache.
            (pinned/'mbx').write_text('#!/bin/sh\nif [ "$1" = settings ] && [ "$2" = get ]; then\n case "$3" in\n'
                                      '  remote.url) echo "${MBX_REMOTE_URL-https://cache.invalid}" ;;\n'
                                      '  gc.max_total_size) echo "${MBX_GC_MAX_TOTAL_SIZE-}" ;;\n'
                                      '  gc.min_free_size) echo "${MBX_GC_MIN_FREE_SIZE-}" ;;\n  *) exit 2 ;;\n esac\n exit 0\nfi\n'
                                      'echo "mbx '+mbx_version+'"\n');(pinned/'mbx').chmod(0o755)
        env=dict(RX.boat_environment.environment(profile),HOME=str(home))
        # Keep the job's home entries and replace the system ones with awk alone,
        # so a host-installed mbx or rustc cannot satisfy the check.
        tools=self.root/'tools';tools.mkdir();(tools/'awk').symlink_to(shutil.which('awk'))
        env['PATH']=':'.join([d for d in env['PATH'].split(':') if d.startswith(str(home))]+[str(tools)])
        return profile,env

    def run_tool_check(self,profile,env):
        # Bash sources a startup file when its stdin is a socket, so the check reads /dev/null.
        return subprocess.run(['/bin/bash','-c',RX.tool_check(profile)],env=env,stdin=subprocess.DEVNULL,capture_output=True,text=True,timeout=10,cwd=env['HOME'])

    def test_job_environment_resolves_pinned_mbx_and_rust(self):
        profile,env=self.fake_worker()
        self.assertIn(profile['worker_home']+'/.local/share/mise/installs/mr-boxington/1.29.1',env['PATH'].split(':'))
        self.assertEqual(env['RUSTUP_TOOLCHAIN'],'1.99.1')
        result=self.run_tool_check(profile,env);self.assertEqual(result.returncode,0,result.stdout+result.stderr)

    def test_admit_tool_check_refuses_missing_mbx_and_wrong_rustc_clearly(self):
        cases=[({'install_mbx':False},'mbx is not on the job PATH'),
               ({'mbx_version':'1.28.0'},'mbx reports 1.28.0, pinned 1.29.1'),
               ({'rustc_version':'1.97.1'},'rustc reports 1.97.1, pinned 1.99.1')]
        for kwargs,reason in cases:
            with self.subTest(reason=reason),tempfile.TemporaryDirectory()as tmp:
                self.root=Path(tmp);profile,env=self.fake_worker(**kwargs)
                result=self.run_tool_check(profile,env)
                self.assertEqual(result.returncode,86,result.stdout+result.stderr)
                self.assertIn('ADMIT-REFUSAL: '+reason,result.stdout,result.stderr)
        # Without RUSTUP_TOOLCHAIN the rustup default is reported and refused, never used.
        with tempfile.TemporaryDirectory()as tmp:
            self.root=Path(tmp);profile,env=self.fake_worker();env.pop('RUSTUP_TOOLCHAIN')
            result=self.run_tool_check(profile,env)
            self.assertIn('ADMIT-REFUSAL: rustc reports 1.97.1, pinned 1.99.1',result.stdout,result.stderr)

    def test_job_environment_is_local_only(self):
        """Boat jobs build with mbx in local-only mode and a local budget, and name no token."""
        profile=self.fixtures['profile'];values=RX.boat_environment.environment(profile)
        self.assertEqual(values['MBX_REMOTE_URL'],'')
        self.assertFalse([k for k in values if 'TOKEN' in k])
        self.assertEqual(values['MBX_GC_MAX_TOTAL_SIZE'],'64GiB');self.assertEqual(values['MBX_GC_MIN_FREE_SIZE'],'45GiB')
        self.assertNotIn('MBX_CACHE_DIR',values)
        body=RX.boat_environment.environment_body(profile).decode()
        self.assertIn("MBX_REMOTE_URL=''\n",body);self.assertNotIn('token',body.lower())
        # Every Boat job shell drops an inherited token setting before it runs.
        script=RX.command('boat:bx_fixture','true')[-1]
        self.assertIn('exec env -u BASH_ENV -u ENV -u MBX_REMOTE_TOKEN_FILE -u MBX_REMOTE_TOKEN ',script)
        self.assertIn(' MBX_REMOTE_URL= ',script)

    def test_environment_preparation_needs_no_token_on_the_worker(self):
        """The worker-side preparation writes the environment file in a home without a token."""
        home=self.root/'prep-home';state=home/'.local/state/sparkles-boat';state.mkdir(parents=True,mode=0o700)
        profile=dict(self.fixtures['profile'],worker_home=str(home),worker_uid=os.getuid())
        q={'safe_owner':{'operation_id':'boat-owned-warm-qual-inert'},'local_guard':{'unit':'boat-owned-warm-qual-inert.service'}}
        expected=RX.boat_environment.expected_environment(profile,q)
        payload={'home':str(home),'uid':os.getuid(),'file':expected,'body':RX.boat_environment.environment_body(profile).decode(),'check':False}
        self.assertFalse((home/'.config/mbx/remote-token').exists())
        out=subprocess.run([sys.executable,'-I','-B','-c',RX.boat_environment.REMOTE,json.dumps(payload)],env={'HOME':str(home),'PATH':os.environ['PATH']},
                           capture_output=True,text=True,timeout=20)
        self.assertEqual(out.returncode,0,out.stderr);self.assertEqual(json.loads(out.stdout),expected)
        self.assertEqual(Path(expected['path']).read_bytes(),RX.boat_environment.environment_body(profile))
        out=subprocess.run([sys.executable,'-I','-B','-c',RX.boat_environment.REMOTE,json.dumps({**payload,'check':True})],env={'HOME':str(home),'PATH':os.environ['PATH']},
                           capture_output=True,text=True,timeout=20)
        self.assertEqual(out.returncode,0,out.stderr)

    def test_admit_tool_check_refuses_a_remote_cache_or_token(self):
        """Admission asks the pinned mbx for its effective settings and refuses anything but local-only."""
        cases=[(lambda e:e.pop('MBX_REMOTE_URL'),'mbx resolves a remote cache URL, not local-only mode'),
               (lambda e:e.update(MBX_REMOTE_URL='https://other.invalid'),'mbx resolves a remote cache URL, not local-only mode'),
               (lambda e:e.update(MBX_REMOTE_TOKEN_FILE='/nonexistent'),'the job environment names an mbx remote token'),
               (lambda e:e.update(MBX_GC_MAX_TOTAL_SIZE='5GiB'),'mbx local budget is 5GiB, pinned 64GiB'),
               (lambda e:e.pop('MBX_GC_MIN_FREE_SIZE'),'mbx free-space floor is , pinned 45GiB')]
        for change,reason in cases:
            with self.subTest(reason=reason),tempfile.TemporaryDirectory()as tmp:
                self.root=Path(tmp);profile,env=self.fake_worker();change(env)
                result=self.run_tool_check(profile,env)
                self.assertEqual(result.returncode,86,result.stdout+result.stderr)
                self.assertIn('ADMIT-REFUSAL: '+reason,result.stdout,result.stderr)

    def test_gate_unit_forwards_the_local_only_job_settings(self):
        """A gate's service unit starts empty, so every job setting but PATH is forwarded when set."""
        text=(ROOT/'scripts/gate/remote/gate-start.sh').read_text()
        line=next(l for l in text.splitlines() if l.startswith('for n in MBX_REMOTE_URL'))
        names=set(line.removeprefix('for n in ').removesuffix('; do').split())
        self.assertEqual(set(RX.boat_environment.environment(self.fixtures['profile']))-{'PATH'},names)
        self.assertIn('[ -z "${!n+x}" ] || forward+=("--setenv=$n=${!n}")',text)
        self.assertIn('systemd-run --user --unit "$unit" --collect -q "${forward[@]}"',text)
        build=(ROOT/'scripts/nsc-build-remote.sh').read_text()
        self.assertNotIn('MBX_REMOTE_TOKEN_FILE=',build)
        self.assertIn('MBX_REMOTE_URL MBX_GC_MAX_TOTAL_SIZE MBX_GC_MIN_FREE_SIZE; do',build)

    def test_admit_renders_its_probe_and_names_a_missing_worker_tool(self):
        """The admit probe parses as shell, carries its formatted values, and checks every worker tool."""
        import subprocess as sp
        profile,env=self.fake_worker()
        boot=Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        data={'worker':'bx_fixture','environment':'fixture-environment','environment_version':3,'_cli':{'path':'/inert/boat'},'_profile':profile,
              'worker_env_path':profile['worker_home']+'/.config/sparkles-boat/job.env','worker_env_sha256':'e'*64,
              'bootstrap_lock_path':profile['worker_home']+'/.local/state/sparkles-boat/bootstrap.lock','toolchain_manifest_path':profile['worker_home']+'/.local/state/sparkles-boat/toolchain.json',
              'toolchain_manifest_sha256':'f'*64,'_lifetime':{'wall_end':time.time()+7200,'boot_end':RX.boat_clock.boottime()+7200,'boot_id':boot}}
        info={'sandbox':{'id':'bx_fixture','environment':'fixture-environment','environmentVersion':3,'type':'large','vcpu':8,'memoryGB':16,'state':'idle'}}
        seen=[]
        with patch.object(RX,'admission',return_value=data),patch.object(RX.boat_environment.controller,'bounded',return_value={'exit':0,'body':info}),\
             patch.object(RX,'command',side_effect=lambda host,text:seen.append(text) or ['true']),patch.object(RX,'exchange',return_value=0),patch('builtins.print'):
            RX.admit('boat:bx_fixture',1800)
        probe=seen[0]
        self.assertEqual(sp.run(['/bin/bash','-n','-c',probe],capture_output=True,timeout=10).returncode,0)
        self.assertIn(shlex.quote(data['worker_env_path']),probe);self.assertNotIn('{envfile}',probe);self.assertIn('{print $2}',probe)
        self.assertIn('for name in '+' '.join(RX.WORKER_TOOLS)+';',probe)
        # A worker without zstd is refused by name before any job work.
        tools=self.root/'some-tools';tools.mkdir()
        for name in RX.WORKER_TOOLS:
            if name!='zstd' and '/' not in name and shutil.which(name):(tools/name).symlink_to(shutil.which(name))
        script="refuse(){ printf 'ADMIT-REFUSAL: %s\\n' \"$1\"; exit 86; }\n"+RX.worker_tools_check()
        result=sp.run(['/bin/bash','-c',script],env={'PATH':str(tools)},stdin=sp.DEVNULL,capture_output=True,text=True,timeout=10)
        self.assertEqual(result.returncode,86);self.assertEqual(RX.refusal_reason(result.stdout.encode()),'zstd is not on the worker')
        (tools/'zstd').symlink_to(shutil.which('true'))
        missing=[n for n in RX.WORKER_TOOLS if not (shutil.which(n) if '/' in n else (tools/n).exists())]
        result=sp.run(['/bin/bash','-c',script],env={'PATH':str(tools)},stdin=sp.DEVNULL,capture_output=True,text=True,timeout=10)
        self.assertEqual(result.returncode,86 if missing else 0,(missing,result.stdout))

    def test_admission_checks_every_command_the_worker_scripts_run(self):
        """Each external command of the worker-side scripts and remote strings is checked at admission."""
        if not shutil.which('shfmt'):self.skipTest('shfmt is needed to parse the worker scripts')
        found={}
        scripts=Path(__file__).resolve().parents[1]
        for name in ('nsc-bench-remote.sh','nsc-writes-remote.sh','nsc-build-remote.sh','nsc-build.sh','cargo-sparkles-task'):
            for word in shell_commands((scripts/name).read_text()):found.setdefault(word,set()).add(name)
        # nsc-jena.sh writes its worker script, remote.sh, from a heredoc.
        for name in ('remote-screen.sh','nsc-build-host.sh','nsc-bench.sh','nsc-writes.sh','nsc-jena.sh'):
            for word in shell_commands((scripts/name).read_text(),remote_only=True):found.setdefault(word,set()).add(name)
        # The command strings remote-executor.py itself sends for put, get, sync and bundle.
        for word in ('bash','env','python3','mkdir','mktemp','cat','sha256sum','cut','mv','tar','git','rm','dirname'):found.setdefault(word,set()).add('remote-executor.py')
        # mbx and rustc are pinned by tool_check, c++ only feeds the build's info.txt, and
        # remote-screen.sh runs nohup and setsid only on the nsc branch.
        exempt={'mbx','rustc','c++','nohup','setsid'}
        missing={w:f for w,f in found.items() if w not in RX.WORKER_TOOLS and w not in exempt}
        self.assertEqual(missing,{})
        self.assertIn('realpath',found);self.assertIn('tee',found);self.assertIn('gzip',RX.WORKER_TOOLS)
        self.assertIn('/lib64/ld-linux-x86-64.so.2',found)
        # The build body reaches bash -s through a heredoc of mbx sparkles-task.
        self.assertIn('nsc-build.sh',found['cp'])
        self.assertIn('nsc-writes-remote.sh',found['tac']);self.assertIn('nsc-jena.sh',found['ls'])

    def test_admit_reports_the_worker_refusal_reason(self):
        self.assertEqual(RX.refusal_reason(b'noise\nADMIT-REFUSAL: mbx is not on the job PATH\n'),'mbx is not on the job PATH')
        self.assertEqual(RX.refusal_reason(b'ADMIT-REFUSAL: \x1b[31mbad\n'),'?[31mbad')
        self.assertIsNone(RX.refusal_reason(b'nothing here'))

    def test_unsafe_selectors_refuse_before_execution(self):
        for host in ('boat:current','boat:bx_test;echo','-oProxyCommand=bad','h x','a:12'):
            with self.subTest(host=host),self.assertRaises(ValueError):RX.boat_id(host)

    def test_ssh_remains_explicit_and_needs_no_boat_receipt(self):
        self.assertEqual(RX.command('wally','exit 17'),[*RX.SSH,'wally','exit 17'])


class Archive(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def archive(self, name='results/exit', data=b'17\n', kind=tarfile.REGTYPE, target=''):
        stream = tempfile.TemporaryFile()
        self.addCleanup(stream.close)
        with tarfile.open(fileobj=stream, mode='w') as t:
            member = tarfile.TarInfo(name)
            member.type = kind; member.linkname = target
            member.size = len(data) if kind == tarfile.REGTYPE else 0
            t.addfile(member, io.BytesIO(data) if member.isfile() else None)
        stream.seek(0)
        return stream

    def test_results_are_extracted_with_numeric_exit_intact(self):
        RX.extract(self.archive(), self.root / 'out')
        self.assertEqual((self.root / 'out/results/exit').read_bytes(), b'17\n')

    def test_traversal_absolute_links_devices_and_existing_symlink_refuse(self):
        for name, kind, target in (('../escape', tarfile.REGTYPE, ''),
                                   ('/escape', tarfile.REGTYPE, ''),
                                   ('results/link', tarfile.SYMTYPE, '../../escape'),
                                   ('results/link', tarfile.LNKTYPE, '/escape'),
                                   ('device', tarfile.CHRTYPE, '')):
            with self.subTest(name=name, kind=kind), self.assertRaises(ValueError):
                RX.extract(self.archive(name, kind=kind, target=target), self.root / 'out')
        outside = self.root / 'outside'; outside.mkdir()
        out = self.root / 'with-link'; out.mkdir(); (out / 'results').symlink_to(outside)
        with self.assertRaises(ValueError):
            RX.extract(self.archive(), out)
        self.assertFalse((outside / 'exit').exists())

    def test_expanded_and_member_bounds_are_enforced(self):
        with patch.object(RX, 'MAX_MEMBERS', 0), self.assertRaises(ValueError):
            RX.extract(self.archive(), self.root / 'out')
        with patch.object(RX, 'MAX_ARCHIVE', 100), self.assertRaises(ValueError):
            RX.extract(self.archive(data=b'x' * 200), self.root / 'out')


class LocalTransport(unittest.TestCase):
    """Execute the wire commands against a local fixture; no provider CLI is used."""
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        libc=ctypes.CDLL(None);previous=ctypes.c_int();self.assertEqual(libc.prctl(37,ctypes.byref(previous),0,0,0),0)
        self.assertEqual(libc.prctl(36,1,0,0,0),0);self.addCleanup(lambda:libc.prctl(36,previous.value,0,0,0))
        self.calls = []
        def command(host, text):
            self.calls.append((host, text))
            return ['bash', '-c', text]
        self.transport = patch.object(RX, 'command', side_effect=command)
        self.transport.start(); self.addCleanup(self.transport.stop)
        self.deadline = patch.object(RX, 'operation_deadline', side_effect=lambda host, seconds=300:time.monotonic()+seconds)
        self.deadline.start(); self.addCleanup(self.deadline.stop)

    def test_binary_roundtrip_and_identity_quoting(self):
        source = self.root / 'local ; $(bad)'
        source.write_bytes(bytes(range(256)) * 16)
        remote = self.root / 'remote ; $(bad)'
        result = self.root / 'copy'
        self.assertEqual(RX.put('boat:bx_test', source, str(remote)), 0)
        self.assertEqual(RX.get('boat:bx_test', str(remote), result), 0)
        self.assertEqual(source.read_bytes(), result.read_bytes())
        self.assertFalse((self.root / 'bad').exists())

    def test_pipe_producer_roundtrip_stall_and_output_deadlines(self):
        read,write=os.pipe()
        with os.fdopen(read,'rb',buffering=0) as source:
            data=bytes(range(256))*8;os.write(write,data);os.close(write)
            with tempfile.TemporaryFile() as output:
                self.assertEqual(RX.exchange(['cat'],output,source=source,timeout=1),0)
                output.seek(0);self.assertEqual(output.read(),data)
        read,write=os.pipe()
        try:
            with os.fdopen(read,'rb',buffering=0) as source,tempfile.TemporaryFile() as output:
                with self.assertRaises(ValueError):RX.exchange(['cat'],output,source=source,timeout=0.1)
                self.assertTrue(os.get_blocking(source.fileno()))
        finally:os.close(write)
        read,write=os.pipe()
        try:
            with os.fdopen(write,'wb',buffering=0) as output:
                with self.assertRaises(ValueError):
                    RX.exchange([sys.executable,'-c','import os;os.write(1,b"x"*100000)'],output,timeout=0.1)
                self.assertTrue(os.get_blocking(output.fileno()))
        finally:os.close(read)

    def test_original_wall_deadline_cannot_be_extended_by_suspend(self):
        deadline=RX.OperationDeadline(time.monotonic()+100,time.time()-1)
        with self.assertRaises(ValueError):RX.remaining(deadline)

    def test_completion_after_the_original_end_is_not_success(self):
        real_time=time.time;base=RX.boat_clock.boottime()
        for clock in ('boot','wall'):
            with self.subTest(clock=clock),tempfile.TemporaryDirectory()as tmp:
                marker=Path(tmp)/'done'
                # Output closes first, so the end passes during the final leader wait.
                argv=['bash','-c','exec 1>&- 2>&-; sleep 0.3; : > '+shlex.quote(str(marker))]
                jump=lambda:100 if marker.exists() else 0
                boot=(lambda:base+jump())if clock=='boot' else (lambda:base)
                wall=(lambda:real_time()+jump())if clock=='wall' else real_time
                deadline=RX.OperationDeadline(time.monotonic()+5,real_time()+5,base+5)
                with patch.object(RX.boat_clock,'boottime',side_effect=boot),patch.object(RX.time,'time',side_effect=wall):
                    with self.assertRaises(ValueError):RX.exchange(argv,io.BytesIO(),deadline=deadline)

    def test_numeric_exit_and_receive_limit(self):
        with tempfile.TemporaryFile() as output:
            self.assertEqual(RX.receive('boat:bx_test', 'exit 17', output), 17)
            with patch.object(RX, 'MAX_ARCHIVE', 4), self.assertRaises(ValueError):
                RX.receive('boat:bx_test', "printf '12345678'", output)

    def test_transfer_refusal_stops_owned_children_after_the_leader_exits(self):
        child_file=self.root/'child.pid'
        text="(trap '' TERM; exec sleep 60) & echo $! > "+shlex.quote(str(child_file))+"; printf '12345678'"
        with tempfile.TemporaryFile() as output, self.assertRaises(ValueError):
            RX.collect(['bash','-c',text],output,limit=4,timeout=2)
        child=int(child_file.read_text())
        deadline=time.monotonic()+2
        while True:
            try:state=Path('/proc',str(child),'stat').read_text().split(') ',1)[1].split()[0]
            except (FileNotFoundError,ProcessLookupError):break
            if state=='Z':break
            self.assertLess(time.monotonic(),deadline,'owned transfer child survived refusal')
            time.sleep(0.02)

    def test_directory_transfer_and_direct_file_destination(self):
        source = self.root / 'src'; source.mkdir(); (source / 'exit').write_text('17\n')
        remote = self.root / 'remote'
        self.assertEqual(RX.sync(['-a', str(source)+'/', 'boat:bx_test:'+str(remote)+'/']), 0)
        out = self.root / 'out'
        self.assertEqual(RX.sync(['-a', 'boat:bx_test:'+str(remote)+'/', str(out)+'/']), 0)
        self.assertEqual((out / 'exit').read_text(), '17\n')
        direct = self.root / 'direct'
        self.assertEqual(RX.sync(['-a', str(source/'exit'), 'boat:bx_test:'+str(direct)]), 0)
        self.assertTrue(direct.is_file())
        with self.assertRaises(ValueError):
            RX.sync(['-a', '--delete', str(source)+'/', 'boat:bx_test:'+str(remote)+'/'])

    def test_unsafe_archive_transfer_does_not_write_the_destination(self):
        source = self.root / 'src'; source.mkdir(); (source/'link').symlink_to('/etc/passwd')
        with self.assertRaises(ValueError):
            RX.sync(['-a', str(source)+'/', 'boat:bx_test:'+str(self.root/'out')+'/'])
        self.assertFalse((self.root/'out').exists())

    def test_delete_mirror_cannot_reuse_old_success_or_failed_current_output(self):
        src=self.root/'fresh';src.mkdir();dst=self.root/'current';dst.mkdir()
        (dst/'gate.exit').write_text('0\n');(dst/'old').write_text('old evidence')
        for value,expected in [(None,99),('17',17),('0',0),(None,99)]:
            if value is None:(src/'gate.exit').unlink(missing_ok=True)
            else:(src/'gate.exit').write_text(value+'\n')
            (src/'run-id').write_text('fresh-'+str(expected))
            self.assertEqual(RX.sync(['-a','--delete','boat:bx_test:'+str(src)+'/',str(dst)+'/']),0)
            code=int((dst/'gate.exit').read_text()) if (dst/'gate.exit').exists() else 99
            self.assertEqual(code,expected)
            self.assertFalse((dst/'old').exists())
            self.assertEqual((dst/'run-id').read_text(),'fresh-'+str(expected))
        previous=list(self.root.glob('.current.previous.*/results'))
        self.assertTrue(any((d/'old').exists() for d in previous))
        with patch.object(RX,'receive',return_value=17):
            self.assertEqual(RX.sync(['-a','--delete','boat:bx_test:'+str(src)+'/',str(dst)+'/']),17)
        self.assertFalse(dst.exists())
        self.assertEqual(list(self.root.glob('.current.fetch.*')),[])

    def test_pack_refuses_before_spool_limit_and_never_dispatches(self):
        src=self.root/'large';src.mkdir();(src/'blob').write_bytes(b'x'*131072)
        with tempfile.TemporaryFile(buffering=0) as out,patch.object(RX,'MAX_ARCHIVE',65536):
            with self.assertRaises(ValueError):RX.pack_directory(out,str(src)+'/',[],time.monotonic()+2)
            self.assertLessEqual(out.seek(0,2),65536)
        calls=len(self.calls)
        with patch.object(RX,'MAX_ARCHIVE',65536),self.assertRaises(ValueError):
            RX.sync(['-a',str(src)+'/','boat:bx_test:/unused/'])
        self.assertEqual(len(self.calls),calls)

    def test_pack_padding_exact_limit_members_exclusions_and_construction_deadline(self):
        src=self.root/'small';src.mkdir();(src/'keep').write_bytes(b'x'*100);(src/'exclude').write_bytes(b'y')
        with tempfile.TemporaryFile(buffering=0) as out:
            RX.pack_directory(out,str(src)+'/',['exclude'],time.monotonic()+2)
            size=out.seek(0,2)
            self.assertGreater(size,100)
        for limit,accept in [(size,True),(size-1,False)]:
            with self.subTest(limit=limit),tempfile.TemporaryFile(buffering=0) as out,patch.object(RX,'MAX_ARCHIVE',limit):
                if accept:RX.pack_directory(out,str(src)+'/',['exclude'],time.monotonic()+2)
                else:
                    with self.assertRaises(ValueError):RX.pack_directory(out,str(src)+'/',['exclude'],time.monotonic()+2)
                self.assertLessEqual(out.seek(0,2),limit)
        with tempfile.TemporaryFile() as out,patch.object(RX,'MAX_MEMBERS',1),self.assertRaises(ValueError):
            RX.pack_directory(out,str(src)+'/',[],time.monotonic()+2)
        nested=src/'nested';nested.mkdir();(nested/'file').write_bytes(b'x')
        with tempfile.TemporaryFile() as out,patch.object(RX,'MAX_MEMBERS',1),self.assertRaises(ValueError):
            RX.pack_directory(out,str(nested),[],time.monotonic()+2)
        with tempfile.TemporaryFile() as out,self.assertRaises(ValueError):
            RX.pack_directory(out,str(src)+'/',[],time.monotonic()-1)

    def test_sender_deadline_covers_blocked_input_missing_ack_and_numeric_exit(self):
        payload=self.root/'payload';payload.write_bytes(b'x'*131072)
        for text in ('sleep 2','cat >/dev/null; sleep 2'):
            with self.subTest(text=text),patch.object(RX,'command',return_value=['bash','-c',text]),payload.open('rb') as data:
                began=time.monotonic()
                with self.assertRaises(ValueError):RX.send('boat:bx_test',text,data,time.monotonic()+0.04)
                self.assertLess(time.monotonic()-began,1)
        with patch.object(RX,'command',return_value=['bash','-c','cat >/dev/null; exit 17']),payload.open('rb') as data:
            self.assertEqual(RX.send('boat:bx_test','ignored',data,time.monotonic()+1),17)
        calls=len(self.calls)
        with payload.open('rb') as data,self.assertRaises(ValueError):
            RX.send('boat:bx_test','ignored',data,time.monotonic()-1)
        self.assertEqual(len(self.calls),calls)

    def test_sender_uses_original_deadline_after_receipt_changes_and_preparation(self):
        source=self.root/'blob';source.write_bytes(b'x')
        self.deadline.stop()
        def life(seconds):
            return {'_lifetime':{'wall_end':time.time()+seconds,'boot_end':RX.boat_clock.boottime()+seconds,
                                 'boot_id':RX.boat_clock.current_boot_id(),'invocation_id':'a'*32}}
        with patch.object(RX,'admission',return_value=life(30.06)):
            deadline=RX.operation_deadline('boat:bx_test')
        with patch.object(RX,'admission',return_value=life(10000)),patch.object(RX,'command',return_value=['bash','-c','cat >/dev/null; sleep 1']),source.open('rb')as data:
            with self.assertRaises(ValueError):RX.send('boat:bx_test','ignored',data,deadline)
        calls=len(self.calls)
        real_digest=hashlib.file_digest
        def slow_digest(*a,**kw):time.sleep(0.04);return real_digest(*a,**kw)
        with patch.object(RX,'operation_deadline',return_value=time.monotonic()+0.02),patch.object(RX.hashlib,'file_digest',side_effect=slow_digest),self.assertRaises(ValueError):
            RX.put('boat:bx_test',source,'/unused')
        self.assertEqual(len(self.calls),calls)

    def test_sender_interrupt_cleans_only_owned_session(self):
        pidfile=self.root/'sender.pid'
        class Interrupted:
            def write(self,data):raise KeyboardInterrupt()
        other=subprocess.Popen(['sleep','3'],start_new_session=True)
        try:
            with self.assertRaises(KeyboardInterrupt):
                RX.exchange(['bash','-c','echo $$ > '+shlex.quote(str(pidfile))+'; printf x; exec sleep 10'],Interrupted(),source=io.BytesIO(b'data'),deadline=time.monotonic()+1)
            self.assertIsNone(other.poll())
            with self.assertRaises(ProcessLookupError):os.kill(int(pidfile.read_text()),0)
        finally:other.terminate();other.wait(timeout=3)

    def test_sender_preserves_failed_leader_exit_and_download_verification_is_atomic(self):
        source=self.root/'blob';source.write_bytes(b'data');pidfile=self.root/'descendant.pid'
        text="(trap '' TERM; exec sleep 10) & echo $! > "+shlex.quote(str(pidfile))+"; exit 17"
        with patch.object(RX,'command',return_value=['bash','-c',text]),source.open('rb')as data:
            with self.assertRaises(ValueError)as error:RX.send('boat:bx_test','ignored',data,time.monotonic()+0.05)
            self.assertEqual(error.exception.native_exit,17)
        child=int(pidfile.read_text());deadline=time.monotonic()+1
        while Path('/proc',str(child)).exists():
            try:state=Path('/proc',str(child),'stat').read_text().split(') ',1)[1].split()[0]
            except (FileNotFoundError,ProcessLookupError):break
            if state=='Z':break
            self.assertLess(time.monotonic(),deadline);time.sleep(0.01)
        target=self.root/'accepted';target.write_bytes(b'old accepted')
        def command(host,body):
            return ['bash','-c',("printf '%s  ignored\\n' "+'0'*64)if body.startswith('sha256sum')else body]
        with patch.object(RX,'command',side_effect=command),self.assertRaises(ValueError):RX.get('boat:bx_test',str(source),target)
        self.assertEqual(target.read_bytes(),b'old accepted')

    def test_zero_and_failed_sender_reap_silent_owned_descendants_only(self):
        for status in (0,17):
            for public_send in (True,False):
                ready=self.root/f'ready-{status}-{public_send}';pidfile=self.root/f'pid-{status}-{public_send}'
                child="import signal,time,pathlib;signal.signal(signal.SIGTERM,signal.SIG_IGN);pathlib.Path("+repr(str(ready))+").write_text('ready');time.sleep(5)"
                leader="import sys,subprocess,pathlib,time;sys.stdin.buffer.read();p=subprocess.Popen([sys.executable,'-c',"+repr(child)+"],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);pathlib.Path("+repr(str(pidfile))+").write_text(str(p.pid));end=time.monotonic()+1\nwhile not pathlib.Path("+repr(str(ready))+").exists():\n if time.monotonic()>end:raise SystemExit(19)\n time.sleep(.005)\nraise SystemExit("+str(status)+")"
                foreign=subprocess.Popen(['sleep','5'],start_new_session=True)
                try:
                    argv=[sys.executable,'-c',leader]
                    with patch.object(RX,'command',return_value=argv),tempfile.TemporaryFile()as output:
                        if public_send:code=RX.send('boat:bx_test','fixture',io.BytesIO(b'exact input'),time.monotonic()+2)
                        else:code=RX.exchange(argv,output,source=io.BytesIO(b'exact input'),deadline=time.monotonic()+2)
                    self.assertEqual(code,status);self.assertTrue(ready.exists())
                    child_pid=int(pidfile.read_text());self.assertFalse(Path('/proc',str(child_pid)).exists())
                    with self.assertRaises(ChildProcessError):os.waitpid(child_pid,os.WNOHANG)
                    self.assertIsNone(foreign.poll())
                finally:
                    foreign.terminate();foreign.wait(timeout=3)
                    if pidfile.exists():
                        child_pid=int(pidfile.read_text())
                        if Path('/proc',str(child_pid)).exists():
                            os.kill(child_pid,signal.SIGKILL);os.waitpid(child_pid,0)

    def test_legacy_ssh_delete_is_delegated_without_changing_arguments(self):
        args=['-a','--delete','wally:gate/out/test/',str(self.root/'dest')+'/']
        with patch.object(RX.subprocess,'call',return_value=17) as call:
            self.assertEqual(RX.sync(args),17)
            call.assert_called_once_with(['rsync',*args])

    def test_bundle_preserves_valid_history_and_excludes_untracked_private_files(self):
        repo = self.root / 'repo'; repo.mkdir()
        def git(*args):
            return subprocess.run(['git', '-C', str(repo), '-c', 'user.name=t', '-c', 'user.email=t@t',
                                   '-c', 'commit.gpgsign=false', *args], check=True, capture_output=True, text=True).stdout.strip()
        git('init', '-q')
        (repo/'old-private-evidence').write_text('historical file must not travel')
        git('add', '.'); git('commit', '-qm', 'initial')
        old_blob = git('rev-parse', 'HEAD:old-private-evidence')
        git('rm', 'old-private-evidence'); (repo/'source').write_text('base')
        git('add', '.'); git('commit', '-qm', 'base'); base = git('rev-parse', 'HEAD')
        (repo/'source').write_text('candidate'); git('commit', '-qam', 'candidate'); full = git('rev-parse', 'HEAD')
        (repo/'untracked-private-evidence').write_text('must not travel')
        private_blob = git('hash-object', 'untracked-private-evidence')
        before = Path.cwd()
        home = self.root / 'worker'; home.mkdir()
        try:
            os.chdir(repo)
            with patch.dict(os.environ, HOME=str(home)):
                record=self.root/'transfer.json'
                self.assertEqual(RX.bundle('boat:bx_test', full, base, record), 0, record.read_text())
                self.assertEqual(json.loads(record.read_text())['mode'],'full-seed')
                transfers=sum('fetch -q' in text for _,text in self.calls)
                self.assertEqual(RX.bundle('boat:bx_test', full, base, record),0)
                self.assertEqual(json.loads(record.read_text())['mode'],'reuse')
                self.assertEqual(sum('fetch -q' in text for _,text in self.calls),transfers)
                (repo/'source').write_text('incremental');git('commit','-qam','next')
                next_full=git('rev-parse','HEAD')
                self.assertEqual(RX.bundle('boat:bx_test',next_full,full,record),0,record.read_text())
                incremental=json.loads(record.read_text())
                self.assertEqual(incremental['mode'],'incremental')
                self.assertEqual(incremental['prerequisite_proof']['commit'],full)
                self.assertIn(full,incremental['prerequisites'])
                self.assertRegex(incremental['bundle_sha256'],r'^[0-9a-f]{64}$')
        finally:
            os.chdir(before)
        remote = home / 'gate/repo.git'
        run = lambda *a: subprocess.run(['git', '-C', str(remote), *a], capture_output=True, text=True)
        self.assertEqual(run('show', full+':source').stdout, 'candidate')
        self.assertEqual(run('merge-base', base, full).stdout.strip(), base)
        self.assertEqual(run('cat-file', '-e', old_blob).returncode, 0)
        self.assertNotEqual(run('cat-file', '-e', private_blob).returncode, 0)
        self.assertEqual(run('fsck', '--no-dangling').returncode, 0)
        self.assertEqual(run('diff', '--name-only', base, full).stdout.strip(), 'source')


class GateFetchFixture(unittest.TestCase):
    def test_actual_whole_and_already_merged_fetch_never_accept_stale_exit(self):
        source=(ROOT/'scripts/gate/gate.sh').read_text().splitlines()
        selected=[];inside=False
        for line in source:
            if any(line.startswith(name+'() {') for name in ('wall','fetch','finished')):inside=True
            if inside:
                selected.append(line)
                if line=='}':inside=False
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp);remote=root/'remote';remote.mkdir();fresh=remote/'source';fresh.mkdir()
            bridge=root/'bridge.py'
            bridge.write_text("import importlib.util,os,subprocess,sys,time\n"
                +"s=importlib.util.spec_from_file_location('rx',"+repr(str(ROOT/'scripts/remote-executor.py'))+");r=importlib.util.module_from_spec(s);s.loader.exec_module(r)\n"
                +"r.command=lambda h,c:['bash','-c','cd '+os.environ['REMOTE_FIXTURE']+'; '+c]\n"
                +"r.operation_deadline=lambda h:time.monotonic()+5\n"
                +"a=sys.argv[1:];raise SystemExit(r.sync(a[1:] if a[:1]==['--'] else a))\n")
            for index,(old,value,helper,expected) in enumerate([(0,None,'none',99),(0,17,'none',17),(17,0,'none',0),(0,17,'other',17)]):
                dest=root/f'accepted-{index}';dest.mkdir();(dest/'gate.exit').write_text(str(old))
                if value is None:(fresh/'gate.exit').unlink(missing_ok=True)
                else:(fresh/'gate.exit').write_text(str(value))
                (fresh/'run-id').write_text('fresh-'+str(index))
                program="\n".join(selected)+r'''
rx_sync() { python3 "$BRIDGE" "$@"; }
rx_exec() { (cd "$REMOTE_FIXTURE" && bash -c "$2"); }
primary=boat:bx_fixture
pout=source
name=fixture
fetch || exit "$?"
code=$(cat "$dest/gate.exit" 2>/dev/null || echo 99)
exit "$code"
'''
                result=subprocess.run(['bash','-c',program],env=dict(os.environ,HOME=str(remote),REMOTE_FIXTURE=str(remote),BRIDGE=str(bridge),RX_DIR=str(ROOT/'scripts'),dest=str(dest),helper=helper),capture_output=True,text=True,timeout=10)
                self.assertEqual(result.returncode,expected,result.stderr)
                self.assertEqual((dest/'run-id').read_text(),'fresh-'+str(index))
                self.assertTrue(list(root.glob('.'+dest.name+'.previous.*/results/gate.exit')))


class ScreenAcceptance(unittest.TestCase):
    def test_unknown_launch_is_recorded_once_and_cleanup_retains_lock_on_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            boot = RX.boat_clock.boottime()
            receipt = root/'admission.json'; receipt.write_text(json.dumps({'schema':'sparkles.boat.adapter-observation','version':2,
                'expires_at':time.time()+3600,'original_wall_end':time.time()+3600,'boot_end':boot+3600,
                'guard_boot_id':RX.boat_clock.current_boot_id()}))
            program = r'''
set -euo pipefail
. "$SCRIPTS/remote-screen.sh"
EXECUTOR=boat:bx_test
SCREEN_LOCK=sparkles-screen-lock-owned
REMOTE=/home/user/lanes/remote-screens/owned
OUT=$RESULTS
screen_exec() { printf '%s\n' "$1" >> "$OUT/commands"; return 255; }
code=0
screen_launch nsc-bench-remote.sh nohup.log || code=$?
test "$code" = 255
test "$(wc -l < "$OUT/workload-units")" = 1
screen_destroy && exit 1
'''
            result = subprocess.run(['bash', '-c', program], env=dict(os.environ, SCRIPTS=str(ROOT/'scripts'), RESULTS=tmp),
                                    capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 97, result.stderr)
            commands = (root/'commands').read_text()
            self.assertEqual(commands.count('systemd-run'), 1)
            self.assertNotIn('stop sparkles-screen-lock-owned', commands)
            self.assertNotIn('resume', commands)



class JenaStaging(unittest.TestCase):
    def test_unique_remote_root_rebases_classpath_and_extra_library_and_records_exit(self):
        source = (ROOT/'scripts/nsc-jena.sh').read_text()
        body = source.split('cat > "$STAGE/remote.sh" << \'EOF\'\n', 1)[1].split('\nEOF', 1)[0]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seed = root/'seed'; seed.mkdir()
            classpath = Path('repo/jvm/sparkles-jena/build/bindings-bench/classpath.txt')
            (seed/classpath).parent.mkdir(parents=True)
            (seed/classpath).write_text('@REMOTE@/repo/cp/a.jar\n')
            with tarfile.open(root/'repo.tar.zst', 'w') as t:t.add(seed/'repo', arcname='repo')
            python = seed/'cpython-test/bin/python3'; python.parent.mkdir(parents=True)
            python.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > python.args\nexit 17\n'); python.chmod(0o755)
            with tarfile.open(root/'python.tar.zst', 'w') as t:t.add(seed/'cpython-test', arcname='cpython-test')
            java = seed/'jdk/bin/java'; java.parent.mkdir(parents=True)
            java.write_text('#!/bin/sh\necho java-fixture\n'); java.chmod(0o755)
            with tarfile.open(root/'jdk.tar.gz', 'w:gz') as t:t.add(seed/'jdk', arcname='jdk')
            bin_dir=root/'bin'; bin_dir.mkdir()
            zstd=bin_dir/'zstd'; zstd.write_text('#!/bin/sh\ncat "${@: -1}"\n'.replace('#!/bin/sh','#!/usr/bin/env bash')); zstd.chmod(0o755)
            (root/'run.env').write_text("BENCH_COMMIT=fixture\nARGS=('--arm' 'test=sparkles-mem;-Dsparkles.native.path=@REMOTE@/repo/native/lib.so')\n")
            (root/'remote.sh').write_text(body)
            result=subprocess.run(['bash','remote.sh'],cwd=root,env=dict(os.environ,PATH=str(bin_dir)+os.pathsep+os.environ['PATH']),capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual((root/'work/done').read_text(),'exit 17\n')
            self.assertEqual((root/classpath).read_text(),str(root)+'/repo/cp/a.jar\n')
            self.assertIn(str(root)+'/repo/native/lib.so',(root/'python.args').read_text())
            status_body=source[source.rindex('case $status in'):]
            exit_result=subprocess.run(['bash','-c','die() { exit 97; }; status="exit 17"\n'+status_body],capture_output=True,text=True)
            self.assertEqual(exit_result.returncode,17)


class FinalizerInterrupt(unittest.TestCase):
    """Real SIGINTs reach the worker only after exchange teardown has started."""
    def check(self,phase,count,leader):
        from finalizer_interrupt import run
        outcome=run('exchange',phase,count,leader)
        self.assertEqual(outcome['fired'],[phase],outcome)
        self.assertFalse(outcome['ready_alive_after_return'],outcome)
        self.assertTrue(outcome['foreign_alive'],outcome)
        self.assertEqual(outcome['streams_closed'],[True,True,True],outcome)
        self.assertNotIn('returned',outcome,outcome)
        return outcome

    def test_first_and_repeated_interrupts_keep_the_timeout_primary(self):
        for phase,count in (('term_wait',1),('term_wait',3),('kill',1),('probe',2)):
            with self.subTest(phase=phase,count=count):
                outcome=self.check(phase,count,'timeout')
                self.assertEqual(outcome['exception'],'ValueError',outcome)
                self.assertIn('deadline',outcome['message'])
                self.assertEqual(outcome['cleanup_interrupted'],count,outcome)

    def test_interrupt_before_teardown_or_after_spawn_cannot_skip_closure(self):
        outcome=self.check('entry',1,'timeout')
        self.assertEqual(outcome['exception'],'ValueError',outcome);self.assertEqual(outcome['cleanup_interrupted'],1,outcome)
        outcome=self.check('spawn',1,'timeout')
        self.assertEqual(outcome['exception'],'KeyboardInterrupt',outcome);self.assertIsNone(outcome['cleanup_interrupted'],outcome)

    def interrupt_inside(self,body):
        """Run body with the default handler and return seconds from SIGINT to the raise."""
        previous=signal.signal(signal.SIGINT,signal.default_int_handler);sent=[]
        try:
            try:body(lambda:(sent.append(time.monotonic()),os.kill(os.getpid(),signal.SIGINT)))
            except KeyboardInterrupt:return time.monotonic()-sent[0]
            self.fail('no cancellation')
        finally:signal.signal(signal.SIGINT,previous)

    def test_blocking_sink_source_and_nested_call_still_cancel_promptly(self):
        import threading
        argv=['bash','-c','printf x; exec sleep 10']
        class Sink:
            def __init__(self,fire):self.fire=fire
            def write(self,data):self.fire();time.sleep(0.65)
        class Source:
            def __init__(self,fire):self.fire=fire
            def read(self,size):self.fire();time.sleep(0.65);return b''
        class Nested:
            def __init__(self,fire):self.fire=fire
            def read(self,size):
                threading.Timer(0.04,self.fire).start()
                RX.boat_environment.controller.bounded([sys.executable,'-c','import time;time.sleep(5)'],0.55);return b''
        cases={'sink':lambda fire:RX.exchange(argv,Sink(fire),deadline=time.monotonic()+5),
               'source':lambda fire:RX.exchange(['bash','-c','cat >/dev/null; exec sleep 10'],io.BytesIO(),source=Source(fire),deadline=time.monotonic()+5),
               'nested':lambda fire:RX.exchange(['bash','-c','cat >/dev/null; exec sleep 10'],io.BytesIO(),source=Nested(fire),deadline=time.monotonic()+5)}
        for name,body in cases.items():
            with self.subTest(case=name):
                self.assertLess(self.interrupt_inside(body),0.45)

    def test_custom_handler_runs_after_closure_without_replacing_the_timeout(self):
        from finalizer_interrupt import run
        outcome=run('exchange','term_wait',1,'timeout','custom')
        self.assertEqual(outcome['exception'],'ValueError',outcome)
        self.assertEqual(outcome['cleanup_interrupted'],1,outcome)
        self.assertEqual(outcome['cleanup_interrupt_error'],'KeyboardInterrupt',outcome)
        self.assertFalse(outcome['ready_alive_after_return'],outcome);self.assertTrue(outcome['foreign_alive'],outcome)

    def test_interrupt_after_leader_exit_is_propagated_after_descendant_closure(self):
        for leader,phase in (('0','term'),('17','term'),('0','probe'),('17','kill')):
            with self.subTest(leader=leader,phase=phase):
                outcome=self.check(phase,1,leader)
                self.assertEqual(outcome['exception'],'KeyboardInterrupt',outcome)
                self.assertEqual(outcome['native_exit'],int(leader),outcome)
                self.assertEqual(outcome['cleanup_interrupted'],1,outcome)

    def test_uncertain_closure_closes_every_stream_and_keeps_numeric_exit(self):
        real=os.killpg
        def denied(pid,sig):
            if sig==0:raise PermissionError()
            return real(pid,sig)
        for status in (0,17):
            created=[];actual=subprocess.Popen
            def launch(*a,**kw):
                p=actual(*a,**kw);created.append(p);return p
            with self.subTest(status=status),patch.object(RX.subprocess,'Popen',side_effect=launch),patch.object(RX.os,'killpg',side_effect=denied):
                argv=['bash','-c','cat >/dev/null; exit '+str(status)]
                if status:
                    self.assertEqual(RX.exchange(argv,io.BytesIO(),source=io.BytesIO(b'x'),deadline=time.monotonic()+2),17)
                else:
                    with self.assertRaises(ValueError)as error:
                        RX.exchange(argv,io.BytesIO(),source=io.BytesIO(b'x'),deadline=time.monotonic()+2)
                    self.assertEqual(error.exception.native_exit,0);self.assertTrue(error.exception.cleanup_errors)
                self.assertEqual([s.closed for s in (created[0].stdin,created[0].stdout,created[0].stderr)],[True,True,True])


if __name__ == '__main__':
    unittest.main()
