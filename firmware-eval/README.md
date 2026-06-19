# firmware-eval

Tooling to benchmark CTADL's command-injection taint analysis against
**Operation Mango** on Linux firmware, so we can systematically find bugs,
precision issues, and missing modeling in CTADL.

Two pieces live here so far:

| Path | What it is |
|------|------------|
| `models/cmdi-firmware.json5` | A CTADL taint model that mirrors Mango's command-injection source/sink set, so the two tools are directly comparable. |
| `flake.nix` | A self-contained nix flake that wraps every Mango invocation (`mango`, `env_resolve`, `mango-pipeline`) reproducibly via the pinned upstream container. |
| `harness/` | The benchmark harness: results DB schema, the Mango + CTADL → Finding normalizers, known-bug ground truth + scoring, and the `bench.py` orchestrator (run / compare / score / triage / stats). |

This is a **nested** flake — it deliberately keeps Mango's container/Python
impurity out of the top-level `ctadl-rs` flake. Run its apps with
`nix run ./firmware-eval#<app>`.

---

## The CTADL model (`models/cmdi-firmware.json5`)

It mirrors Operation Mango's modeling so a disagreement between the tools is a
real capability gap, not a config difference:

* **Sinks** — all 32 `COMMAND_INJECTION_SINKS` from Mango's
  `sink_lists.py` (`system`, `popen`, `execve`, vendor wrappers like
  `doSystemCmd`, `pegaSystem`, `SLIBC*`, …).
* **Sources** — Mango's `INPUT_EXTERNAL_FUNCTIONS` plus the NVRAM/env getters:
  `nvram_get` & friends, `getenv`, `recv`/`recvfrom`, `read`/`fread`/`fgets`.
* **Propagation** — the string builders (`sprintf`/`strcpy`/`strcat`/`memcpy`/…)
  that carry tainted config into a command string. These overlap CTADL's
  built-in pcode default model harmlessly (the Datalog engine is set-based).

### Conventions baked in

* **Port index**: Mango's `vulnerable_parameters` is 1-based; CTADL `Argument(n)`
  is 0-based. So Mango `[1]` → `Argument(0)`, `[2]` → `Argument(1)`.
* **`.deref`**: cmdi sinks/sources take `char *`; the string bytes live at the
  dereferenced location, so we taint `Argument(0).deref` — matching CTADL's
  shipped pcode default. (See caveat below.)
* **Source `kind` carries the source class.** CTADL reports a flow whenever any
  source taint reaches any sink — kinds need not match (the shipped default
  cmdi detector itself pairs `user_input` → `command_injection`). We exploit
  that so the normalized finding's source class falls straight out of the report:

  | source `kind` | source class |
  |---------------|--------------|
  | `nvram_input`   | NVRAM |
  | `env_input`     | ENV |
  | `network_input` | NETWORK |
  | `file_input`    | FILE |

  If a future CTADL build ever requires kind-matching and you get **zero**
  flows, collapse every source `kind` to `command_injection`.

* **HTTP `KEY_BEACONS`** (Mango's confidence boost when the getter key is an HTTP
  CGI var) are *not* modeled here — CTADL has no "argument string equals X"
  primitive, so that stays a harness-side ranking step over the resolved key.

### Run CTADL with it

```sh
ctadl go <project> --models firmware-eval/models/cmdi-firmware.json5 <pcode-artifact>
# or split: ctadl index <project> --models ... <artifact> ; ctadl query <project>
```

`--models` can be repeated; load this alongside or instead of the defaults.

### Validation status (checked against the built `target/release/ctadl`)

* ✅ Parses cleanly through CTADL's real JSON5 + port parser (no errors).
* ✅ Matches source/sink functions by name (`Matched 1 sources and 1 sinks` on a
  probe with `nvram_get`/`system`).
* ✅ Reports an end-to-end `nvram_get → sprintf → system` flow when access paths
  align.

