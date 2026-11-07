# Next steps for `rework-hybrid-inlining` - DO-NOT-MERGE

State of the branch: the work in `docs/hybrid-inlining-rework.md` is committed (`1d9eee34`);
the work in `docs/decision-sets.md` -- the call-string lattice removed, contexts keyed by
decision *sets*, and the `collapse` mode -- is complete, tested and uncommitted on top of it.
Items 1 and 2 below are done and kept for the record; 3-6 stand, with the commit plan in 6
updated.

## 1. Bound `--hybrid-context decision` so it can become the default -- DONE

Done by moving the decision from the row key to a set-valued lattice column
(`decision-sets.md`): `context_locals` has one row per `(f, v, p, a, p4)` whatever the number
of decisions, `xbot_android_samp` converges (435 s, 6.3 GB, where per-decision keying died at
125 GB), and `decision` is the default. The bound needed neither demand, nor a ⊤ threshold,
nor the pair cap: hash-consed sets hold 6 billion row-decision memberships in 292 distinct
sets.

What remains of this item is speed on a hub: `decision` re-closes a function once per wave of
decisions that reach it (fourteen times on `interpretLoop`), and `collapse` -- the flat
lattice `⊥ < {d} < ⊤` -- is the parameter-free abstraction that makes it one. If the
435 s matters, flip the default; the TaintBench verdicts are identical under both.

## 2. Decide what to do about the call-string lattice's incompleteness -- DONE

The lattice is removed rather than documented: `CallString`, `SmallestCallString`, the push
and pop rules and `--hybrid-context call-string` are gone. The result no longer depends on
iteration order; `smbd` gets back the 8,398 summaries the lattice dropped (277,816,678
`locals`, the count the old `decision` mode computed), at the old default's time and memory.

## 3. Constant factors on inputs that already converged

Converging inputs pay 10–20% time and 15–60% memory for the split relations
(`ath_dev` 8.4 → 9.8 s, 0.68 → 1.12 GB; modelled rcs 32.5 → 36.7 s, 3.5 → 4.1 GB). In order
of expected return:

1. **Drop `reach_vp`.** Derive `locals_key` / `locals_key_wild` / `locals_wild` straight from
   `locals` using the precomputed `PathSet::splits`. Costs three dedup probes per `locals`
   row instead of one hash insert, saves one relation (~100 MB on rcs) and one semi-naive
   iteration per new-path hop (iterations went 1957 → 4269 on rcs). Measure both ways; it
   may be a wash on time.
2. **Move the wild relations into the stores.** `locals_key_wild`, `edge_split_wild`,
   `assign_wild`, `locals_wild` are plain Ascent relations (three copies each). They have
   five columns and fit `locals_trie`'s shape; `assign_wild` needs its `i64` folded into a
   tuple column as `edge_split` does.
3. **Per-iteration overhead** turned out to be small (≈1 ms), so do not chase the iteration
   count for its own sake.
4. **Path bound** (catalog idea 3a). `paths` is still the flat program set plus one level of
   model×program concatenation. The scratchpad's chain closure with the redefinition gate
   gives a vocabulary of hundreds where ours is tens of thousands. That is a precision knob,
   not an optimization, so it needs the TaintBench and regression gates, and a diff of
   `summary.parquet` on rcs/smbd to see what it removes.

## 4. Firmware targets still out of reach

`pluto` does not converge in any mode: 60 GB after 31 min (`call-string`), 80 GB after 44
min (`none`), against the old engine's flat 10 GB plateau — it is producing, and the answer
is too large. `ath_hal`, `wpa_supplicant`, `lk_latest` were not run. Before trying them:

- Run `pluto` capped (`CTADL_INDEX_TIMEOUT_SECS=600`) with `RUST_LOG=warn,ctadl=debug` and
  read `relation increase: summary` and the `locals store estimate` line: if a handful of
  functions summarize to tens of thousands of rows, it is the unwinder story again and the
  answer is a `skip-analysis` model, not an engine change. The query in the plateau note
  (`select f.name, count(*) ... group by 1 order by n desc`) finds them from a capped index.
- Only then consider engine-side bounds (summary size cap, plateau note item 5).

## 5. Contextual flows that need two decisions at once are dropped

