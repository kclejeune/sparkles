# Root-only qualification stdin. Existing operation recovery never starts a unit twice.
from pathlib import Path
import os,json,subprocess,base64
exec(COMMON_SOURCE_LITERAL)
PAYLOAD=PAYLOAD_JSON_LITERAL
CODE=PROBE_BASE64_LITERAL

def launch():
    payload=json.loads(PAYLOAD);bind_profile(payload['pins']);tag=payload['operation_tag'];phase=payload['phase'];need(phase in ('startup','metadata','stopcheck'),'phase')
    unit=tag+'-'+phase+'.service';runtime=240 if phase=='metadata' else (20 if phase=='stopcheck' else payload['observation_seconds']+30)
    need((runtime==240 if phase=='metadata' else (runtime==20 if phase=='stopcheck' else 31<=runtime<=1980)),'phase runtime')
    need(os.getuid()==profile()['worker_uid'] and Path.home()==Path(profile()['worker_home']),'remote user')
    parent=Path.home()/'.local/state/sparkles-boat';need(parent.resolve()==parent,'remote state directory')
    operation=parent/('qualification-'+tag+'-'+phase)
    program=base64.b64decode(CODE).decode();program_sha=digest(program.encode())
    def command(argv,limit=15,summary=False):
        r=subprocess.run(argv,stdin=subprocess.DEVNULL,stdout=subprocess.PIPE,stderr=subprocess.PIPE if summary else subprocess.DEVNULL,timeout=limit)
        need(len(r.stdout)<=1024*1024 and len(r.stderr or b'')<=1024*1024,'output bound');return r
    def show():
        r=command(['systemctl','--user','show',unit,'--property=LoadState,ActiveState,MainPID,ControlGroup,InvocationID,ExecMainStatus,MemoryPeak,MemorySwapPeak,Result'])
        need(r.returncode==0,'unit metadata unavailable')
        return dict(x.split('=',1)for x in r.stdout.decode().splitlines()if '='in x)
    def finish(run_exit=None,summary=None):
        result=load(operation/'probe-result.json');need(result.get('program_sha256')==program_sha and result.get('phase')==phase and result.get('unit')==unit,'remote result binding')
        loaded=show();cg=loaded.get('ControlGroup','');closed=loaded.get('MainPID')=='0' and loaded.get('ActiveState')in('inactive','failed')
        if cg:
            root=Path('/sys/fs/cgroup')/cg.lstrip('/')
            if root.exists():closed=closed and all(not path.read_text().strip()for path in root.rglob('cgroup.procs'))
        healthy,code,evidence=terminal_health(result,loaded,closed,run_exit,summary)
        result.update({'metadata_closed':closed,'phase_closed':closed,'metadata_unit':unit,'terminal_properties':loaded,'terminal_evidence':evidence})
        if not healthy:
            result['qualified']=False;result['startup_closed']=False
            # The probe's own code names its refusal. Otherwise the unit's end does.
            if not isinstance(result.get('reason_code'),str):result['reason_code']=bounded_text(code)
        return result
    if operation.exists():
        need(operation.resolve()==operation and operation.stat().st_uid==profile()['worker_uid'],'operation recovery owner')
        if (operation/'probe-result.json').exists():print(json.dumps(finish()));return
        print(json.dumps({'qualified':False,'startup_closed':False,'phase_closed':False,'existing_phase_pending':True,'reason_code':'existing_phase_pending','unit':unit,'engine_or_job_started':False}));return
    operation.mkdir(mode=0o700)
    initial=show();need(initial.get('LoadState')=='not-found' and initial.get('ActiveState')=='inactive' and initial.get('MainPID')=='0','unit name reused')
    stattext=Path('/proc/self/stat').read_text();payload['launcher_identity']={'pid':os.getpid(),'start':int(stattext[stattext.rfind(')')+2:].split()[19])};payload['phase_runtime_seconds']=runtime;payload['result_directory']=str(operation);payload['program_sha256']=program_sha
    argv=['systemd-run','--user','--wait','--pipe','--unit='+tag+'-'+phase,'--property=Type=exec','--property=MemoryMax=512M','--property=MemorySwapMax=0','--property=MemoryAccounting=yes','--property=CPUQuota=100%','--property=RuntimeMaxSec='+str(runtime),'--property=TimeoutStopSec=10','--property=KillMode=control-group','--property=OOMPolicy=continue','--property=TasksMax=64','--property=LimitNOFILE=4096','--property=WorkingDirectory='+profile()['worker_home']+'/.local/state/sparkles-boat/context','/usr/bin/python3','-I','-B','-c',program,json.dumps(payload)]
    run=command(argv,runtime+(15 if phase=='stopcheck'else 25),summary=True)
    print(json.dumps(finish(run.returncode,run.stderr.decode('ascii','replace'))))
if __name__=='__main__':
    try:launch()
    except Exception as error:print(json.dumps({'qualified':False,'startup_closed':False,'metadata_closed':False,'phase_closed':False,'engine_or_job_started':False,'failure':'owned phase requires Root reconciliation; no arbitrary stderr','reason_code':'launcher:'+reason_code(error)}))