> **`.deref` caveat for `.tnt` probes.** On the abstract `.tnt` test language,
> arguments are scalars with no pointer indirection, so `.deref` under-matches
> there (a `.tnt` probe needs a no-`deref` model to show the flow). On real
> Ghidra-pcode firmware, arguments to these functions *are* pointers, so
> `.deref` is correct — which is exactly why the shipped pcode default uses it.
> Re-validate on the first real firmware binary.

---

## The nix wrappers (`flake.nix`)

Mango pins `angr==9.2.94` + native deps + a git-pinned binwalk; building that
under nix is a perpetual yak-shave. The honest, content-addressed reproducibility
anchor is the prebuilt image **digest**, and nix wraps `docker`/`podman run`
around it.

```sh
# one-time: resolve and pin the digest (prints a line to paste into flake.nix)
nix run ./firmware-eval#mango-pin

# then run Mango on one binary, reproducibly:
nix run ./firmware-eval#mango -- ./squashfs-root/usr/sbin/httpd \
    --results out --category cmdi --concise

# whole-corpus pipeline:
nix run ./firmware-eval#mango-pipeline -- --path ./corpus --results out --env --mango --parallel 20

# dev shell with all three wrappers + python + sqlite + jq:
nix develop ./firmware-eval
```

How the wrappers behave:

* Prefer `podman`, fall back to `docker`.
* Mount `$PWD` at the same path inside the container (and `$MANGO_DATA` if you
  export it for a corpus outside the cwd), so absolute paths you pass —
  the target binary and `--results` dir — resolve identically inside.
* Run as your uid (`--user`), so result files are owned by you, not root.
* Override the container entrypoint to call `mango`/`env_resolve`/`mango-pipeline`
  directly (bypassing the kube/local `entrypoint.py` wrapper).

### Pin before you trust a number

`flake.nix` ships `imageDigest = ""`, which falls back to `:latest` and prints a
loud warning — **not reproducible**. Run `mango-pin`, paste the
`imageDigest = "sha256:…";` line, and every machine then runs byte-identical
Mango.

---

---

## The harness (`harness/`)