A `DecisionSet` is a **disjunction**: a row holding `{d1, d2}` holds under either. That is
exact for everything the rules do derive -- unioning where two flows meet is sound, and rule
3.2's `ds.contains(d)` admits precisely the callers that established a member -- but it cannot
say `d1 AND d2`, and a flow that holds only when *two* decisions of the same function hold at
once is not approximated, it is dropped.

The rules never compose two contextual hops. 3.3a extends `context_locals` by `ext_dst` /
`edge_split` / `ext_fml`, all built from context-free `assign_like`; 3.3b extends the
contextual edges (`ctx_ext_dst`, `ctx_edge_split`, `ctx_ext_fml`) by context-free `locals`.
Nothing joins `context_assign` with `context_locals`. This is not new to decision sets: the
same join shapes are in `1d9eee34` under call strings (read off the rules; that mode is gone,
so it was not measured), so the call-string engine could *represent* the situation and still
could not derive it.

Repro -- `decision` and `collapse` both miss it, `none` finds it:

```
def copyA(x): 1  where summaries [return <- x]  { start: return x; }
def copyB(x): 1  where summaries [return <- x]  { start: return x; }

def f(cbA, cbB, z): 1
{ start:
  y = cbA(z);        # decision d = [arg0 = copyA]:  y <- z
  x = cbB(y);        # decision e = [arg1 = copyB]:  x <- y
  return x; }

def bar(obj): 1
where summaries [return <- obj]
{ start: tmp = f(ptr<copyA>, ptr<copyB>, obj); return tmp; }
```

`bar` passes both targets, so `return <- obj` holds; under `decision` the check reports it
absent (2 passed, 5 failed; `none` gives 7 passed, 0 failed). `RUST_LOG=warn,ctadl=trace` says
where it stops:

```
Context Assign (2):
  {[arg0 = ptr<0>]} f: call-arg(16,-1) = call-arg(16,0)    # y <- z, under d
  {[arg1 = ptr<1>]} f: call-arg(18,-1) = call-arg(18,0)    # x <- y, under e
Context Locals (4):   all under {[arg0 = ptr<0>]}
  ... f: call-arg(18,0) from arg2                          # y reached from z, under d
Context Summary (0)
```

The `d`-conditioned flow reaches `y`, which is the second call's argument; continuing it needs
the `e`-conditioned edge to compose with a `d`-conditioned local, so `x` never gets a row, `f`
gets no summary, and nothing reaches `bar`. `none` finds it because it drops the seeds in as
plain `assign_like`, which compose; a cloning k-CFA finds it at k=1 for the same reason -- the
context already fixes both targets, so there is no condition left to track. The failure is
about the shape of the condition, not the depth of the context. It is also the opposite kind
of error from `collapse`'s: a missed flow, not a spurious one.

Two harness notes. Do **not** drop this fixture into `ctadl-ascent/tests/tnt/`: `flowy_tests`
walks that directory and runs every `.tnt`, so it would fail the suite until this is fixed.
And `flowy::check` hardcodes `IndexConfig::default()` (`codegen/flowy.rs:370`), so
`CTADL_HYBRID_CONTEXT` does not reach the `flowy` example or `cargo test` -- the `.tnt`
fixtures are only ever checked under `decision` today. Wiring the variable in there is a
two-line change and a prerequisite for covering `collapse` in the suite.

Before spending anything on a fix, size it. The shape to count is a function with two or more
critical sites where one site's result reaches another site's argument -- a query over
`callee_info` and `locals` on an existing index, no engine change needed. TaintBench and
`smbd` will say whether this is a curiosity or a real source of missed flows. Only then
consider a fix, and note that the obvious one is wrong: writing `{d, e}` on the row claims the
flow under `d` alone, and would hand it to a caller that passes `copyA` with some other `cbB`.
A correct key is a *conjunction* of decisions -- products of decisions per row, which is the
unboundedness of item 1 in a new place -- so any fix needs its bound before it needs rules.

### Measured (2026-09-10)

Two counts, on every TaintBench app, `smbd` and the modelled `rcs`, all under the default
`decision` mode:

- **Shape**, from the index alone (`callee_info`, `actual_param`, `assign`, `formal_param`;
  variable-level closure over `assign`, globals channel excluded): functions with two or more
  critical sites whose receivers come from a formal, where one site's return/out vertex reaches
  another site's argument.
