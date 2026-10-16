"""The results database: one SQLite file holding the commit queue, the work units, every run
of every unit, and the numbers each run produced.

A *unit* is one ctadl invocation on one benchmark at one commit (`import`, `index`, `query`, or
`report`). A unit can be *run* several times -- a run killed because the user came back is
recorded and the unit goes back on the queue -- so measurements hang off runs, not units.
"""

from __future__ import annotations

import sqlite3
import time
from pathlib import Path

SCHEMA = """
CREATE TABLE IF NOT EXISTS commits (
    sha          TEXT PRIMARY KEY,
    subject      TEXT,
    committed_at INTEGER,          -- committer date, unix seconds
    seen_at      INTEGER NOT NULL, -- when the daemon first saw it on the tracked ref
    build_status TEXT NOT NULL DEFAULT 'pending',  -- pending | building | ok | failed
    build_out    TEXT,             -- nix store path of the built ctadl
    build_log    TEXT
);

CREATE TABLE IF NOT EXISTS units (
    id         INTEGER PRIMARY KEY,
    sha        TEXT NOT NULL REFERENCES commits(sha),
    suite      TEXT NOT NULL,
    bench      TEXT NOT NULL,      -- benchmark name within the suite
    phase      TEXT NOT NULL,      -- import | index | query | report
    seq        INTEGER NOT NULL,   -- order within the commit's pass
    after      INTEGER REFERENCES units(id),  -- must finish first (the import an index reads)
    status     TEXT NOT NULL DEFAULT 'pending',
        -- pending | running | done | failed | skipped
    attempts   INTEGER NOT NULL DEFAULT 0,
    spec       TEXT NOT NULL,      -- JSON: argv and environment, see suites.py
    UNIQUE (sha, suite, bench, phase)
);
CREATE INDEX IF NOT EXISTS units_by_status ON units(status, sha);

CREATE TABLE IF NOT EXISTS runs (
    id          INTEGER PRIMARY KEY,
    unit_id     INTEGER NOT NULL REFERENCES units(id),
    started_at  REAL NOT NULL,
    ended_at    REAL,
    start_tier  TEXT NOT NULL,     -- p (normal priority) | e (background QoS, efficiency cores)
    demoted     INTEGER NOT NULL DEFAULT 0,  -- spent time demoted after starting on p
    promoted    INTEGER NOT NULL DEFAULT 0,  -- spent time promoted after starting on e
    stopped     INTEGER NOT NULL DEFAULT 0,  -- was SIGSTOPped (memory pressure)
    outcome     TEXT,              -- ok | failed | killed | oom | timeout
    reason      TEXT,              -- why killed, or the failure
    exit_code   INTEGER,
    log_path    TEXT,
    host_load   REAL               -- 1-minute load average at start, for reference
);
CREATE INDEX IF NOT EXISTS runs_by_unit ON runs(unit_id);

-- One row per number a run produced. Timing metrics come from /usr/bin/time -l on the ctadl
-- process itself; quality metrics from scoring its output.
--   wall_s is only written for clean runs: started on p, never demoted or stopped.
CREATE TABLE IF NOT EXISTS metrics (
    run_id INTEGER NOT NULL REFERENCES runs(id),
    name   TEXT NOT NULL,
    value  REAL NOT NULL,
    PRIMARY KEY (run_id, name)
);

-- Non-numeric results worth keeping, e.g. the set of findings, so a change in *which* flows
-- are reported is visible even when the count is unchanged.
CREATE TABLE IF NOT EXISTS artifacts (
    run_id INTEGER NOT NULL REFERENCES runs(id),
    name   TEXT NOT NULL,
    value  TEXT NOT NULL,
    PRIMARY KEY (run_id, name)
);

-- Activity-gate transitions and scheduler decisions, for tuning the thresholds later.
CREATE TABLE IF NOT EXISTS events (
    at     REAL NOT NULL,
    kind   TEXT NOT NULL,
    detail TEXT
);

-- Small key/value state, e.g. the rotation cursor for sampled suites.
CREATE TABLE IF NOT EXISTS state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"""


def connect(path: Path) -> sqlite3.Connection:
    path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, timeout=30, isolation_level=None)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA journal_mode=WAL")
    conn.execute("PRAGMA foreign_keys=ON")
    conn.executescript(SCHEMA)
    return conn


def event(conn: sqlite3.Connection, kind: str, detail: str = "") -> None:
    conn.execute("INSERT INTO events(at, kind, detail) VALUES (?, ?, ?)", (time.time(), kind, detail))


def get_state(conn: sqlite3.Connection, key: str, default: str | None = None) -> str | None:
    row = conn.execute("SELECT value FROM state WHERE key = ?", (key,)).fetchone()
    return row["value"] if row else default


def set_state(conn: sqlite3.Connection, key: str, value: str) -> None:
    conn.execute(
        "INSERT INTO state(key, value) VALUES (?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )
