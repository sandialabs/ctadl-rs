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
* **Source `kind` carries the source class.** CTADL reports a flow whenever any/clear
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

### ✅ Resolved: argv/offset taint propagates again (saturating sources)

The argv/offset-taint regression first seen on **cta@beb327a** (recall collapsed
to 4.2%) is **fixed**. Recall now *beats* the old last-good baseline on both
precision and recall:

| Build | TP | FN | extra | recall | note |
|---|---|---|---|---|---|
| `beb327a` | 1 | 23 | 9 | 4.2% | regressed (argv taint broken) |
| `b06b137` | 20 | 4 | 12 | 83.3% | old last-good baseline |
| `593ce9a` | 21 | 3 | 4 | 87.5% | first build with the fix |
| **`4b7e767`** | **21** | **3** | **4** | **87.5%** | current — **no regression** (findings byte-identical to `593ce9a`) |

The fix is three parts (full write-up in `../EVALSESSION.md`):

1. **`saturating: true` on the argv sources** (`cmdi-firmware.json5`, the `main`
   source generator). `argv[i]` is `*(argv+8i)`, a sibling offset of the modeled
   `.deref` path; saturation taints the whole subtree so `argv[i]` reconnects to
   the source. Recovers the direct `system(argv[1])` cases.
2. **Base-level string-builder propagation** (`sprintf`/`strcpy`/`memcpy`
   families) — added `{ input: "Argument(n)", output: "Argument(0).deref" }`
   alongside the `.deref → .deref` edges, so pointer-level argv taint crosses
   into the destination string (recovers argv-through-a-builder and the
   `off_shoot` `read → alter_command → system` flow).
3. **Harness — SARIF call-site extraction** (`normalize_ctadl._callsite_addrs`):
   substring-match `call-arg(` and prefer the real `call … in <sink>` forwarding
   site over the degenerate PLT-thunk twin. Corrects scoring (dropped `extra`
   20→4 with zero TP loss).

Verify the argv recovery on a single binary:

```sh
ctadl go -n nested -l pcode --models firmware-eval/models/cmdi-firmware.json5 \
    /Users/.../operation-mango-public/package/tests/binaries/nested/program
jq '[.runs[0].results[]|select(.ruleId=="C0001.tainted-path")
     |select(.properties.taintLabels|index("argv_input"))]|length' results.sarif  # -> 2
```

<details>
<summary>Original engine-side diagnosis (pre-fix, cta@beb327a) — kept for history</summary>

Root cause — **offset-qualified (indexed) taint stopped crossing a base-pointer
copy.** On `nested` (`system(argv[1])`), the frontend spills `argv` (`formal(1)`)
to a stack local and reads the element as `local(@p1_0).[8].deref`; the index
graph shows the copy edge `formal(1) ↔ @p1_0` on the *bare base*. At b06b137 an
offset-0 source view (`Argument(1).deref`) covered every `argv[i]` read via
offset-insensitive loads; on beb327a the source and the `argv[i]` load sit in
**disconnected components** of the taint graph — forward+backward taint no longer
meet with the source label (the debug `C0002` node carries only the sink's
`command_injection` label, never `argv_input`), so no `C0001` path forms. The
`saturating` source primitive (added in ctadl `a836dbe`, "Add saturating sources
(#70)") is what closed this gap model-side.
</details>

### Corpus status (Operation Mango tests, cta@2afaf7d6 — current build)

Re-validated after rebasing the branch onto ctadl `main` @ `92d6a996`. Run over
all 15 Operation Mango test binaries (`operation-mango-public`), scored against
24 Mango known bugs with `--addr-tolerance 0`:

```
23 TP / 1 FN / 4 extra   (recall 95.8%)   15/15 binaries run OK   27 findings
```

Every one of the 27 findings carries a concrete sink call-site address, so the
23 TP are **exact** address matches at tolerance 0.

| Build | TP | FN | extra | recall | note |
|---|---|---|---|---|---|
| `b06b137` | 20 | 4 | 12 | 83.3% | old last-good baseline |
| `4b7e767` | 21 | 3 | 4 | 87.5% | previous documented status |
| `aef39ab` | 23 | 1 | 5 | 95.8% | ⚠ all 28 findings lacked addresses — matched by `sink_func` only |
| **`2afaf7d6`** | **23** | **1** | **4** | **95.8%** | current, post-rebase — exact address matching |

The one remaining FN is a genuine CTADL analysis gap, not a harness artifact.
Diagnose by dumping the index/taint graphs (`--dump-index-graph` /
`--dump-taint-graph`) and the debug SARIF profile (`--sarif-profile debug`,
which exposes `C0002.tainted-instruction` = where forward+backward taint *meet*,
`C0003.taint-source`, `C0004.taint-sink`):

| Binary | FN | Symptom | Root cause |
|--------|----|---------|-----------|
| `layered`  | 1 | 1 of 2 system call sites found | second call site missed |

(`off_shoot`, `execve`, and `nvram` were in this list until, respectively,
base-level string-builder propagation — item 2 of the ✅ Resolved section above —
wildcard sink ports — the **Resolved: wildcard sink ports** section below — and
the rebase onto `main` landed; all three now report.)

**`nvram` — resolved by the rebase.** `nvram/program` is still an unlinked
`ET_REL` object (`file` reports "relocatable"; `system`/`acosNvramConfig_get`
were unresolved `R_X86_64_PLT32` relocs), and the Ghidra-pcode frontend used to
bind **0 sinks** on it, so no flow could form. On the current `main` both call
sites now bind and match Mango's ground truth exactly (`system@4194349` and
`system@4194380`, both `src=NVRAM`). No harness or model change was needed.

The 4 extras are in `early_resolve` (`53a6…`, 2: a NETWORK and a FILE
source→`system`) and `multi_input` (`e2f7…`, 2: two FILE source→`system`) —
extra source-classes into genuine multi-channel sinks that Mango's GT lists once
(both binaries are built with several input channels), likely `cta_advantage`
rather than FP; needs triage to confirm.

### Resolved: benign warnings misclassified every run as `unsupported`

`bench.py::classify()` substring-scanned the whole of stderr for
`"unsupported"` (and `"unimplemented"`, …) *before* looking at the exit code.
The current pcode frontend prints `warning: CHA: unsupported virtual method
table` on every import, so after the rebase all 15 binaries were classified
`unsupported` with `findings=0` — CTADL had actually succeeded, but `run_one`
only parses the SARIF when `status == "ok"`, so every finding was silently
dropped and the corpus scored 0.

Fixed in `classify()`: exit 0 is `ok` unconditionally, and the
`_UNSUPPORTED_MARKERS` scan runs only on a failed run with `warning:` lines
stripped out. A genuinely unsupported input still exits non-zero with
`Error: … unrecognized filename extension`, so it is still classified
`unsupported`.

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