- **Dropped compositions**, exact, from a post-fixpoint pass the engine now logs at `debug`
  (`dropped_compositions` in `index_engine/mod.rs`, after the `context_locals by function`
  histogram): `context_locals` rows sitting at the source vertex of a `context_assign` edge with
  no context-free `locals` twin, i.e. the compositions 3.3a/3.3b never derive. Split by whether
  the row's set and the edge's set share a decision. Exact-split matches only, so a lower bound
  for the offset/wild cases.

| app | functions | sites in shape (functions) | resolvents | `context_locals` | dropped rows | shared | disjoint | vertices (functions) | summaries only under `none` |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| xbot | 11,012 | 1,249 (179) | 2,868 | 9,147,332 | **26,457** | 26,141 | **316** | 521 (59) | 109,297 |
| smbd | 6,398 | 89 (16) | 38 | 75,032 | **2,640** | 2,640 | 0 | 174 (9) | 1,626,850 |
| vibleaker | 33,524 | 1,007 (208) | 446 | 10,364 | 524 | 524 | 0 | 252 (12) | 1,660 |
| scipiex | 3,876 | 70 (29) | 57 | 1,376 | 376 | 376 | 0 | 106 (5) | 184 |
| fakeplay | 6,984 | 166 (55) | 76 | 2,685 | 76 | 76 | 0 | 33 (9) | 282 |
| beita | 4,066 | 92 (27) | 75 | 2,299 | 69 | 69 | 0 | 29 (7) | 287 |
| fakedaum | 2,125 | 33 (12) | 29 | 1,649 | 42 | 42 | 0 | 15 (6) | 132 |
| rcs (modelled) | 22,064 | 1,510 (87) | 116 | 3,578 | 16 | 16 | 0 | 16 (9) | 193 |
| save_me | 17,174 | 1,680 (133) | 285 | 3,382 | 12 | 12 | 0 | 6 (2) | 227 |
| cajino_baidu | 18,715 | 351 (124) | 224 | 4,961 | 12 | 12 | 0 | 5 (4) | 156 |
| phospy | 1,872 | 26 (20) | 22 | 38 | 3 | 3 | 0 | 3 (3) | 3 |
| hummingbad | 31,802 | 260 (127) | 372 | 2,919 | 1 | 0 | 1 | 1 (1) | 188 |
| fakemart | 2,470 | 12 (9) | 21 | 177 | 1 | 0 | 1 | 1 (1) | 10 |
| 5 more apps | | ≤ 36 | ≤ 15 | ≤ 496 | 0 | 0 | 0 | 0 | |
| 21 more apps | | ≤ 21 | 0 | 0 | 0 | 0 | 0 | 0 | |

(The `none` column is an upper bound that also contains the caller-merging `none` does by
design; on `smbd` it is all merging -- `tdb_traverse` alone has dozens of callback targets.)

What it says:

- **The shape is everywhere the contextual rows are, but it is not the conjunction case.**
  Of 30,229 dropped rows across the corpus, **318 (1.1 %) need two different decisions**; 316 of
  those are in `xbot`'s `Interpreter.doCallSpecial` / `doAdd`, one each in `hummingbad` and
  `fakemart`. Everything else composes under a decision *both* sides already hold: the row's set
  and the edge's set intersect. That case is not a conjunction at all -- the row holds under each
  `d ∈ D`, the edge under each `e ∈ E`, so the composition holds under every `d ∈ D ∩ E`, and
  `D ∩ E` is a set of the same kind, no larger than either. The fixture in this item (`d` and
  `e` on different formals) is the rare shape; the common one is one decided formal used at two
  sites, or one decided site in a loop whose output feeds its own input
  (`ASCIIUtility.getBytes` / `MimeMultipart.readFully` reading into a buffer, `tdb_traverse`
  calling its callback per record, `CodedInputStream.readMessage`, `tdb`/`prs_pointer`/
  `pass_check`/`pm_process` on `smbd`). Minimal form -- one decision, two sites, and the
  engine still reports `return <- obj` absent (`dropped compositions: rows=1 shared=1`):

  ```
  def copy(x): 1  where summaries [return <- x]  { start: return x; }

  def f(cb, z): 1
  { start:
    y = cb(z);        # decision d = [arg0 = copy]:  y <- z   under {d}
    x = cb(y);        # the same d:                  x <- y   under {d}
    return x; }

  def bar(obj): 1
  where summaries [return <- obj]
  { start: tmp = f(ptr<copy>, obj); return tmp; }
  ```

  Both edges hold under `{d}`; `y` reaches `z` under `{d}` (3.3b); but `x <- y` is a
  contextual edge and `y`'s row is a contextual local, and no rule joins those two, so `x`
  never gets a row. Nothing here needs `d AND e`.
