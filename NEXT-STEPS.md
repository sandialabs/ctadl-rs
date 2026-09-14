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
