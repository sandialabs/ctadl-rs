# firmware-eval - DO-NOT-MERGE

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
same binary address space, so the sink call-site address is the discriminator.
`sink_func` refines it and is the fallback when an address is missing on one
side.

Two address details make the join exact (`--addr-tolerance 0`):

1. **Base rebasing.** Ghidra loads PIE ELFs at `0x100000` and non-PIE at
   `0x400000`; Mango/angr loads *every* binary at `0x400000`. CTADL's SARIF
   `address` object carries a base-independent `relativeAddress` (RVA);
   `normalize_ctadl` rebases it onto the angr base (`ANGR_LOAD_BASE`) so PIE and
   non-PIE alike land in Mango's space.
2. **Call-site vs callee entry.** CTADL anchors each result's top-level
   `location` at the sink *callee's* entry (its PLT thunk on these ELFs) — that
   is hundreds of bytes off from Mango, which reports the `call` instruction.
   The real call site is inside the `codeFlows`: the step whose message begins
   `call-arg(...)` is the call that passes tainted data into the sink.
   `normalize_ctadl._callsite_addrs` takes the last such step per threadFlow
   (one result can carry several codeFlows → several distinct call sites, which
   Mango counts as separate bugs) and emits one finding each. With both fixes the
   addresses match Mango exactly.

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

### ⚠️ Regression on cta@beb327a — argv/offset taint no longer propagates

Re-running all 15 binaries on the newer engine build **cta@beb327a**
(`--addr-tolerance 32`) collapses recall:

```
1 TP / 23 FN / 9 extra    (recall 4.2%)    15/15 binaries run OK   <- cta@beb327a
20 TP / 4 FN / 12 extra   (recall 83.3%)   15/15 binaries run OK   <- cta@b06b137 (last good)
```

**This is an engine regression, not a model-config gap — no change to
`cmdi-firmware.json5` recovers it.** Every surviving finding is ENV/FILE/NETWORK
(single-level `char*` sources: `getenv`/`read`/`recv` → `Return.deref` /
`Argument(1).deref`); **all 24 known bugs sourced from `argv` are missed**, and
the offset-driven multi-input flows also thinned (`53a6…` 9→4, `e2f7…` 6→4).

Root cause — **offset-qualified (indexed) taint stopped crossing a base-pointer
copy.** On `nested` (`system(argv[1])`), the frontend spills `argv` (`formal(1)`)
to a stack local and reads the element as `local(@p1_0).[8].deref`; the index
graph shows the copy edge `formal(1) ↔ @p1_0` on the *bare base*. At b06b137 an
offset-0 source view (`Argument(1).deref`) covered every `argv[i]` read via
offset-insensitive loads; on beb327a the source and the `argv[i]` load sit in
**disconnected components** of the taint graph — forward+backward taint no longer
meet with the source label (the debug `C0002` node carries only the sink's
`command_injection` label, never `argv_input`), so no `C0001` path forms.

Model fixes attempted and **ruled out** (all still 0 `C0001` paths on `nested`,
and 1 TP / 23 FN / 9 extra across the corpus — byte-identical to the shipped
model):

* Bare-base source `Argument(1)` (hoping base taint flows field-insensitively
  through the spill copy) — no.
* Explicit element offsets `Argument(1).[8·i].deref` / `.deref.deref` for
  `i = 0…7`, in both pointer and byte forms (17 source ports total) — no.
* Sink-side `wildcard` (already default `true`) is irrelevant: the sink is seeded
  correctly; the break is on the **source→sink forward propagation** across the
  spill, and `wildcard` is source-rejected (see `docs/model-generators.md` §7),
  so there is no source-side offset/wildcard primitive to reach for.

