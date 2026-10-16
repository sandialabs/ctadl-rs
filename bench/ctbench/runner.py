"""The daemon: watch the tracked branch, build each commit, work through its pass, and sweep the
whole firmware corpus in the background -- without getting in the user's way.

Two kinds of work, never mixed:

* **Exclusive** work -- a build, or a unit of a commit's pass -- runs alone, so a p-tier
  measurement never shares the machine with our own work. Pass work always goes first: while
  any is waiting, no new sweep worker starts, and running ones drain.
* **Sweep** units run several at a time (`workers_p` when quiet, `workers_e` otherwise). Their
  wall times are shared, so they are kept as `wall_shared_s` and never compared.

Tiers follow the activity gate:

* p tier (normal priority) only after the machine has been quiet for `required_quiet_s`.
* When the user comes back during p-tier work: an index/query pass unit is killed and requeued
  (at most 30 minutes lost); an import, a build or a sweep unit is demoted in place. A pass
  unit killed `max_activity_kills` times runs on the e tier from then on.
* e-tier work is promoted when the machine goes quiet; its wall time is then not clean.
* Memory pressure: exclusive work is stopped (SIGSTOP) until pressure is normal; sweep workers
  are shed youngest first (killed and requeued) down to one.

Wall time is recorded only for clean runs: started on p, never demoted, promoted or stopped.
Instruction counts, cycles and peak footprint are recorded for every run that finishes.

Disk: a benchmark's store (its import and index) is deleted as soon as its last phase is over,
however it ended, and a periodic pass removes any store nothing will read again.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import subprocess
import time
import traceback
from dataclasses import dataclass
from pathlib import Path

from . import activity, db as dbm, proc, score, suites

SWEEP = "firmware-sweep"


@dataclass
class Active:
    kind: str                  # unit | build
    run: proc.Running
    timeout_s: float
    unit: dict | None = None   # the units row, for kind == unit
    run_id: int | None = None
    sha: str | None = None     # for kind == build
    target: str | None = None  # build: "ctadl" or "xtask"
    paused: bool = False
    effective_s: float = 0.0   # time charged against the timeout; e-tier time counts less
    last_tick: float = 0.0

    @property
    def sweep(self) -> bool:
        return self.kind == "unit" and self.unit["suite"] == SWEEP


class Daemon:
    def __init__(self, cfg: dict, inputs: dict):
        self.cfg = cfg
        self.inputs = inputs
        self.gate = cfg["gate"]
        self.root: Path = cfg["paths"]["root"]
        self.repo: Path = cfg["paths"]["repo"]
        self.bench_repo: Path = cfg["paths"]["bench_repo"]
        self.conn = dbm.connect(self.root / "bench.db")
        self.memguard = self.bench_repo / "bench" / "memguard.sh"
        self.active: list[Active] = []
        self.last_busy = time.time()   # assume busy at startup; quiet has to be observed
        self.next_poll = 0.0
        self.next_gc = 0.0
        self.xtask: str | None = None
        self.xtask_retry_at = 0.0
        self.last_gate_state = None
        self.git = inputs.get("git", "git")
        for d in ("tmp", "home"):
            (self.root / d).mkdir(parents=True, exist_ok=True)
        path = ":".join([str(Path.home() / ".nix-profile/bin"), "/nix/var/nix/profiles/default/bin",
                         "/usr/bin", "/bin", "/usr/sbin", "/sbin"])
        # git talks to the remote as the user, so it keeps the real HOME (ssh keys, config).
        self.git_env = {"PATH": path, "HOME": str(Path.home()), "LANG": "en_US.UTF-8"}
        # Everything else keeps its scratch on the big volume: TMPDIR for Rust's tempfile (the
        # Ghidra project and export dirs), java.io.tmpdir for the JVM (which ignores TMPDIR on
        # macOS), and HOME for Ghidra's per-user cache and cargo's registry.
        self.env = {
            "PATH": path,
            "HOME": str(self.root / "home"),
            "TMPDIR": str(self.root / "tmp"),
            "JAVA_TOOL_OPTIONS": f"-Djava.io.tmpdir={self.root / 'tmp'}",
            "LANG": "en_US.UTF-8",
            # Pin any thread pool a phase might start, so instruction counts do not depend on
            # how many cores the tier gave it. ctadl index is serial by default.
            "RAYON_NUM_THREADS": "4",
            # HOME moved, so point nix at the user's own config (flakes are enabled there).
            "NIX_USER_CONF_FILES": str(Path.home() / ".config" / "nix" / "nix.conf"),
            "NIX_REMOTE": "daemon",
        }

    # --- bookkeeping ---------------------------------------------------------------------------

    def log(self, kind: str, detail: str = "") -> None:
        dbm.event(self.conn, kind, detail)
        print(f"{time.strftime('%Y-%m-%d %H:%M:%S')} {kind} {detail}", flush=True)

    def recover(self) -> None:
        """A restart leaves nothing running: put interrupted work back on the queue."""
        n = self.conn.execute("UPDATE runs SET outcome='killed', reason='daemon restart', "
                              "ended_at=? WHERE outcome IS NULL", (time.time(),)).rowcount
        self.conn.execute("UPDATE units SET status='pending' WHERE status='running'")
        self.conn.execute("UPDATE commits SET build_status='pending' WHERE build_status='building'")
        if n:
            self.log("recover", f"{n} interrupted run(s) requeued")

    # --- the activity gate ---------------------------------------------------------------------

    def observe(self) -> tuple[activity.Snapshot, bool, bool]:
        """Returns (snapshot, user_here, p_ok)."""
        snap = activity.snapshot(self.gate, {a.run.pid for a in self.active})
        now = snap.at
        reasons = snap.busy_reasons(self.gate)
        if any(not r.startswith("typed") for r in reasons):
            self.last_busy = now
        self.last_busy = max(self.last_busy, now - snap.input_age_s)
        quiet_for = now - self.last_busy
        user_here = bool(reasons)
        p_ok = quiet_for >= activity.required_quiet_s(self.gate, now)
        state = "p-ok" if p_ok else "busy" if user_here else "quiet"
        if state != self.last_gate_state:
            self.log("gate", f"{state}: quiet {quiet_for:.0f}s; " + "; ".join(reasons))
            self.last_gate_state = state
        return snap, user_here, p_ok

    # --- tracking the branch -------------------------------------------------------------------

    def git_out(self, *args: str, repo: Path | None = None) -> str:
        return subprocess.run([self.git, "-C", str(repo or self.repo), *args], capture_output=True,
                              text=True, check=True, env=self.git_env).stdout.strip()

    def poll(self) -> None:
        t = self.cfg["track"]
        self.next_poll = time.time() + t["poll_s"]
        try:
            subprocess.run([self.git, "-C", str(self.repo), "fetch", "--quiet", t["remote"], t["branch"]],
                           capture_output=True, timeout=120, env=self.git_env, check=True)
        except (subprocess.SubprocessError, OSError) as e:
            self.log("fetch-failed", str(e)[:300])
        self.add_commit(self.git_out("rev-parse", f"{t['remote']}/{t['branch']}"))

    def add_commit(self, sha: str) -> bool:
        if self.conn.execute("SELECT 1 FROM commits WHERE sha=?", (sha,)).fetchone():
            return False
        subject, ct = self.git_out("log", "-1", "--format=%s%x00%ct", sha).split("\0")
        self.conn.execute("INSERT INTO commits(sha, subject, committed_at, seen_at) VALUES (?,?,?,?)",
                          (sha, subject, int(ct), int(time.time())))
        self.log("commit", f"{sha[:10]} {subject}")
        return True

    def backfill(self) -> bool:
        """Queue the commit in the middle of the longest unmeasured stretch of first-parent
        history between the oldest measured commit and the tip."""
        if not self.cfg["track"].get("backfill"):
            return False
        known = {r["sha"] for r in self.conn.execute("SELECT sha FROM commits")}
        t = self.cfg["track"]
        history = self.git_out("rev-list", "--first-parent", f"{t['remote']}/{t['branch']}").split()
        history.reverse()  # oldest first
        idx = [i for i, s in enumerate(history) if s in known]
        gaps = [(b - a, a, b) for a, b in zip(idx, idx[1:]) if b - a >= 2]
        if not gaps:
            return False
        _, a, b = max(gaps)
        mid = history[(a + b) // 2]
        self.log("backfill", f"{mid[:10]} between {history[a][:10]} and {history[b][:10]}")
        return self.add_commit(mid)

    # --- builds --------------------------------------------------------------------------------

    def start_build(self, target: str, sha: str, tier: str) -> None:
        """Build `ctadl` at a tracked commit, or `xtask` from the bench repo, with cargo in the
        commit's own pinned dev shell. cargo runs as our child, so it moves between tiers like
        everything else, and the shared target dir keeps rebuilds incremental."""
        if target == "ctadl":
            src = self.root / "src"
            if not src.exists():
                subprocess.run([self.git, "-C", str(self.repo), "worktree", "add", "--detach",
                                str(src), sha], check=True, capture_output=True, env=self.git_env)
            subprocess.run([self.git, "-C", str(src), "checkout", "--quiet", "--detach", "--force", sha],
                           check=True, capture_output=True, env=self.git_env)
            flake = f"git+file://{self.repo}?rev={sha}"
            cargo = ["cargo", "build", "--release", "-p", "ctadl-ascent", "--bin", "ctadl"]
            target_dir = self.root / "target"
            self.conn.execute("UPDATE commits SET build_status='building' WHERE sha=?", (sha,))
        else:
            # The tracked branch's dev shell, not the bench branch's: the bench branch carries
            # taintbench's flake, whose dev shell does not build on macOS. The toolchain is the
            # same, since the bench branch is merged from the tracked one.
            src = self.bench_repo
            t = self.cfg["track"]
            tip = self.git_out("rev-parse", f"{t['remote']}/{t['branch']}")
            flake = f"git+file://{self.repo}?rev={tip}"
            cargo = ["cargo", "build", "--release", "-p", "xtask"]
            target_dir = self.root / "target-bench"
        env = {**self.env, "CARGO_TARGET_DIR": str(target_dir)}
        log = self.root / "builds" / sha / f"{target}.log"
        argv = ["nix", "develop", flake, "--command", *cargo]
        run = proc.launch(argv, cwd=src, env=env, log_path=log, time_file=log.with_suffix(".time"),
                          memguard=self.memguard, mem_cap_gib=self.cfg["build"]["mem_cap_gib"], tier=tier)
        self.active.append(Active(kind="build", run=run, timeout_s=self.cfg["build"]["timeout_s"],
                                  sha=sha, target=target, last_tick=time.time()))
        self.log("build-start", f"{target} {sha[:10]} tier={tier}")

    def finish_build(self, a: Active, rc: int) -> None:
        sha, target, log = a.sha, a.target, a.run.log_path
        ok = rc == 0 and a.run.kill_reason is None
        if target == "ctadl":
            if ok:
                out = self.root / "builds" / sha / "ctadl"
                shutil.copy2(self.root / "target" / "release" / "ctadl", out)
                self.conn.execute("UPDATE commits SET build_status='ok', build_out=?, build_log=? WHERE sha=?",
                                  (str(out), str(log), sha))
                n = suites.enqueue_pass(self.cfg, self.inputs, self.conn, sha)
                self.log("build-ok", f"{sha[:10]}; queued {n} units")
            else:
                status = "pending" if a.run.kill_reason == "daemon stopping" else "failed"
                self.conn.execute("UPDATE commits SET build_status=?, build_log=? WHERE sha=?",
                                  (status, str(log), sha))
                self.log("build-failed", f"{sha[:10]} rc={rc} {a.run.kill_reason or ''} log={log}")
        elif ok:
            out = self.root / "builds" / sha / "xtask"
            shutil.copy2(self.root / "target-bench" / "release" / "xtask", out)
            self.xtask = str(out)
            self.log("build-ok", f"xtask {sha[:10]}")
        else:
            # Only TaintBench scoring needs xtask; keep measuring and try again in an hour.
            self.xtask_retry_at = time.time() + 3600
            self.log("build-failed", f"xtask rc={rc} log={log}; retrying in an hour")

    # --- units ---------------------------------------------------------------------------------

    def pass_commit(self) -> str | None:
        """The commit whose pass to work on: finish the one in progress before starting
        another, then the newest built commit."""
        row = self.conn.execute(
            "SELECT u.sha FROM units u JOIN commits c ON c.sha=u.sha "
            "WHERE u.status='pending' AND u.suite!=? AND c.build_status='ok' "
            "ORDER BY EXISTS (SELECT 1 FROM units d WHERE d.sha=u.sha AND d.suite!=? "
            "                 AND d.status IN ('done','failed','skipped')) DESC, "
            "c.committed_at DESC LIMIT 1", (SWEEP, SWEEP)).fetchone()
        return row["sha"] if row else None

    def next_unit(self, sha: str, sweep: bool):
        op = "=" if sweep else "!="
        while True:
            u = self.conn.execute(
                "SELECT u.*, a.status AS after_status FROM units u LEFT JOIN units a ON a.id=u.after "
                f"WHERE u.sha=? AND u.suite{op}? AND u.status='pending' "
                "AND (u.after IS NULL OR a.status NOT IN ('pending','running')) "
                "ORDER BY u.seq LIMIT 1", (sha, SWEEP)).fetchone()
            if u is None:
                return None
            if u["after"] is not None and u["after_status"] != "done":
                self.conn.execute("UPDATE units SET status='skipped' WHERE id=?", (u["id"],))
                self.maybe_clean(u)
                continue
            return u

    def work_dir(self, u) -> Path:
        return self.root / "work" / u["sha"][:12] / u["suite"] / u["bench"]

    def maybe_clean(self, u) -> None:
        """Free the benchmark's store once nothing will read it again."""
        left = self.conn.execute("SELECT count(*) FROM units WHERE sha=? AND suite=? AND bench=? "
                                 "AND status IN ('pending','running')",
                                 (u["sha"], u["suite"], u["bench"])).fetchone()[0]
        if left == 0:
            shutil.rmtree(self.work_dir(u) / "store", ignore_errors=True)

    def gc_stores(self) -> None:
        """Remove any store whose benchmark has nothing left to run, e.g. after a crash."""
        self.next_gc = time.time() + 3600
        live = {(r["sha"][:12], r["suite"], r["bench"]) for r in self.conn.execute(
            "SELECT DISTINCT sha, suite, bench FROM units WHERE status IN ('pending','running')")}
        removed = 0
        for store in (self.root / "work").glob("*/*/*/store"):
            sha12, suite, bench = store.parts[-4:-1]
            if (sha12, suite, bench) not in live:
                shutil.rmtree(store, ignore_errors=True)
                removed += 1
        # Scratch a killed Ghidra or JVM left behind. Nothing we run lasts a day without
        # touching its own scratch, so older entries are orphans.
        cutoff = time.time() - 86400
        for entry in (self.root / "tmp").iterdir():
            try:
                if entry.lstat().st_mtime < cutoff:
                    shutil.rmtree(entry) if entry.is_dir() and not entry.is_symlink() else entry.unlink()
                    removed += 1
            except OSError:
                pass
        if removed:
            self.log("gc", f"removed {removed} orphaned store(s) and scratch entries")

    def activity_kills(self, unit_id: int) -> int:
        return self.conn.execute("SELECT count(*) FROM runs WHERE unit_id=? AND outcome='killed' "
                                 "AND reason LIKE 'user active%'", (unit_id,)).fetchone()[0]

    def start_unit(self, u, tier: str) -> None:
        spec = json.loads(u["spec"])
        work = self.work_dir(u)
        work.mkdir(parents=True, exist_ok=True)
        ctadl = self.conn.execute("SELECT build_out FROM commits WHERE sha=?", (u["sha"],)).fetchone()[0]
        fill = {"{ctadl}": ctadl, "{store}": str(work / "store")}
        argv = [fill.get(a, a).replace("{work}", str(work)) for a in spec["argv"]]
        run_id = self.conn.execute(
            "INSERT INTO runs(unit_id, started_at, start_tier, host_load) VALUES (?,?,?,?)",
            (u["id"], time.time(), tier, os.getloadavg()[0])).lastrowid
        log = work / f"{u['phase']}.{run_id}.log"
        run = proc.launch(argv, cwd=work, env={**self.env, **spec.get("env", {})}, log_path=log,
                          time_file=work / f"{u['phase']}.{run_id}.time", memguard=self.memguard,
                          mem_cap_gib=spec["mem_cap_gib"], tier=tier)
        self.conn.execute("UPDATE runs SET log_path=? WHERE id=?", (str(log), run_id))
        self.conn.execute("UPDATE units SET status='running', attempts=attempts+1 WHERE id=?", (u["id"],))
        self.active.append(Active(kind="unit", run=run, timeout_s=spec["timeout_s"], unit=dict(u),
                                  run_id=run_id, last_tick=time.time()))
        if u["suite"] != SWEEP:
            self.log("unit-start", f"{u['sha'][:10]} {u['suite']}/{u['bench']}/{u['phase']} "
                     f"tier={tier} run={run_id}")

    def finish_unit(self, a: Active, rc: int) -> None:
        r, u, run_id = a.run, a.unit, a.run_id
        if r.kill_reason == "timeout":
            outcome = "timeout"
        elif r.kill_reason:
            outcome = "killed"
        elif rc == 0:
            outcome = "ok"
        elif rc == 137:
            outcome = "oom"  # memguard's exit status for "over the cap"
        else:
            outcome = "failed"
        self.conn.execute(
            "UPDATE runs SET ended_at=?, demoted=?, promoted=?, stopped=?, outcome=?, reason=?, exit_code=? "
            "WHERE id=?", (time.time(), int(r.demoted), int(r.promoted), int(r.stopped), outcome,
                           r.kill_reason, rc, run_id))
        if outcome == "killed":
            self.conn.execute("UPDATE units SET status='pending' WHERE id=?", (u["id"],))
            self.log("unit-killed", f"{u['suite']}/{u['bench']}/{u['phase']} run={run_id}: {r.kill_reason}")
            return
        metrics = proc.parse_time_file(r.time_file)
        real = metrics.pop("real_s", None)
        if real is not None and outcome == "ok":
            if a.sweep:
                metrics["wall_shared_s"] = real
            elif r.start_tier == "p" and not (r.demoted or r.promoted or r.stopped):
                metrics["wall_s"] = real
        self.put_metrics(run_id, metrics)
        self.conn.execute("UPDATE units SET status=? WHERE id=?",
                          ("done" if outcome == "ok" else "failed", u["id"]))
        if not a.sweep or outcome != "ok":
            self.log("unit-" + outcome, f"{u['suite']}/{u['bench']}/{u['phase']} run={run_id} "
                     f"wall={metrics.get('wall_s', '-')} instr={metrics.get('instructions', '-')}")
        spec = json.loads(u["spec"])
        if outcome == "ok" and "score" in spec:
            try:
                q, arts = score.score(spec["score"], self.work_dir(u), self)
                self.put_metrics(run_id, q)
                self.conn.executemany("INSERT OR REPLACE INTO artifacts(run_id, name, value) VALUES (?,?,?)",
                                      [(run_id, k, v) for k, v in arts.items()])
            except Exception:
                err = traceback.format_exc()
                self.conn.execute("INSERT OR REPLACE INTO artifacts(run_id, name, value) VALUES (?,?,?)",
                                  (run_id, "score_error", err))
                self.log("score-error", f"{u['suite']}/{u['bench']}: {err.splitlines()[-1]}")
        self.maybe_clean(u)

    def put_metrics(self, run_id: int, metrics: dict) -> None:
        self.conn.executemany("INSERT OR REPLACE INTO metrics(run_id, name, value) VALUES (?,?,?)",
                              [(run_id, k, float(v)) for k, v in metrics.items()])

    # --- the sweep -----------------------------------------------------------------------------

    def sweep_sha(self) -> str | None:
        """The commit the current sweep is pinned to, starting a new sweep when the last one is
        complete and a newer commit has been built."""
        s = self.cfg.get("sweep", {})
        if not s.get("enabled"):
            return None
        sha = dbm.get_state(self.conn, "sweep.sha")
        total = len(suites.sweep_list(self.cfg))
        if sha:
            cursor = int(dbm.get_state(self.conn, "sweep.cursor", "0"))
            pending = self.conn.execute("SELECT count(*) FROM units WHERE sha=? AND suite=? "
                                        "AND status IN ('pending','running')", (sha, SWEEP)).fetchone()[0]
            if cursor < total or pending:
                if pending < 3 * self.cfg["sweep"]["workers_p"] and cursor < total:
                    n = suites.enqueue(self.conn, sha, suites.sweep_batch(self.cfg, self.inputs, cursor,
                                                                          s["batch"]))
                    dbm.set_state(self.conn, "sweep.cursor", str(min(total, cursor + s["batch"])))
                    if n == 0:
                        return sha
                return sha
            self.log("sweep-done", f"{sha[:10]}: {total} binaries")
        newest = self.conn.execute("SELECT sha FROM commits WHERE build_status='ok' "
                                   "ORDER BY committed_at DESC LIMIT 1").fetchone()
        if newest is None or newest["sha"] == sha:
            return None
        dbm.set_state(self.conn, "sweep.sha", newest["sha"])
        dbm.set_state(self.conn, "sweep.cursor", "0")
        dbm.set_state(self.conn, "sweep.started_at", str(int(time.time())))
        self.log("sweep-start", f"{newest['sha'][:10]}: {total} binaries")
        return self.sweep_sha()

    # --- the loop ------------------------------------------------------------------------------

    def supervise(self, a: Active, snap, user_here: bool, p_ok: bool) -> None:
        r = a.run
        rc = r.popen.poll()
        if rc is not None:
            self.active.remove(a)
            (self.finish_build if a.kind == "build" else self.finish_unit)(a, rc)
            return
        now = time.time()
        if not a.paused:
            weight = 1.0 if r.tier == "p" else self.cfg["timeouts"]["e_time_weight"]
            a.effective_s += (now - a.last_tick) * weight
        a.last_tick = now
        if a.effective_s > a.timeout_s:
            proc.kill(r, "timeout")
            return  # reaped and finished on the next tick
        if a.paused:
            if snap.pressure == 1:
                proc.cont(r)
                a.paused = False
                self.log("continue", self.describe(a))
            return
        if r.tier == "p" and user_here:
            if a.kind == "build" or a.sweep or a.unit["phase"] == "import":
                proc.demote(r)
                if not a.sweep:
                    self.log("demote", self.describe(a))
            else:
                proc.kill(r, "user active: " + "; ".join(snap.busy_reasons(self.gate)))
        elif r.tier == "e" and p_ok and not self.forced_e(a):
            proc.promote(r)
            if not a.sweep:
                self.log("promote", self.describe(a))

    def relieve_pressure(self, snap) -> None:
        if snap.pressure == 1:
            return
        sweepers = sorted((a for a in self.active if a.sweep and a.run.kill_reason is None),
                          key=lambda a: a.run.started_at)
        if len(sweepers) > 1:
            proc.kill(sweepers[-1].run, "memory pressure")  # requeued, like a user kill
            self.log("shed", f"memory pressure {snap.pressure}: {self.describe(sweepers[-1])}")
            return
        for a in self.active:
            if not a.paused and not a.sweep:
                proc.stop(a.run)
                a.paused = True
                self.log("stop", f"memory pressure {snap.pressure}: {self.describe(a)}")

    def forced_e(self, a: Active) -> bool:
        return (a.kind == "unit" and not a.sweep
                and self.activity_kills(a.unit["id"]) >= self.gate["max_activity_kills"])

    def describe(self, a: Active) -> str:
        if a.kind == "build":
            return f"build {a.target} {a.sha[:10]}"
        u = a.unit
        return f"{u['suite']}/{u['bench']}/{u['phase']} run={a.run_id}"

    def schedule(self, snap, p_ok: bool) -> None:
        tier = "p" if p_ok else "e"
        exclusive = [a for a in self.active if not a.sweep]
        if exclusive:
            return
        sweeping = [a for a in self.active if a.sweep]
        if self.xtask is None and time.time() >= self.xtask_retry_at:
            head = self.git_out("rev-parse", "HEAD", repo=self.bench_repo)
            built = self.root / "builds" / head / "xtask"
            if built.exists():
                self.xtask = str(built)
            elif not sweeping:
                self.start_build("xtask", head, tier)
                return
            else:
                return
        # Exclusive work waiting: let the sweep drain, then run it alone.
        pending_build = self.conn.execute("SELECT sha FROM commits WHERE build_status='pending' "
                                          "ORDER BY committed_at DESC LIMIT 1").fetchone()
        sha = self.pass_commit()
        if pending_build or sha:
            if sweeping:
                return
            if sha and self.conn.execute(
                    "SELECT 1 FROM units WHERE sha=? AND suite!=? AND status IN ('done','failed','skipped') "
                    "LIMIT 1", (sha, SWEEP)).fetchone():
                pass  # a pass in progress finishes before a newer commit is built
            elif pending_build:
                self.start_build("ctadl", pending_build["sha"], tier)
                return
            u = self.next_unit(sha, sweep=False)
            if u is not None:
                if self.activity_kills(u["id"]) >= self.gate["max_activity_kills"]:
                    tier = "e"
                self.start_unit(u, tier)
            return
        # Nothing exclusive: fill the sweep's worker slots.
        if snap.pressure > 1:
            return
        ssha = self.sweep_sha()
        if ssha is None:
            if not sweeping:
                self.backfill()
            return
        limit = self.cfg["sweep"]["workers_p" if p_ok else "workers_e"]
        while len([a for a in self.active if a.sweep]) < limit:
            u = self.next_unit(ssha, sweep=True)
            if u is None:
                break
            self.start_unit(u, tier)

    def run(self) -> None:
        self.recover()
        self.log("daemon-start", f"pid {os.getpid()}")
        stopping: list[int] = []
        signal.signal(signal.SIGTERM, lambda *_: stopping.append(1))
        signal.signal(signal.SIGINT, lambda *_: stopping.append(1))
        # tmux and ssh hang up rather than terminate; children run in their own sessions and
        # would outlive us, so a hangup shuts down the same way.
        signal.signal(signal.SIGHUP, lambda *_: stopping.append(1))
        while not stopping:
            try:
                snap, user_here, p_ok = self.observe()
                if time.time() >= self.next_poll:
                    self.poll()
                if time.time() >= self.next_gc:
                    self.gc_stores()
                for a in list(self.active):
                    self.supervise(a, snap, user_here, p_ok)
                self.relieve_pressure(snap)
                self.schedule(snap, p_ok)
            except SystemExit:
                raise
            except Exception:
                self.log("error", traceback.format_exc()[-2000:])
            time.sleep(self.gate["tick_s"])
        for a in list(self.active):
            proc.kill(a.run, "daemon stopping")
            rc = a.run.popen.wait()
            self.active.remove(a)
            if a.kind == "unit":
                self.finish_unit(a, rc)
            else:
                self.conn.execute("UPDATE commits SET build_status='pending' WHERE sha=? "
                                  "AND build_status='building'", (a.sha,))
        self.log("daemon-stop")
