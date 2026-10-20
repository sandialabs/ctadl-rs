"""Launching one measured ctadl invocation and moving it between tiers while it runs.

The process chain is

    sh (stops itself) -> memguard.sh <cap> -> /usr/bin/time -l -o <file> -> ctadl ...

`time -l` reports instructions, cycles and peak footprint for the process it starts and only
that one (it reads them from the child before reaping it), which is ctadl -- except that a
pcode import's Ghidra JVM is ctadl's child, so its CPU shows up in user/sys time but not in
the instruction count. memguard kills the whole tree if its footprint passes the cap.

Tiers: background QoS (`taskpolicy -b`) keeps a process on the efficiency cores and throttles
its I/O. A process *launched* under `taskpolicy -b` cannot be promoted later (`taskpolicy -B -p`
has no effect on it), but one launched normally and then demoted with `taskpolicy -b -p` can be.
So every run starts normally, stopped: the shell at the head of the chain SIGSTOPs itself
before exec'ing anything, the parent applies the starting tier, then lets it go with SIGCONT.
Children spawned later inherit the tier; moving an already-running tree re-applies it to every
process in it.
"""

from __future__ import annotations

import os
import re
import signal
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

from . import activity

_STOP_THEN_EXEC = 'kill -STOP $$; exec "$@"'


@dataclass
class Running:
    popen: subprocess.Popen
    time_file: Path
    log_path: Path
    tier: str                      # current tier, p or e
    start_tier: str
    started_at: float
    demoted: bool = False
    promoted: bool = False
    stopped: bool = False
    kill_reason: str | None = None
    notes: list[str] = field(default_factory=list)

    @property
    def pid(self) -> int:
        return self.popen.pid


def launch(argv: list[str], *, cwd: Path, env: dict[str, str], log_path: Path, time_file: Path,
           memguard: Path, mem_cap_gib: int, tier: str) -> Running:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    time_file.unlink(missing_ok=True)
    full = ["/bin/sh", "-c", _STOP_THEN_EXEC, "sh",
            str(memguard), str(mem_cap_gib),
            "/usr/bin/time", "-l", "-o", str(time_file), *argv]
    log = open(log_path, "ab")
    log.write(f"# argv: {argv}\n# cwd: {cwd}\n# tier: {tier}\n".encode())
    log.flush()
    popen = subprocess.Popen(full, cwd=cwd, env=env, stdin=subprocess.DEVNULL, stdout=log,
                             stderr=subprocess.STDOUT, start_new_session=True)
    log.close()
    # Wait for the self-stop so the tier is in place before the first instruction is counted.
    deadline = time.time() + 10
    while True:
        pid, status = os.waitpid(popen.pid, os.WUNTRACED | os.WNOHANG)
        if pid and os.WIFSTOPPED(status):
            break
        if pid:  # exited before stopping; let Popen see the status
            popen.returncode = os.waitstatus_to_exitcode(status)
            break
        if time.time() > deadline:
            raise RuntimeError(f"run {popen.pid} never stopped before exec")
        time.sleep(0.01)
    r = Running(popen=popen, time_file=time_file, log_path=log_path, tier="p",
                start_tier=tier, started_at=time.time())
    if tier == "e":
        _taskpolicy("-b", [popen.pid])
        r.tier = "e"
    if popen.returncode is None:
        os.kill(popen.pid, signal.SIGCONT)
    return r


def tree(r: Running) -> list[int]:
    return sorted(activity.descendants(activity.list_procs(), {r.pid}))


def _taskpolicy(flag: str, pids: list[int]) -> None:
    for pid in pids:
        subprocess.run(["/usr/sbin/taskpolicy", flag, "-p", str(pid)],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def demote(r: Running) -> None:
    if r.tier == "e":
        return
    _taskpolicy("-b", tree(r))
    r.tier = "e"
    if r.start_tier == "p":
        r.demoted = True


def promote(r: Running) -> None:
    if r.tier == "p":
        return
    _taskpolicy("-B", tree(r))
    r.tier = "p"
    if r.start_tier == "e":
        r.promoted = True


def _measured(r: Running) -> list[int]:
    """The processes doing the work: everything under /usr/bin/time. memguard's shell and its
    sleeps, and time itself, are left alone -- a shell that sees its child stop turns that
    into an exit status."""
    procs = activity.list_procs()
    timers = [p.pid for p in procs if p.comm == "time" and p.pid in activity.descendants(procs, {r.pid})]
    return sorted(activity.descendants(procs, set(timers)) - set(timers))


def stop(r: Running) -> None:
    for pid in _measured(r):
        try:
            os.kill(pid, signal.SIGSTOP)
        except ProcessLookupError:
            pass
    r.stopped = True
    r.notes.append(f"stopped at {time.time():.0f}")


def cont(r: Running) -> None:
    for pid in _measured(r):
        try:
            os.kill(pid, signal.SIGCONT)
        except ProcessLookupError:
            pass
    r.notes.append(f"continued at {time.time():.0f}")


def terminate(r: Running, reason: str) -> None:
    """Ask the run to exit, without waiting; `kill` finishes the job."""
    r.kill_reason = reason
    for sig in (signal.SIGTERM, signal.SIGCONT):
        try:
            os.killpg(r.pid, sig)
        # macOS reports EPERM for a group whose members have all exited but not been reaped.
        except (ProcessLookupError, PermissionError):
            return


def kill(r: Running, reason: str) -> None:
    terminate(r, reason)
    if r.popen.poll() is not None:
        return
    # Anything that escaped the process group (none should) goes by tree.
    try:
        r.popen.wait(timeout=20)
    except subprocess.TimeoutExpired:
        for pid in tree(r):
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        r.popen.wait()


# --- /usr/bin/time -l output ------------------------------------------------------------------

_TIME_FIELDS = {
    "maximum resident set size": "maxrss_b",
    "instructions retired": "instructions",
    "cycles elapsed": "cycles",
    "peak memory footprint": "peak_footprint_b",
    "involuntary context switches": "invol_csw",
}
_REAL = re.compile(r"^\s*([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys")


def parse_time_file(path: Path) -> dict[str, float]:
    """The numbers from `time -l -o`, empty when the run never got that far."""
    try:
        text = path.read_text()
    except OSError:
        return {}
    out: dict[str, float] = {}
    for line in text.splitlines():
        if m := _REAL.match(line):
            out["real_s"], out["user_s"], out["sys_s"] = map(float, m.groups())
            continue
        parts = line.strip().split(None, 1)
        if len(parts) == 2 and parts[1] in _TIME_FIELDS:
            try:
                out[_TIME_FIELDS[parts[1]]] = float(parts[0])
            except ValueError:
                pass
    return out
