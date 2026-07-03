# `locals` prefix-sharing (trie) storage — prototype status & resume notes

**Branch:** `path-and-closure-queries`
**Goal:** Cut `ctadl index` peak memory so 1–4 MB firmware binaries analyze without the
~50 GB RAM pain threshold, **with aliasing enabled** (correctness) and **byte-identical
results**. Memory is measured by macOS `phys_footprint` peak (NOT RSS — cold pages compress).

Status: **prototype complete, correctness-verified, measured on 5 benchmarks.** Delivers a
consistent **~2–3× peak-memory reduction**. Next step is *compact nodes* to push toward the
modeled ~4×. This doc is the resume point.

---

## 1. The problem being optimized

`locals(FunctionId, FlowVariable, Path, FormalIndex, Path)` is the dominant memory consumer
of the index phase. As a normal Ascent relation it is stored **~6× over**:

- the physical `Vec`, plus
- indices `none`, `0_1`, `0_1_2`, the full existence index `0_1_2_3_4`, and the inverse `0_3_4`.

Every index stores its **value columns inline**, so the full 5-column tuple is replicated
many times. Column widths: `FunctionId`=u32, `FlowVariable`=u64 (tagged), `Path`=8-byte
`tailshare::Seq` handle, `FormalIndex`=i16.

## 2. What the prototype does

A single Ascent **BYODS** ("bring your own data structure") provider replaces all six copies
with **one shared store** (`ind_common`); every logical index becomes a lightweight *view*:

- **forward** `(F,V) → P → {(M,Fp)}` — serves `none`, `0_1`, `0_1_2`, existence, iteration.
  The `(F,V)` and `P` prefixes are stored **once** and shared across all leaves (this is
  where the savings come from).
- **inverse** `(F,M,Fp) → [(V,P)]` — serves the `0_3_4` view. Push-only (each full tuple is
  unique on insert).

The physical `locals` is a `CountingVec` that stores **no tuples** but tracks the row count,
so `prog.locals.len()` (the only post-run consumer of the physical relation, `mod.rs:1167`)
still reports the true size with **zero change** to `mod.rs` call sites.

### Key correctness subtlety (the one that matters)

Ascent's generated per-iteration code calls `merge_delta_to_total_new_to_delta` on **both**
the `ind_common` **and** each index's write target. The reference BYODS impl (`eqrel`)
tolerates this double-merge of the shared store only because union-find merge is
**idempotent**. `locals` is a *plain* relation — a double merge would corrupt semi-naive
evaluation. Fix:

- the **only** real merge lives on the `ind_common` (`LocalsIndCommon::absorb`);
- **all** index write targets (`FullWrite`, `NoopWrite`) have **no-op** `RelIndexMerge`;
- data enters the store solely via the full index's `insert_if_not_present`; partial-index
  `index_insert` is a no-op.

---

## 3. Baselines and improvements

Peak `phys_footprint`, `ctadl index`, aliasing enabled. `baseline` = default Ascent storage;
`+trie` = this prototype. `locals` row counts are **byte-identical** between the two.

| binary            | baseline | + trie   | reduction     | `locals` rows (identical) |
|-------------------|---------:|---------:|---------------|--------------------------:|
| smbd              | 27.49 GB | **9.13 GB**  | **3.01×** (−67%) | 130,139,075 |
| wpa_supplicant    |  9.03 GB | **3.52 GB**  | **2.57×** (−61%) |  35,779,394 |
| pluto             | 25.68 GB | **10.57 GB** | **2.43×** (−59%) | 194,565,083 |
| samba_multicall   | 10.01 GB | **4.31 GB**  | **2.32×** (−57%) |  41,608,096 |
| amuled            |  8.61 GB | **4.35 GB**  | **1.98×** (−50%) |  55,368,800 |

- Wall time is neutral (±6%; pluto 232 s → 246 s).
- The win tracks the **prefix-sharing ratio**: smbd (highest sharing) gains most at 3.0×,
  amuled (lowest) least at 2.0× — consistent with the model.
- The two former worst cases (smbd, pluto) now sit under 11 GB.

## 4. Correctness verification

- **Nix regression suite (the authoritative check):** `nix build .#checks.aarch64-darwin.regression`
  → **33 passed, 0 failed** (ArrayFlow, FieldSensitivity, ObjectSensitivity, funcptr, all
  JVM/DEX reader cases). The README notes the suite is only reliable through Nix (pins
  compilers/Ghidra), so this is the signal that counts.
- **Unit tests:** 103/103 ctadl-ascent lib tests pass (cover the alias and func-ptr flows).
- **Row counts:** identical to baseline on all 5 benchmarks (see table).

### ⚠️ Gotcha that already bit us once — Nix only sees git-tracked files

The Nix flake build copies **only git-known files** into the sandbox. When `locals_trie.rs`
was untracked (`??`), `nix build` failed with `error[E0583]: file not found for module
locals_trie` and 52 cascading errors — even though local `cargo build` succeeded (it reads
straight off disk). **Fix:** `git add ctadl-ascent/src/index_engine/locals_trie.rs`. Any new
file must be `git add`ed before it exists for Nix. (This is exactly why the README says the
regression suite is only reliable through Nix.)

## 5. Files touched

| file | change | tracked? |
|------|--------|----------|
| `ctadl-ascent/src/index_engine/locals_trie.rs` | **new**, ~700 lines — the provider | staged (`A`) |
| `ctadl-ascent/src/index_engine/mod.rs` | `pub mod locals_trie;` (~L55); `#[ds(crate::index_engine::locals_trie)]` on the `locals` relation (~L823) | modified (`M`) |

