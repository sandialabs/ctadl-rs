"""Is someone using this machine?

The user works over ssh/mosh, so the console's HID idle time is useless (it reads hours idle
while they type). What does track them is the terminal: every keystroke that reaches a shell or
a tmux client is a read() on its pty, and a read updates the device's access time. So "seconds
since the newest atime among ttys that have a process attached" is the time since the user last
typed anything, over any transport. Unattached ptys are skipped -- macOS leaves unallocated
/dev/ttys* nodes with nonsense (even future) times.

An agent or build left running while the user is away types nothing, so two more signals
catch work without keystrokes: processes from a configured list of development tools, and CPU
used by processes that are not ours. System daemons that our own work wakes (Spotlight indexing
what we write, XProtect scanning the binaries we build) are configured out of that CPU figure.

Memory pressure is not a sign of the user: our own work can cause it, and the scheduler sheds
work under pressure on its own.
"""

from __future__ import annotations

import os
import subprocess
import time
from dataclasses import dataclass, field


@dataclass
class Proc:
    pid: int
    ppid: int
    pgid: int
    cpu: float  # percent of one core, ps's decaying average
    comm: str


@dataclass
class Snapshot:
    at: float
    input_age_s: float          # since the user last typed, on any attached tty
    dev_procs: list[str]        # "name(pid)" of matching development processes, not ours
    foreign_cpu: float          # percent of one core used by processes that are not ours
    foreign_by: dict[tuple[str, int], float]  # that CPU by (comm, pid), busy processes only
    pressure: int               # kern.memorystatus_vm_pressure_level: 1 normal, 2 warn, 4 critical
    load1: float
    notes: list[str] = field(default_factory=list)

    def busy_reasons(self, cfg) -> list[str]:
        """Why the machine is not quiet right now, empty if it is."""
        why = []
        if self.input_age_s < cfg["input_idle_s"]:
            why.append(f"typed {self.input_age_s:.0f}s ago")
        if self.dev_procs:
            why.append("dev processes: " + ", ".join(self.dev_procs[:5]))
        if self.foreign_cpu > cfg["foreign_cpu_pct"]:
            why.append(f"foreign cpu {self.foreign_cpu:.0f}%")
        return why


def list_procs() -> list[Proc]:
    out = subprocess.run(
        ["/bin/ps", "-A", "-o", "pid=,ppid=,pgid=,%cpu=,comm="],
        capture_output=True, text=True, check=True,
    ).stdout
    procs = []
    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 5:
            continue
        try:
            procs.append(Proc(int(parts[0]), int(parts[1]), int(parts[2]), float(parts[3]),
                              os.path.basename(parts[4].strip())))
        except ValueError:
            continue
    return procs


def descendants(procs: list[Proc], roots: set[int]) -> set[int]:
    children: dict[int, list[int]] = {}
    for p in procs:
        children.setdefault(p.ppid, []).append(p.pid)
    seen, frontier = set(roots), list(roots)
    while frontier:
        for c in children.get(frontier.pop(), []):
            if c not in seen:
                seen.add(c)
                frontier.append(c)
    return seen


def input_age_s(now: float) -> float:
    """Seconds since the newest keystroke on any tty a process is attached to."""
    out = subprocess.run(["/bin/ps", "-A", "-o", "tty="], capture_output=True, text=True).stdout
    ttys = {t.strip() for t in out.split() if t.strip() not in ("", "??", "-")}
    newest = 0.0
    for t in ttys:
        dev = "/dev/" + (t if t.startswith("tty") else "tty" + t)
        try:
            at = os.stat(dev).st_atime
        except OSError:
            continue
        if at <= now + 5:  # ignore the future-dated nodes described above
            newest = max(newest, at)
    return now - newest if newest else float("inf")


def pressure_level() -> int:
    try:
        out = subprocess.run(["/usr/sbin/sysctl", "-n", "kern.memorystatus_vm_pressure_level"],
                             capture_output=True, text=True).stdout
        return int(out.strip())
    except ValueError:
        return 1


def snapshot(cfg, ours: set[int], building: bool = False) -> Snapshot:
    """`ours` are the pids whose trees belong to the benchmark job, excluded everywhere. Each
    leads its own process group, which also claims a member whose parent died first and left it
    to launchd -- a ctadl memguard killed, still freeing its memory after `time` is gone.
    `building`: one of ours is a build, whose `nix develop` gets its own nix-daemon worker."""
    now = time.time()
    procs = list_procs()
    mine = descendants(procs, ours | {os.getpid()}) | {p.pid for p in procs if p.pgid in ours}
    dev_names = set(cfg["dev_processes"])
    ignored = set(cfg.get("ignore_cpu", []))
    # The nix daemon itself always runs (a child of launchd); its per-build workers are what
    # mean someone is building -- unless we are, since a worker's parent is the daemon, not us.
    dev = [f"{p.comm}({p.pid})" for p in procs
           if p.pid not in mine and p.comm in dev_names
           and not (p.comm == "nix-daemon" and (p.ppid == 1 or building))]
    foreign_by = {(p.comm, p.pid): p.cpu for p in procs
                  if p.pid not in mine and p.comm not in ignored and p.cpu > 0}
    return Snapshot(
        at=now,
        input_age_s=input_age_s(now),
        dev_procs=dev,
        foreign_cpu=sum(foreign_by.values()),
        foreign_by=foreign_by,
        pressure=pressure_level(),
        load1=os.getloadavg()[0],
    )


def required_quiet_s(cfg, now: float | None = None) -> float:
    """How long the machine must have been quiet before a p-tier run may start."""
    hour = time.localtime(now).tm_hour
    start, end = cfg["night_start_hour"], cfg["night_end_hour"]
    night = (start <= hour or hour < end) if start > end else (start <= hour < end)
    return cfg["quiet_night_s"] if night else cfg["quiet_day_s"]