- **A bounded fix covers 99 % of it**: compose a contextual edge with a contextual local under
  `D ∩ E` when that is non-empty. Three rules mirroring 3.3b's `ctx_ext_dst` / `ctx_edge_split`
  / `ctx_ext_fml` with `context_locals` in place of `locals` and `ds.intersection(es)` as the
  set, gated on non-empty. Sets only shrink along a composition, so the row count stays bounded
  by the same context-free `locals` as today; what it costs is one more join family in the
  contextual SCC (`context_locals` rows at a contextual edge source are 155 k of `xbot`'s 9.1 M,
  and 3 k of `smbd`'s 75 k). Under `collapse` ⊤ ∩ X = X, so it is the same rule.
- **The remaining 1 % is the conjunction case** and needs the product key this item describes;
  on this corpus it is confined to Rhino's interpreter and two library methods, and is not
  worth a bound of its own until the intersection rule is in and the count re-measured.
- Whether any dropped row reaches a *summary* is not measured (the `none` diff cannot separate
  it from merging). The intersection rule would answer that exactly: diff `summary.parquet`
  before and after.

Reproduce: `RUST_LOG=warn,ctadl=debug` on any index and read the `dropped compositions` line;
the shape query is `duckdb` over the four parquet tables above (call-arg vertex ids decode as
`insn = arg >> 32`, `formal = (arg >> 12) & 0xffff`).

## 6. Bookkeeping

- `../ct-firmware-eval/BENCHMARKS.md` still records `ath_dev`/`cfg80211`/`smbd` as killed;
  on `99bbed37` they converge in seconds/minutes. Refresh its §2 from
  `docs/hybrid-inlining-rework.md` before anyone measures against it again.
- `ctadl query` on `roidsec` takes 3–5 minutes on an 11 k-edge index with either engine.
  That is the query engine's path search and deserves its own note; it is not an index
  problem.
- Commit the uncommitted work as one piece on top of `1d9eee34`: `decision.rs`, the rule
  changes in `index_engine/mod.rs`, the removal in `facts.rs`, the CLI default, and the two
  notes. The workspace tests, the three regression frontends and the 38-app TaintBench sweep
  (under both `decision` and `collapse`) pass on it.

## How to re-measure

```sh
# the plateau configuration (converges in ~18 min at 52 GB under the default now)
grep -v '"aeabi_unwind_cpp_pr0"' ctadl-ascent/src/models/defaults/native-index.jsonl > /tmp/native-nounwind.jsonl
RUST_LOG=warn,ctadl=debug CTADL_INDEX_TIMEOUT_SECS=1800 \
  ~/.claude/skills/memory-guard/memguard.sh 70 /usr/bin/time -l \
  ./target/release/ctadl index rcs_plateau rcs6 --no-default-models \
    -m ctadl-ascent/src/models/defaults/java-index.jsonl -m /tmp/native-nounwind.jsonl

# xbot, the hub (435 s under `decision`, 135 s under `collapse`)
ctadl import -l apk -n xbot /nix/store/8c19c89z2dslyb5ky5a94h6y4j2mg2ny-xbot_android_samp.apk
RUST_LOG=warn,ctadl=debug \
  ~/.claude/skills/memory-guard/memguard.sh 40 /usr/bin/time -l \
  ./target/release/ctadl index xbot_dec xbot \
    -m ../ct-taintbench/taintbench/apps/xbot_android_samp/model.json [--hybrid-context collapse]

# the suites
cargo test --workspace
cargo xtask regression --frontend c,lua,pcode   # then jvm,dex and jni
(cd ../ct-taintbench && PATH=$PWD/../ct-rework-hybrid-inlining/target/release:$PATH \
   cargo run --release -p xtask -- taintbench --apps-dir taintbench/apps --apk <name>=<apk> ...)
```

Read memory off `peak memory footprint`, never RSS, and diff two indexes as sets
(`docs/debugging.md`), never by row order.