`mod.rs:1167` (`final_locals: prog.locals.len()`) is **unchanged** — `CountingVec` keeps it correct.
`results.sarif` in the repo root is untracked scratch output; leave it alone.

### Anatomy of `locals_trie.rs` (for orientation)

- `CountingVec<T>` — physical `rel!` type; stores nothing, counts `push`es.
- `DynIter<'a,T>` — clone-able boxed iterator (local copy of byods' private `IteratorFromDyn`).
- `LocalsIndCommon<F,V,P,M,Fp>` — the shared store (`fwd`, `inv`, `len`); `insert`, `contains`,
  `absorb`; impls `Default`/`Clone`/`RelIndexMerge` (the one real merge).
- `NoopWrite`, `FullWrite` — index write targets (no-op merge on both; `FullWrite` does the real inserts).
- `ViewNone/View01/View012/View034/ViewFull` — read views; `RelIndexRead`/`RelIndexReadAll`
  (+ `RelFullIndexRead` on `ViewFull`).
- `marker!` macro → `ToNone/To01/To012/To034/ToFull` (`ToRelIndex` markers).
- 5 provider macros at the bottom: `rel!`→`CountingVec`, `rel_ind_common!`→`LocalsIndCommon`,
  `rel_full_ind!`→`ToFull`, `rel_ind!` dispatched by subset (`[]`,`[0,1]`,`[0,1,2]`,`[0,3,4]`),
  `rel_codegen!`→nothing.

---

## 6. Path forward: from prototype to compact nodes

**Why 2.5× and not the modeled ~4×:** the prototype uses nested `hashbrown` maps with
`HashSet` leaves. That means (a) real per-entry hash-table overhead (control bytes + load-factor
slack, often ~1.5–2×), (b) a min allocation per inner map / per leaf set even when it holds one
element, and (c) the inverse `inv` map is stored in **full** (every `(V,P)` duplicated from the
forward side). The forward-map savings are real but hashbrown's constant factors eat part of it.

The interface (the 5 views + merge contract) does **not** change; only the internal storage of
`LocalsIndCommon` changes. Work items, roughly in priority order:

1. **Sorted-vector leaves instead of `HashSet<(M,Fp)>`.**
   Replace each leaf `Set<(M,Fp)>` with a `SmallVec<[(M,Fp); N]>` kept **sorted**. Most leaves
   are tiny (1–few entries), so inline storage + binary search removes the per-set allocation
   and the hash-table slack. `insert` = binary-search-then-insert returning "was new"; iteration
   is already ordered. This is the single biggest expected win.

2. **Pack `(M,Fp)`.** `FormalIndex`=i16 + `Path`=8-byte handle. Store as a packed 16-byte (or
   `(i16, u64)` `#[repr(C)]`) element rather than a tuple in a hash set, so leaves are dense
   arrays with no padding waste and cache-friendly scans.

3. **Sorted inner P-map instead of `Map<P, …>`.**
   `P` is an 8-byte handle. Replace the inner `Map<P, leaf>` with a sorted
   `Vec<(P, leaf)>` (or a small B-tree-ish structure). Same rationale: most `(F,V)` groups have
   few distinct `P`s, and hashbrown's per-inner-map overhead is paid once per `(F,V)` group,
   which there are many of.

4. **Shrink/eliminate the inverse `inv`.** Options, cheapest first:
   - keep `inv` but store `(V,P)` packed and `shrink_to_fit` after the fixpoint; or
   - make `0_3_4` a **derived** view that scans `fwd` lazily (drop `inv` entirely) — trades the
     `0_3_4` `index_get` from O(1) to O(scan); only worth it if `0_3_4` lookups are rare. Check
     how hot `0_3_4` is before committing (grep the datalog rules that key on `(F,M,Fp)`).
   - or build `inv` **once** at end-of-fixpoint if it's only read after saturation.

5. **Only-if-needed: interned `(F,V)` keys.** If the top-level `Map<(F,V), …>` itself is heavy,
   intern `(F,V)` to a u32 id and key an outer `Vec` by it. Likely unnecessary; measure first.

### How to validate each step

Do them **one at a time**, and after each:

1. `cargo test -p ctadl-ascent` (fast correctness signal, 103 tests).
2. `nix build .#checks.aarch64-darwin.regression` → must stay **33 passed** (remember to
   `git add` any new file first).
3. Re-measure peak `phys_footprint` on **smbd** (highest sharing, biggest absolute baseline)
   and **amuled** (lowest sharing, worst case) — those two bracket the range. Confirm `locals`
   row count still matches the table above (byte-identical results gate).

Target: ~4× reduction / smbd into the ~6–7 GB range, amuled near ~2.2 GB, all row counts
unchanged.

---

## 7. Resume checklist / commands

```bash
# where things stand
git status --short          # expect: A locals_trie.rs, M mod.rs, ?? results.sarif
git -C . diff --staged --stat

# fast correctness
cargo test -p ctadl-ascent

# authoritative correctness (pins toolchain; ONLY reliable check per README)
nix build .#checks.aarch64-darwin.regression -L   # expect "33 passed, 0 skipped, 0 failed"

# memory measurement: build release, run `ctadl index`, poll phys_footprint peak
#   (see the measure-process-memory skill: use `footprint -p PID -f bytes`, peak, poll ~4s)
#   scratchpad had run_new.sh / sweep.sh drivers using ./target/release/ctadl with
#   XDG_STATE_HOME pointed at a scratch store and cached imports.
```

**Not yet done (needs a decision):** commit the two changes; then start compact nodes at
§6.1. The prototype is not committed — only staged/modified in the working tree.
