"""Provider contracts using bounded local fixtures, never provider qualification."""
import hashlib
import ctypes
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import signal
import sys
import subprocess
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch
from boat_fixtures import fixture as boat_fixture

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('remote_executor', ROOT / 'scripts/remote-executor.py')
RX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RX)


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

    def test_exact_warm_identity_and_binary_safe_command(self):
        result=RX.command('boat:bx_fixture',"printf '%s' 'a;$(bad)' && cat")
        self.assertEqual(result[:4],[self.fixtures['release']['boat']['path'],'--no-update','ssh','bx_fixture'])
        decoded=shlex.split(result[-1]);self.assertEqual(len(decoded),1)
        self.assertIn("a;$(bad)",decoded[0]);self.assertNotIn('; . ',decoded[0])
        self.assertFalse(any(action in result for action in ('create','resume','stop','snapshot')))

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
        with patch.object(RX,'admission',return_value={'expires_at':time.time()+30.06}):
            deadline=RX.operation_deadline('boat:bx_test')
        with patch.object(RX,'admission',return_value={'expires_at':time.time()+10000}),patch.object(RX,'command',return_value=['bash','-c','cat >/dev/null; sleep 1']),source.open('rb')as data:
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
            receipt = root/'admission.json'; receipt.write_text(json.dumps({'expires_at':time.time()+3600}))
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


if __name__ == '__main__':
    unittest.main()
