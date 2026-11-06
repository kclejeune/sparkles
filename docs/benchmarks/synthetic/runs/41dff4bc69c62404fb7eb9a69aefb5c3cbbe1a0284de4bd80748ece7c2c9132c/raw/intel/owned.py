"""Exact existing owned process/output primitives; no command or main entrypoint."""
import ctypes, json, os, select, signal, time
from pathlib import Path

def require(condition,message):
    if not condition: raise ValueError(message)

def write(path,value):
    Path(path).write_text(json.dumps(value,indent=2,sort_keys=True)+"\n")


class OutputOwner:
    """Retain a directory descriptor only after this invocation exclusively creates it."""
    def __init__(self):self.directory_fd=None
    def __enter__(self):return self
    def create(self,path):
        require(self.directory_fd is None,'this invocation already owns an output directory')
        Path(path).mkdir(exist_ok=False)
        # A retained descriptor keeps failure writes inside our newly created inode,
        # even if a concurrent rename replaces its pathname. Preexisting paths never
        # reach this assignment and can never receive a failure record from us.
        self.directory_fd=os.open(path,os.O_RDONLY|os.O_DIRECTORY|os.O_NOFOLLOW)
        return Path(path)
    def __exit__(self,kind,error,traceback):
        if self.directory_fd is not None:
            try:
                if error is not None:
                    try:fd=os.open('failure.json',os.O_WRONLY|os.O_CREAT|os.O_EXCL,0o600,dir_fd=self.directory_fd)
                    except FileExistsError:pass
                    else:
                        with os.fdopen(fd,'w') as f:json.dump({'exception':kind.__name__,'message':str(error)},f);f.write('\n')
            finally:os.close(self.directory_fd);self.directory_fd=None
        return False


def enable_subreaper():
    # Adopt only our orphan descendants so their zombies can be reaped by exact PGID.
    libc=ctypes.CDLL(None,use_errno=True)
    require(libc.prctl(36,1,0,0,0)==0,'could not enable owned-descendant reaping')


def own_process(p):
    p._owned_pgid=p.pid
    return p


def group_members(pgid):
    members=[]
    for path in Path('/proc').glob('[0-9]*/stat'):
        try:
            fields=path.read_text().rsplit(') ',1)[1].split()
            if int(fields[2])==pgid:members.append((int(path.parent.name),fields[0]))
        except (OSError,ValueError,IndexError):continue
    return members


def _stop_owned(process,term_seconds=1.0,kill_seconds=2.0):
    pgid=getattr(process,'_owned_pgid',process.pid)
    require(pgid==process.pid and pgid!=os.getpgrp(),'refusing unowned process group')
    sent=[]
    def send(sig):
        try:os.killpg(pgid,sig);sent.append(sig.name)
        except ProcessLookupError:pass
    def gone():
        process.poll() # Preserve/reap the leader through Popen, not waitpid(-1).
        for pid,state in group_members(pgid):
            if pid!=process.pid and state=='Z':
                try:os.waitpid(pid,os.WNOHANG)
                except ChildProcessError:pass
        return not group_members(pgid)
    def wait_group(seconds):
        deadline=time.monotonic()+seconds
        while not gone():
            remaining=deadline-time.monotonic()
            if remaining<=0:return False
            # Bounded cleanup polling; no process-name matching or broad signals.
            import threading
            threading.Event().wait(min(.02,remaining))
        return True
    send(signal.SIGTERM)
    if not wait_group(term_seconds):send(signal.SIGKILL);require(wait_group(kill_seconds),'owned process group survived SIGKILL')
    process.wait(timeout=1)
    return {'pgid':pgid,'signals':sent,'leader_exit':process.returncode,'remaining':group_members(pgid)}


def stop_owned(process,term_seconds=1.0,kill_seconds=2.0):
    # Python can dispatch a handler on its main thread after another thread receives
    # the signal, even while the main thread's kernel mask blocks it. Defer both
    # Python handlers as well as the mask until the whole owned group is gone.
    pending=[];signals={signal.SIGTERM,signal.SIGINT}
    previous_mask=signal.pthread_sigmask(signal.SIG_BLOCK,signals)
    handlers={sig:signal.getsignal(sig) for sig in signals}
    def defer(signum,frame):pending.append((signum,frame))
    for sig in signals:signal.signal(sig,defer)
    try:return _stop_owned(process,term_seconds,kill_seconds)
    finally:
        try:signal.pthread_sigmask(signal.SIG_SETMASK,previous_mask)
        finally:
            for sig,handler in handlers.items():signal.signal(sig,handler)
        for sig,frame in pending:
            handler=handlers[sig]
            if callable(handler):handler(sig,frame)
            elif handler!=signal.SIG_IGN:raise SystemExit(128+sig)


def cleanup_logged(process,log):
    try:
        result=stop_owned(process)
    except BaseException as error:
        write(Path(str(log)+'.cleanup.json'),{'pgid':getattr(process,'_owned_pgid',process.pid),'error':str(error),'remaining':group_members(process.pid),'leader_exit':process.poll()})
        raise
    else:write(Path(str(log)+'.cleanup.json'),result)
    finally:Path(str(log)+'.exit').write_text(str(process.returncode)+'\n')
    return result


def unit_members(cgroup):
    """The exclusive service and every nested cgroup, irrespective of PGID/session."""
    files = [cgroup / 'cgroup.procs', *sorted(cgroup.rglob('*/cgroup.procs'))]
    members = set()
    for path in files:
        try:
            members.update(int(pid) for pid in path.read_text().split())
        except FileNotFoundError:
            if path == cgroup / 'cgroup.procs':
                raise
    return members


def require_unit_idle(cgroup):
    members = unit_members(cgroup)
    require(members == {os.getpid()}, 'unexpected service descendants: ' + repr(sorted(members - {os.getpid()})))


def process_start(pid):
    # /proc/PID/stat field 22; ')' terminates comm even when comm contains spaces.
    return int(Path('/proc', str(pid), 'stat').read_text().rsplit(')', 1)[1].split()[19])


def cleanup_owned_unit(cgroup, receipts, child=None):
    """Signal/await only pidfds revalidated inside this exclusive named service."""
    deadline = time.monotonic() + 10
    for round_number in range(8):
        if child is not None:
            child.poll()  # Reap our direct command promptly if it has exited.
        members = unit_members(cgroup) - {os.getpid()}
        if not members:
            require_unit_idle(cgroup)
            return
        descriptors = []
        try:
            for pid in sorted(members):
                fd = None
                try:
                    started = process_start(pid)
                    fd = os.pidfd_open(pid)
                    require(process_start(pid) == started, 'owned PID identity changed before cleanup')
                    require(pid in unit_members(cgroup), 'owned PID moved outside the service; refusing signal')
                    number = signal.SIGTERM if round_number == 0 else signal.SIGKILL
                    signal.pidfd_send_signal(fd, number)
                    receipts.append({'pid': pid, 'start_ticks': started, 'signal': int(number), 'pidfd_identity_validated': True})
                    descriptors.append(fd)
                    fd = None
                except ProcessLookupError:
                    pass
                except FileNotFoundError:
                    pass
                finally:
                    if fd is not None:
                        os.close(fd)
            pending = list(descriptors)
            until = min(deadline, time.monotonic() + 2)
            while pending and time.monotonic() < until:
                exited, _, _ = select.select(pending, [], [], max(0, until - time.monotonic()))
                pending = [fd for fd in pending if fd not in exited]
            require(time.monotonic() < deadline, 'owned-unit cleanup deadline')
        finally:
            for fd in descriptors:
                os.close(fd)
    require_unit_idle(cgroup)
