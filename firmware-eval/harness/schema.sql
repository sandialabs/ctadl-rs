-- firmware-eval results database.
--
-- One SQLite file, content-addressed by binary sha256, tracks findings across
-- analyzer versions so a fix's effect is measured on the *same* binaries.
--
-- Join key for cross-tool comparison: (binary sha256, sink call-site).
-- See firmware-eval/README.md and the eval plan for the rationale.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- One row per unique file (firmware has heavy binary duplication; dedup by hash).
CREATE TABLE IF NOT EXISTS binary (
  sha256              TEXT PRIMARY KEY,
  arch                TEXT,            -- mipsel|mipseb|armel|aarch64|...
  path_example        TEXT,            -- one representative path in the corpus
  vendor              TEXT,
  firmware            TEXT,
  is_network_facing   INTEGER          -- 0/1/NULL; set by binary-selection step
);

-- One row per (binary, tool, analyzer_version). The unit of "we ran X on Y".
CREATE TABLE IF NOT EXISTS run (
  id                  INTEGER PRIMARY KEY,
  sha256              TEXT NOT NULL REFERENCES binary(sha256),
  tool                TEXT NOT NULL,   -- "cta" | "mango" | "karonte" | "satc"
  analyzer_version    TEXT NOT NULL,   -- git sha / mango image digest / dataset tag
  status              TEXT NOT NULL,   -- ok|crash|timeout|oom|unsupported
  wall_s              REAL,
  peak_mem_mb         REAL,
  exit_code           INTEGER,
  cfg_time            REAL,            -- phase timings (mirror mango's cfg/vra/mango_time)
  taint_time          REAL,
  unsupported_reason  TEXT,
  stderr_excerpt      TEXT,
  started_at          TEXT,
  UNIQUE(sha256, tool, analyzer_version)
);

-- One row per reported source->sink finding, normalized across tools.
CREATE TABLE IF NOT EXISTS finding (
  id                  INTEGER PRIMARY KEY,
  run_id              INTEGER NOT NULL REFERENCES run(id),
  sha256              TEXT NOT NULL REFERENCES binary(sha256),
  sink_func           TEXT,            -- normalized callee name: system|popen|execve|...
  sink_callsite       INTEGER,         -- call instruction address (the match discriminator)
  sink_site_kind      TEXT,            -- "address" | "line"  (provenance of sink_callsite)
  sink_argpos         INTEGER,         -- 0-based tainted arg
  source_class        TEXT,            -- NETWORK|NVRAM|ENV|ARGV|FILE|CONST|UNKNOWN
  source_sites        TEXT,            -- JSON array of source addresses/labels
  reach_from_main     INTEGER,         -- 0/1/NULL
  sanitized           INTEGER,         -- 0/1/NULL
  confidence          REAL,            -- normalized [0,1] (mango rank; cta has none -> NULL)
  raw_path            TEXT             -- tool-specific full trace, verbatim, for triage
);
CREATE INDEX IF NOT EXISTS finding_join ON finding(sha256, sink_func, sink_callsite);

-- Unified ground truth from all oracles. provenance + tier model the fact that
-- no single tool is authoritative.
CREATE TABLE IF NOT EXISTS ground_truth (
  id                  INTEGER PRIMARY KEY,
  sha256              TEXT REFERENCES binary(sha256),
  sink_func           TEXT,
  sink_callsite       INTEGER,
  addr_known          INTEGER,         -- 0/1: match on address vs func-only
  source_class        TEXT,
  provenance          TEXT,            -- mango|karonte|satc|cve|manual
  tier                TEXT,            -- gold (>=2 tools / CVE / manual) | silver (single-tool)
  cve                 TEXT,
  note                TEXT
);

-- A human verdict on a finding (or an FN recorded against ground_truth).
CREATE TABLE IF NOT EXISTS label (
  id                  INTEGER PRIMARY KEY,
  finding_id          INTEGER REFERENCES finding(id),   -- NULL for an FN
  gt_id               INTEGER REFERENCES ground_truth(id),
  label               TEXT NOT NULL,   -- TP|FP|FN|unknown|cta_advantage|path_divergence
  analyst             TEXT,
  evidence            TEXT,
  analyzer_version    TEXT,
  ts                  TEXT
);

-- A confirmed analyzer defect, with its reducer + localization.
CREATE TABLE IF NOT EXISTS bug (
  id                  INTEGER PRIMARY KEY,
  title               TEXT,
  category            TEXT,            -- taxonomy primary (missing_sink, aliasing, ...)
  secondary_category  TEXT,
  layer               TEXT,            -- lifting|cfg|callgraph|pointer|taint|value|sink
  repro_path          TEXT,            -- committed C/pcode fixture
  discriminating_probe TEXT,           -- the toggle that flipped the verdict
  status              TEXT,            -- open|fixed|wontfix
  fixed_in_version    TEXT,
  regression_test     TEXT
);

-- Which real findings a bug explains (drives prioritization by breadth).
CREATE TABLE IF NOT EXISTS bug_instance (
  bug_id              INTEGER NOT NULL REFERENCES bug(id),
  finding_id          INTEGER REFERENCES finding(id),
  sha256              TEXT
);

-- Convenience view: latest run status counts per tool+version.
CREATE VIEW IF NOT EXISTS run_status_summary AS
  SELECT tool, analyzer_version, status, COUNT(*) AS n
  FROM run GROUP BY tool, analyzer_version, status;
