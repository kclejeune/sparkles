#!/usr/bin/env python3
"""Bounded Linux ACK regression. Each invocation requires a fresh output directory.

Uses independently copied addon/fixture/package outputs. LD_PRELOAD is applied
only to each owned Node child, never to the driver/compiler/author shell.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import time


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--addon', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir()  # Never reuse another child's ACKs or overwrite evidence.
    fixture = Path(__file__).resolve()
    engine = fixture.parents[2]
    common = engine.parent / 'common'
    copied = output / 'replay'
    for package in [engine, common]:
        dest = copied / 'js' / package.name
        dest.mkdir(parents=True)
        shutil.copytree(package / 'dist', dest / 'dist')
        shutil.copy2(package / 'package.json', dest / 'package.json')
    scope = copied / 'js/engine/node_modules/@sparkles-rdf'
    scope.mkdir(parents=True)
    (scope / 'common').symlink_to(copied / 'js/common', target_is_directory=True)
    child = copied / 'js/engine/test/fixtures/algo-stats-child.mjs'
    child.parent.mkdir(parents=True)
    shutil.copy2(fixture.with_name('algo-stats-child.mjs'), child)
    addon = copied / 'sparkles.node'
    shutil.copy2(args.addon.resolve(), addon)
    assert sha(addon) == sha(args.addon.resolve())
    assert addon.stat().st_ino != args.addon.resolve().stat().st_ino
    source = output / 'algo-stats-ack-delay.c'
    shutil.copy2(fixture.with_name('algo-stats-ack-delay.c'), source)
    library = output / 'algo-stats-ack-delay.so'
    commands = []

    def run(label, argv, env=None, seconds=12):
        started = time.monotonic()
        proc = subprocess.Popen(list(map(str, argv)), cwd=copied,
                                env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                start_new_session=True)
        timed_out = False
        interrupted = None
        try:
            stdout, stderr = proc.communicate(timeout=seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(proc.pid, signal.SIGKILL)
            stdout, stderr = proc.communicate(timeout=5)
        except BaseException as error:
            interrupted = error
            if proc.poll() is None:
                os.killpg(proc.pid, signal.SIGKILL)
            stdout, stderr = proc.communicate(timeout=5)
        (output / (label + '.stdout')).write_bytes(stdout)
        (output / (label + '.stderr')).write_bytes(stderr)
        rec = dict(label=label, argv=list(map(str, argv)), returncode=proc.returncode,
                   elapsed_seconds=time.monotonic()-started, timed_out=timed_out,
                   signal=-proc.returncode if proc.returncode < 0 else None,
                   reaped=True, pid=proc.pid, interrupted=repr(interrupted) if interrupted else None,
                   environment={key:env[key] for key in ['LD_PRELOAD','SPARKLES_NODE_NATIVE',
                                'SPARKLES_TEST_ALGO_STATS_DIR','SPARKLES_ACK_ROOT',
                                'SPARKLES_ACK_NAME','SPARKLES_ACK_MODE'] if env and key in env})
        commands.append(rec)
        (output / 'commands.json').write_text(json.dumps(commands, indent=2)+'\n')
        if interrupted:
            raise interrupted
        assert not timed_out, f'{label}: child timed out (killed and reaped)'
        return rec, stdout, stderr

    compiler = Path(shutil.which('cc')).resolve()
    node = Path(shutil.which('node')).resolve()
    rec, _, _ = run('compile-interposer', [compiler, '-shared', '-fPIC', '-Wall', '-Wextra',
                                        '-Werror', '-o', library, source, '-ldl'], seconds=30)
    assert rec['returncode'] == 0, 'interposer compilation failed'
    inputs = dict(addon=dict(original=str(args.addon.resolve()), sha256=sha(addon)),
                  fixture=sha(child), interposer_source=sha(source), interposer=sha(library),
                  compiler=dict(path=str(compiler), sha256=sha(compiler)),
                  node=dict(path=str(node), sha256=sha(node)),
                  packages={str(p.relative_to(copied)): sha(p) for p in copied.rglob('*')
                            if p.is_file() and 'node_modules' not in p.parts})
    (output / 'inputs.json').write_text(json.dumps(inputs, indent=2)+'\n')
    modes = ['compute-cancel', 'compute-timeout', 'slot-cancel', 'slot-timeout']
    configurations = [('none', 'entered')]+[(mode,name) for mode in ['delay','partial']
                                                          for name in ['entered','finished']]
    for injection, name in configurations:
        for mode in modes:
            label = injection + '-' + name + '-' + mode
            directory = output / label
            directory.mkdir()
            env = dict(os.environ, SPARKLES_NODE_NATIVE=str(addon),
                       SPARKLES_TEST_ALGO_STATS_DIR=str(directory))
            env.pop('LD_PRELOAD', None)
            if injection != 'none':
                env.update(LD_PRELOAD=str(library), SPARKLES_ACK_ROOT=str(directory),
                           SPARKLES_ACK_NAME=name, SPARKLES_ACK_MODE=injection)
            rec, stdout, stderr = run(label, [node, child, mode, directory], env)
            if injection != 'none':
                trace = [json.loads(row) for row in (directory / 'intercept.jsonl').read_text().splitlines()]
                rec['intercepts'] = trace
                assert any(row['event'] == ('before-bytes' if injection == 'delay' else 'prefix-written')
                           for row in trace), 'vacuous injection'
            (output / 'commands.json').write_text(json.dumps(commands, indent=2)+'\n')
            assert rec['returncode'] == 0, f'{label}: native child failed: {stderr.decode()}'
            result = json.loads(stdout)
            assert result['native']['result'] in ['Cancelled', 'Timeout']
            assert result['native']['slotReusable'] and not result['native']['holderReleasedEarly']
            assert result['entry']['deadlineForwarded']
            assert result['native']['algorithmEntered'] == mode.startswith('compute')
            if injection != 'none':
                assert any(row['path'].endswith('/.'+name+'.json.tmp') for row in trace), 'temporary write not intercepted'
    failures = []
    for name in ['entered','finished']:
        for injection in ['fail-write','fail-close','fail-rename']:
            label=injection+'-'+name
            directory=output/label;directory.mkdir()
            env=dict(os.environ, SPARKLES_NODE_NATIVE=str(addon),
                     SPARKLES_TEST_ALGO_STATS_DIR=str(directory),LD_PRELOAD=str(library),
                     SPARKLES_ACK_ROOT=str(directory),SPARKLES_ACK_NAME=name,SPARKLES_ACK_MODE=injection)
            rec, stdout, stderr=run(label,[node,child,'slot-cancel',directory],env)
            assert rec['returncode'] != 0, 'failed publication misreported success'
            trace=[json.loads(row) for row in (directory/'intercept.jsonl').read_text().splitlines()]
            assert any(row['event']=={'fail-write':'write-failed','fail-close':'close-failed','fail-rename':'rename-failed'}[injection] for row in trace)
            assert not (directory/(name+'.json')).exists(), 'failed ACK published'
            assert not (directory/('.'+name+'.json.tmp')).exists(), 'owned failed temp leaked'
            if name=='finished':
                attempted=json.loads((directory/'attempted-finished.json').read_text())
                assert attempted['slotReusable'] and not attempted['holderReleasedEarly']
                assert attempted['result'] in ['Cancelled','Timeout']
                rec['unpublished_cleanup_witness']=attempted
            rec['intercepts']=trace;failures.append(label)
            (output/'commands.json').write_text(json.dumps(commands,indent=2)+'\n')
    directory=output/'malformed-final';directory.mkdir()
    (directory/'entered.json').write_text('{')
    env=dict(os.environ,SPARKLES_NODE_NATIVE=str(addon),SPARKLES_TEST_ALGO_STATS_DIR=str(directory))
    env.pop('LD_PRELOAD',None)
    rec,stdout,stderr=run('malformed-final',[node,child,'slot-cancel',directory],env)
    assert rec['returncode'] != 0 and b'SyntaxError' in stderr
    assert (directory/'entered.json').read_text()=='{', 'stale final overwritten'
    assert not (directory/'.entered.json.tmp').exists()
    (output/'result.json').write_text(json.dumps(dict(passed=True, real_child_cases=20,
        bounded_expected_failures=failures+['malformed-final'],
        missing_final='six failed-publication cases require missing final, bounded observer failure and child reap',
        cleanup='finished failure payload proves already-joined holder/reusable slot; entered failure actual Rust child controls prove Drop cleanup'))+'\n')


if __name__ == '__main__':
    main()