Stdlib-only Python (runs under the devShell's `python311`, no extra deps).

| File | Role |
|------|------|
| `schema.sql` | SQLite schema: `binary`, `run`, `finding`, `ground_truth`, `label`, `bug`, `bug_instance`. Content-addressed by sha256; tracks findings across analyzer versions. |
| `findings.py` | Shared core: the normalized `Finding`, source-class mapping, DB I/O, and the address-primary `compare()` / `match_rows()` matcher. |
| `normalize_mango.py` | Parse Mango `cmdi_results.json` (`closures[]`) → findings + run status (from the `error` field). |
| `normalize_ctadl.py` | Parse CTADL `results.sarif` (`C0001.tainted-path` results) → findings; source class from `taintLabels`, sink kind from `taintVertices`. |
| `ground_truth.py` | Known-bug ground truth: `from-mango` (snapshot a Mango run) or `ingest` (a dataset file), then `score` ctadl's found/missed/extra. |
| `bench.py` | Orchestrator: `run` CTADL over a work-list (classify ok/crash/timeout/unsupported), `compare` cta-vs-mango (the 2×2), `score` vs known bugs, `triage` (optional `label` bookkeeping), `stats`. |

### One shot

Run ctadl over a work-list, load ground truth, and score — in a single command.
Ground truth comes from a Mango results dir (`--mango-out`) or a dataset file
(`--gt`); either is optional if you've already loaded it.

```sh
# manifest: [{binary, artifact, arch}, ...]
python harness/bench.py eval --db results.db --manifest worklist.json \
    --mango-out mango_out --show 20
# -> runs ctadl, snapshots mango as known bugs, prints found / missed / extra
```

```
cta=cta@<sha> vs 2 known bugs
  found  (TP)   :     1
  missed (FN)   :     1   <- improve recall   (recall=50.0%)
  extra  (FP?)  :     0   <- improve precision / GT incomplete
-- missed (FN): known bugs ctadl didn't report --
  66068b701464 popen@4215244 src=NETWORK [mango]
```

### The pieces, if you want them separately

```sh
# run ctadl only
python harness/bench.py run --db results.db --manifest worklist.json

# get Mango ground truth reproducibly
nix run .#mango-pipeline -- --path ./corpus --results mango_out --env --mango --parallel 20
python harness/normalize_mango.py --db results.db \
    --version "$(nix run .#mango-pin | grep -o 'sha256:[0-9a-f]*')" mango_out
python harness/ground_truth.py from-mango --db results.db
#   ...or a dataset instead:  ground_truth.py ingest --db results.db --origin foo bugs.json

# score / diff
python harness/bench.py score   --db results.db --version cta@<gitsha> --show 20
python harness/bench.py compare  --db results.db \
    --cta-version cta@<gitsha> --mango-version mango@sha256:<digest> --show 20
```

### Ground truth & scoring — the improve-the-tool loop

Ground truth is just a flat set of **known cmdi bugs per binary**. Where it
comes from doesn't matter — snapshot a Mango run (`ground_truth.py from-mango`)
or load a dataset file (`ground_truth.py ingest`, JSON/JSONL/CSV with lenient
field names). Both land in the `ground_truth` table tagged with an `origin`.

Then `bench.py score` (or `ground_truth.py score`) answers the only question
that matters:

```
cta=cta@<sha> vs 3 known bugs
  found  (TP)   :     2
  missed (FN)   :     1   <- improve recall
  extra  (FP?)  :     1   <- improve precision / GT incomplete
```

with `--show N` listing the actual missed bugs (your FN worklist) and extra
findings (FP candidates) so you can go fix them. Matching is address-primary
(`findings.match_rows`); `--addr-tolerance` absorbs any base-offset delta.

`bench.py triage` is **optional** bookkeeping for when you've investigated one
and want to record the verdict so you don't redo it: `fn-seed` stamps an `FN`
label on every missed bug; `set --finding-id <id> --label FP|cta_advantage ...`
records a manual call. `score` alone is enough for the day-to-day loop.

### Matching

Findings join **address-primary** within a binary: both tools reference the
same binary address space, so the sink call-site address is the discriminator
(`--addr-tolerance` absorbs any angr↔ghidra base delta). `sink_func` refines it
and is the fallback when an address is missing on one side.

> CTADL's SARIF now emits the **source and sink function names** directly on
> each `C0001.tainted-path` result (added in
> `ctadl-ascent/src/query_engine/formatter.rs` — the model attaches an endpoint
> to the callee method, so the endpoint's `infunc` resolves to the name):
> - `properties.sinkCallee` / `sinkFunctions` — the sink function(s) (`system`, …)
> - `properties.sourceCallee` / `sourceFunctions` — the source function(s)
>   (`nvram_get`, …), complementing `taintLabels` which carries the source *kind*.
>
> `normalize_ctadl` reads both (sink name → `sink_func`; source names →
> `source_sites`, and as a classification fallback), with the old text-scan kept
> for pre-change SARIF.

### Validation status

End-to-end smoke test passed against the built `target/release/ctadl`: `bench
run` executed CTADL on a probe, classified the run, and normalized the SARIF to
a finding (`source_class=NVRAM`, `sink_func=system` from the new `sinkCallee`
property); a synthetic Mango result ingested with correct source classification
(`http_passwd`→NVRAM, `recv`→NETWORK); `compare` produced a correct 2×2 (1
matched, 1 mango-only FN candidate); `stats` summarized.

### One open item to finalize on the first real pcode binary

**Sink call-site address in CTADL SARIF.** On the `.tnt`/C frontends the sink
location is line/col; pcode encodes the instruction address. The normalizer
captures whatever is present and tags `sink_site_kind` (`address` vs `line`).
Confirm the pcode encoding (likely `region.startLine` or a location property)
and adjust `normalize_ctadl._sink_site` if needed. (The sink *callee name* is no
longer an open item — CTADL now emits it directly; see Matching above.)