This is the same base↔offset deref class the `Taint from base into offset derefs`
(`e629a64`) and the `Move IR to Load & Store Instructions` (#53) /
`Compose paths from loads and stores` (#62) engine changes touch — the bridging
is complete enough for the over-approx meet on some paths but not for precise
`C0001` reconstruction through an argv element load. **The fix is engine-side**
(offset-insensitive base↔element deref on the C0001 path); the model is already
maximal. Reproduce with:

```sh
ctadl go -n probe -l pcode --models firmware-eval/models/cmdi-firmware.json5 \
    --dump-taint-graph /tmp/t.txt \
    .../operation-mango-public/package/tests/binaries/nested/program
# -> 4 isolated nodes, no edges: argv source never reaches the system call-arg
```

### Corpus status (Operation Mango tests, cta@b06b137 — last good baseline)

Run over all 15 Operation Mango test binaries (`operation-mango-public`), scored
against 24 Mango known bugs with `--addr-tolerance 0`:

```
20 TP / 4 FN / 12 extra   (recall 83.3%)   15/15 binaries run OK
```

The remaining 4 FN are genuine CTADL analysis gaps, not harness artifacts.
Diagnosed by dumping the index/taint graphs (`--dump-index-graph` /
`--dump-taint-graph`) and the debug SARIF profile (`--sarif-profile debug`,
which exposes `C0002.tainted-instruction` = where forward+backward taint *meet*,
`C0003.taint-source`, `C0004.taint-sink`):

| Binary | Symptom | Root cause |
|--------|---------|-----------|
| `off_shoot`| sink+source labels **meet**, no `C0001` path | base↔offset deref gap (below) |
| `nvram`    | **0 sinks matched** | unlinked ET_REL object (below) |
| `layered`  | 1 of 2 system call sites found | second call site missed |

(`execve` was in this list until wildcard sink ports landed — see **Resolved:
wildcard sink ports** below.)

**`off_shoot` — incomplete base↔offset deref reconciliation on the precise
path.** The model's source/propagation/sink ports are at offset-0
(`Argument(n).deref`), but the real taint lands at a *nonzero* stack offset:
source `file_input` (from `read(0, extras, …)`) is at `call-arg(…,1).[-88].deref`
(the `extras` stack buffer), while the flow is blocked upstream at the
auto-derived `alter_command` summary. Forward and backward taint
over-approximately **meet** (the debug profile emits a `C0002` node carrying both
labels), but the precise `C0001.tainted-path` query can't cross the base↔offset
boundary, so no flow is reported. This is the same base↔offset class the
`Normalize offset 0 away` / `Taint from base into offset derefs` commits address:
the bridging is complete enough for the over-approx meet but not yet for precise
path reconstruction. Unlike `execve` (which was a *sink*-matching gap, now fixed
by wildcard ports), this block is at an intra-procedural summary, so the sink
wildcard does not reach it — the fix is engine-side (offset-insensitive
base↔element deref on the C0001 path).

**`nvram` — unlinked ELF relocatable object.** `nvram/program` is `ET_REL`
(`file` reports "relocatable", `main` at 0x0, `system`/`acosNvramConfig_get` are
unresolved `R_X86_64_PLT32` relocs) — the Makefile's generic rule compiles
`program.c` without linking `nvram_lib.c`. angr/Mango load object files and
apply relocations; the Ghidra-pcode frontend does not, so the `call system`
sites (`e8 00000000`, reloc unapplied) never bind to the external `system`
function. Result: the calls appear only under `function(main)\ncall-arg(...)`
with no callee resolution (contrast `execve`, which has
`function(execve)\ncall-arg(47,...)`), so **0 sinks match** and no flow can form.
Irrelevant to real firmware (linked images); to exercise this test either link
it (`gcc program.c nvram_lib.c`) or teach the Ghidra import to apply ET_REL
relocations.

The 12 extras are all in `early_resolve` (4) and `multi_input` (8) — extra
NETWORK/FILE/ENV source→system flows Mango's GT doesn't list (`multi_input` is
built with several input channels), likely `cta_advantage` rather than FP;
needs triage to confirm.

### Resolved: wildcard sink ports (recovers `execve`)

Sink ports now carry a boolean `wildcard` property (**default `true`**): a sink
port matches any concrete access-path **extension** of the port — any path that
has the port path as a prefix. So `Argument(1)` matches `Argument(1).deref`,
`Argument(1).[12].deref`, etc.; it does *not* match unrelated paths. On sink
instantiation the port is expanded against the program's path universe into the
concrete matching vertices, one seeded endpoint each (sinks seed backward taint
and the formatter resolves endpoints by exact path equality, so the wildcard
can't be left abstract). `wildcard` is sink-only (rejected on sources /
propagations); see the model schema `source-sink-model.wildcard`.

This recovered `execve`: the argv element `args[2] = argv[1]` reaches the sink at
`Argument(1).[-40].deref` (the offset sits *before* the deref, so it is not a
suffix of `.deref`). Anchoring the array-form sink at the bare `Argument(1)` +
wildcard makes that element an extension, so the flow is flagged — while
`system`/`popen`/`execl` keep their `.deref` string ports (anchoring at the bare
pointer there only inflated FPs: it took `extra` 12→20 with no recall gain).
Net effect: recall 79.2%→83.3% (19→20 TP), `extra` unchanged at 12.

### Resolved: sink call-site address in CTADL SARIF

Pcode encodes the sink instruction address in the SARIF `address` object, but
the result's top-level `location` points at the callee PLT entry, not the call
site. Resolved in `normalize_ctadl` by extracting the call site from the
`codeFlows` `call-arg` steps and rebasing onto the angr base — see **Matching**
above. This is what took corpus recall from a broken 4.2% to 79.2%.
