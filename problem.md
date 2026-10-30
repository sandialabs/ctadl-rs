# Native index blowups that are not the stack - DO-NOT-MERGE

What is left over budget on native code after P1 (`problem/p1-stack-slots`, #174), which fixed the
stack frame as a mixing pool. The budget is 100x the imported IR. Peaks are `time -l` "peak memory
footprint" on macOS (arm64). Everything here was measured on this branch (`main` at `a617c790`,
defaults `--frame update --mem2reg window:8`) unless marked "estimated" or "sweep". Each agent's
full notes, tables and scripts are under `/Volumes/Shampoo/nb-work/<agent>/` (`p4`, `p1ctx`,
`p2wild`, `p3locals`, `apps`, `verify`).

## What the blowups have in common

Most of the rows are **a few functions' summaries, instantiated at every call site**. In the
binaries checked, 95-99% of `assign_like` is summary instances. Each hub's summary is a near-complete
bipartite block, and every caller inherits it through summary composition. So:

- **The largest function or relation is not the cause.** The cost shows up as `locals` and
  `edge_split` in the callers and in rule time on `locals <-- edge_split`. `edge_split`
  (3.1-4.4x `assign_like`) and `edge_split_wild` (0.9-1.26x) are fixed multiples of `assign_like`
  in all 43 sweep profiles, so they never name a cause.
- **`skip-analysis` on one function is the decisive test.** In every binary below, skipping one
  to three named functions brings it within 1-4x of budget.
- **None is the stack.** `--mem2reg full` moves none of them by more than a few percent once P1's
  frontend fix is in. Earlier readings of `chat`, `wifitool` and `glxy` as stack effects were
  wrong.

## Independent causes

Ordered by how many over-budget binaries they explain.

### 1. A statically linked uClibc allocator writes `$globals` from `malloc`'s size, and every caller inherits it

**Binaries:** 25 of the 34 sweep binaries over 1.5x, including both that ran out of memory. All
link uClibc's `malloc` (dlmalloc).

| Binary | Default (x budget) | `malloc` skipped | Whole allocator skipped |
|---|---|---|---|
| DSP-W115 `hnapserver-combo` | killed at 32 GiB | 2.20 GB, 4.2x | 0.62 GB, 1.2x |
| MR200 `adb` | killed at 32 GiB | 2.05 GB, 3.4x | 0.57 GB, 0.94x |
| DSP-W115 `GpioForCrond` | 27.30 GiB, 104x | 0.90 GiB, 3.4x | 0.13 GB, 0.46x |
| R7000 `bdupdater` | 13.08 GiB, 30x | 0.49 GiB, 1.14x | |
| DSP-W115 `file` | 7.27 GiB, 20x | | 0.41 GB, 1.04x |
| MBM621 `chat` | 0.93 GiB, 8.5x | | 0.07 GB, under |
| LG6100D `upnpd` | 21.81 GiB, 39x | 1.31 GB, 2.17x | 0.47 GB, 0.79x (`malloc` + `realloc`) |
| DCS-6045L `alphapd` | 6.68 GiB, 15x | 1.33 GB, 2.76x | 0.33 GB, 0.68x (`malloc` + `realloc`) |
| MVBR1210C `sdkmips` | 4.28 GiB, 16x | 0.21 GiB, 0.8x | |

- **The other members:** `malloc` skipped alone cuts each of them 5-30x:
  - `testI2C-r-20ms`, `mxcam`, DCS `iperf`, `upnpd`, `KC_PRINT`, `KC_BONJOUR`;
  - `iperf.mips`, both `aptestmips`, both `SwiRelayAgentmips`;
  - `signalc`, `fsck_hfs` (under budget after), `iptables`, `alphapd`, `kcairplay`;
  - `captival_usockc`, `wifitool`.

**Mechanism.**
- **The size becomes a pointer.** `malloc`'s size goes through arithmetic lowered as a merge of
  its operands (`nb = size + 11 & ~7` becomes `assign nb = size, 11`). A bin address is the size
  class with the bin array's absolute address as an offset (`%u = %idx.[4824424]`).
- **The heap state is global.** The allocator's state (`av->top`, bins, fastbins: about 90 words,
  each read-modify-written about 200 times) is `$globals.[addr]`.
- **So the summary is a dense block.** `malloc`'s summary is nearly complete bipartite,
  `$globals.<138 paths> <- p0.<38 paths>` in `hnapserver` (5,230 rows), plus a 2,000-9,700-row
  `$globals <- $globals` block. `free` threads the bin lists into `$globals` too
  (`$globals.[k].deref.deref.deref <- $globals.[k].deref.deref`).
- **Every caller inherits it.** `$globals` is an implicit in/out formal of every function, so the
  block lands in the summary of every transitive caller of `malloc` (201-1,129 functions per
  binary). There it is 60-74% of `summary`, 96% or more of `assign_like` and 23-97% of
  `edge_split`, and it is re-instantiated at every call site. That is why `edge_split` looked
  spread out over 18-167 functions.
- **Inside `malloc` the offsets pile up as in cause 3.** `@p0` sits at about 350 offsets.

**Other details.**
- **stdio.** In `signalc`, `malloc` and stdio form a 22-function call-graph SCC
  (`__malloc_consolidate -> sysconf -> config_read -> fopen -> malloc`). The stdio hub in
  `kcairplay`, `wifitool` and `iptables` is a different cause: 11.
- **Stripped binaries.** `malloc` is recognizable by shape: one parameter, about 5,200
  instructions, about 1,550 stores to `$globals`, and the most call sites among such functions
  (`/Volumes/Shampoo/nb-work/p4/rankalloc.py`). `realloc` carries its own copy of the code
  (8,040 instructions, 2,618 global stores).
- **`realloc` matters as much as `malloc`.**
  - **The linuxthreads rows in `GpioForCrond` and `testI2C` (about 60% of `locals` left after the
    `malloc` skip) are this cause again, through `realloc`.** Skipping `malloc` and `realloc` alone
    puts both under budget, with no `none` needed: 0.70x and 0.69x. Skipping linuxthreads instead
    leaves 1.4x.
  - **How the block gets into linuxthreads.** `on_exit` calls
    `realloc(__exit_function_table, size)` with both arguments from globals, and gets a 36 K-row
    `$globals <- $globals` summary. That climbs `pthread_initialize`, `pthread_create` and
    `sem_post` into `__pthread_manager` (48 K rows). It enters the manager through its one
    indirect call, which resolves to `sem_post`, so inheritance also runs along resolved
    function-pointer edges.
  - **Inside the manager, cause 8 multiplies it.** 18 globals are reloaded and re-stored at about
    112 sites, and the identity temps hold 26% of its 1.06 M rows.
  - **`testI2C` is the same build,** with per-function counts identical to the row.
- **The `upnpd` and `alphapd` residuals were `realloc` too.**
  - **The named hubs are real cause-3 functions,** but they don't set the peak:
    - `upnpd`'s `FUN_0042b28c` is uClibc's `regex_compile`.
    - `alphapd`'s `FUN_00450b80` is MatrixSSL's `matrixSslDecode`.
  - **What sets the peak is `realloc`'s block copied into every caller.** It lands in
    `edge_split`, `assign_like` and the context relations, not in `locals`.
    - Skipping `upnpd`'s hub removes 5.4 M `locals` rows but only 0.10 GB of 1.31 GB.
    - With `realloc` skipped, `alphapd`'s hub still holds 86% of `locals` at 0.68x.
  - **`alphapd`'s `realloc` (`FUN_00443390`) is a full second dlmalloc copy:** 7,990 instructions
    and 2,583 `$globals` stores. 90% of its 1.22 M `summary` rows go to `$globals`.
  - **`upnpd`'s `realloc` is a wrapper** around `malloc`, `free` and `memcpy`. It still carries a
    `$globals` block, plus `memcpy`'s 98 offsets (a source pointer stepped by 32).
  - **In `regex_compile`, those rows are 71% of the hub.** Each of its 30 buffer regrowths moves
    every write pointer onto the new buffer through merged arithmetic.
  - **Skipping `alphapd`'s hub seemed to help** only because its summary carried `realloc`'s block.
- **Not measured:** findings on the skipped indexes. Taint through the allocator's free lists is
  spurious anyway: `malloc`'s return should carry nothing from its size.

### 2. The context relations copy the context-free ones, under a decision on `$globals`

**Binaries:** the 13 binaries that were "problem 1", which all link the allocator of cause 1. This
cause roughly doubles what cause 1 leaves.

- **97-99.9% of the context rows are duplicates.** That holds for `context_locals`,
  `context_assign` and `context_summary`, in 7 dumps. Each row is identical, once its decision set
  is dropped, to a `locals`, `assign_like` or `summary` row. Per function, `context_locals` is
  about `locals` and `ctx_edge_split` about `edge_split`. These relations do not grow
  independently, and the rows are not spread out.
- **The decision.** uClibc's `exit()` calls `(*__exit_cleanup)(status)`, a function pointer
  stored in a global. That makes one decision on the globals pseudo-formal,
  `[arg-32768.[G].deref = ptr<__exit_handler>]`, which reaches the 43-57-function SCC that holds
  `malloc`, stdio, `abort` and `exit`, and its callers. 86-99.9% of `context_locals` holds under
  it. A decision on `$globals` cannot separate callers, since every caller passes the same globals.
- **No subsumption.** Rule 3.1 seeds `context_assign` from `summary(__exit_handler)`, which is
  mostly cause 1's block, and every function in the SCC already holds that block unconditionally.
  Rule 3.3b (`context_locals <-- ctx_edge_split, locals`) copies every context-free row reaching a
  contextual edge without checking that `locals` already holds it. 3.3a closes the copies over the
  same edges, and 3.4 and 3.2 carry them up (`ctadl-index/src/index_engine/mod.rs`, around
  1947-2030). A row that holds unconditionally holds under every decision set, so nothing needs
  the copies.

**Measured.**

| Measurement | Effect |
|---|---|
| `--hybrid-context none` (allocator present) | context relations gone; `locals` +0.2-1.7%; peak 1.7-2.4x lower; time 3-5.5x lower (`GpioForCrond` 2,402 s to 730 s) |
| `malloc` skipped plus `none` | `GpioForCrond` 3.4x to 1.4x; `testI2C` 3.8x to 1.4x; DCS `iperf` 2.7x to 1.1x; `KC_PRINT` 1.8x to 1.0x; `iperf.mips` 1.7x to 0.8x; `SwiRelayAgentmips` 1.2x to 0.7x |
| Skipping only the handler target | `context_locals` 19-31x lower on the six small members |

- **The handler skip is not enough on `GpioForCrond`.** With `malloc` present it is killed at
  16.6 GiB, against 11.95 GiB under `none`: other decisions (stdio cookies, RPC/XDR, 18,219
  resolvents) carry copies too.
- **Precision of `none` is not measured.** No small member has query endpoints.

### 3. Offsets pile up on a loop-incremented pointer (and a loop SCC through merged operands)

**Binaries:** Tenda `nginx` (5.0x), `uclited` (3.6x), and the bulk of `a2050` and `fir.tube`. It
is also inside the hubs of causes 1 and 4.

- **The mechanism.** A pointer incremented in a loop (`p = p.[1]`) adds 1 to the source path's
  offset on each trip. Exact concatenation (`[k]·[1]` becomes `[k+1]`) bounds it only by the
  program-wide `path_set`. Each variant of a source shape reaches the same `(v, p)` set, so the
  rows are variants x reach.
- **`nginx`:** PCRE 8.11's `match()` (`FUN_0005e130`, identified by its shape and strings)
  holds 92% of 319 M `locals`.
  - The offsets of `p0.[k]` run 1..64 contiguous, then every offset in `path_set` up to 832:
    227 variants, most reaching the same 470 K `(v, p)`.
  - Collapsing offset-only variants of formal sources would leave 6.3 M rows, about 50x fewer
    (counted in the dump; not tried in the engine).
  - Skipping `match()`: 25.9 M rows, 0.88 GiB, under budget.
  - It is only 5x over with 319 M rows because nothing else is large: about 16 bytes per row.
- **`uclited`:** zlib's `inflate` holds 99.3% of 40.7 M `locals` (0.66 GiB, 3.55x).
  - About 60 offsets per shape: collapsing gives about 3x (estimated).
  - Here the second mechanism matters too: the state machine is one loop SCC (4,941 of 8,313
    variables), and a binary operator is lowered as a merge, so a table pointer indexed by the bit
    buffer shares the buffer's paths.
  - `--mem2reg full`: 11.8 M rows, 0.29 GiB, 1.55x.
- **`a2050` (1.72x):** 30 libc++ `std::__sort<T>` copies hold 51% of `locals`; skipping them
  cuts 51.0% (1.16x). Cause 12 is the rest. Their sources sit
  at about 370 offsets (`first++`/`last--`, element strides 1-16) in 2 distinct sets: about 84x
  collapse (estimated). `FUN_002a2af4` holds another 19% (not examined).
- **`fir.tube` (1.34x, Rust `argon2`):** the rows are spread over 10 functions. Collapse in the
  top function is about 54x (estimated).
- **Not the uclited shape elsewhere.** In `gzip`, the `tar`s and `unzip` the variables-only SCC is
  91-97% of the hub only through the `$globals` vertex, and 14-52 variables without it. The SCC
  measure must exclude `$globals`.

### 4. A callee's summary relates every offset or field to every other, and every call site copies it

**Binaries:** `libgdx` (`glxy` 1.52x, `retrodrawing` 1.91x, `halma` 1.26x), and libntfs
(`ntfslabel` 1.7x, `ntfsdebug` 1.8x). A small callee mixes all the offsets of its arguments, and
its O(offsets²)-per-argument-pair summary makes every caller mix them too.

- **`libgdx`:** stb_image's load function (`glxy` `FUN_0010b408`, 87.5% of `locals`) calls a
  NEON-vectorized pixel loop (`FUN_0011bc30`, 6 calls).
  - The loop's pointers step by 64 and it touches 32 lanes at `.[-32]..[30]`. Its summary is
    `@pA.[a].deref <- @pB.[b].deref` for every `a, b` in -64..95: 4,027 rows per argument pair.
  - 73-96% of the hub's `assign_like` is callee summary instances.
  - Skipping the callee cuts `locals` 55-67%: `glxy` 1.09x (0.89x with `FUN_0011c900` too),
    `retrodrawing` 1.09x, `halma` 0.82x.
  - The hub's own `img_buffer++` loops add cause 3 on top: about 200 offsets per shape, about 54x
    collapse (estimated).
  - Not the stack: `--mem2reg full` cuts 1% now (16% before P1's fix). Not the wild relations:
    under 1% of the hub's rows.
  - `retrodrawing` and `halma` are one libgdx build relinked; per-function counts match within
    0.1%.
- **libntfs:** `ntfs_attr_pwrite` holds 79% of `locals`. Skipping it puts both under budget
  (12.1 M rows to 1.16 M, 0.16 GB).
  - The mixing comes from two 286-line callees (`FUN_00423ae0`, `FUN_00424028`, 2 calls each).
    Their 64-bit field arithmetic (`a->x + a->y`, compares) is lowered as merges, with no loop.
  - Each summary is `p0.deref.deref.[f].deref <- p0.deref.[g].deref` over about 45 fields:
    9,560 rows.
  - Not a loop SCC (6% without summary edges), not a +1 offset run: the offsets are field
    offsets, stride 4.
- **Fix ideas.** Not lowering integer arithmetic as a merge would remove the libntfs case. The
  offset collapse of cause 3 would shrink `libgdx`'s callee summary. A fix aimed only at the
  caller's loop would not.

### 5. Lua's GC links the running `lua_State` into its own gray list

**Binaries:** D-Link DCS-6818 `evLua` (22x) and `ipsWeb` (22x). They call the external `realloc`,
so cause 1 is not involved.

- **Measured.** Skipping `reallymarkobject` (or `atomic`, `singlestep` or `luaC_step`, its only
  route in) takes `evLua` from 3.36 GiB to 0.16 GiB (1.09x) and `ipsWeb` from 4.74 GiB to
  0.19 GiB (0.93x). It removes 97.5% of the rows, and nothing is left.
- **The summary.** `reallymarkobject(g, o)` has an 11-row summary: `g->gray = o`,
  `o->gclist = g->gray`.
  1. **The self-link.** The heap is flow-insensitive, so this gives `o.gclist <- o`.
  2. **All four offsets.** The switch on `o->tt` is not seen, so `gclist` is at all four types'
     offsets (8, 24, 68, 100).
  3. **The running thread points at itself.** `atomic` marks the running thread
     (`reallymarkobject(g, L)`). Offset 8 of a `lua_State` is `L->top`, so the Lua stack pointer
     appears to point at the `lua_State`.
  4. **Every field reaches every stack slot.** Through Lua's 73-function call-graph SCC, 343
     functions (every API function that takes `L`) carry the same block of about 5,800 rows,
     `L.<24 paths> <- L.<365 paths>`, 67% dense, instantiated at each call site.
- **Evidence.** Steps 1-4 are read from the summaries. Which of the first two matters more is not
  settled: a propagation model of the 11 rows does not behave like the derived summary.
- **Not P6.** This was "wild relations next to `locals`" (P6). `assign_wild` is 0.13-5.6 M here
  (26-31 M in P6's case), and the wild rules take 0.9-6.4% of rule time.

### 6. A self-recursive function feeds its summary back into its own body

**Binary:** `avifview`. It reaches 15.9 GiB at a 3,000 s timeout, 3.3x or more over, still
growing, with no fixpoint.

- **The function.** dav1d's `FUN_001189a8` in `libavif-jni.so` has 95 K IR lines, 49 recovered
  parameters, 568 `$globals` offsets and 8 direct calls to itself.
- **The summary.** It is a cross product into globals, `$globals.[k].deref <- @pN.deref.[k].deref`,
  57 K rows per argument: 1.03 M rows, 67% of `summary` at 300 s. It holds 74% of
  `edge_split_wild`.
- **The loop.** Each self-call instantiates the summary into the body, which grows `locals`, which
  grows the summary.
- **Measured.** Skipping it alone gives the fixpoint in 153 s at 4.30 GiB (0.93x).

### 7. The frontend gives a global's `unique` and `register` temps fake addresses that unrelated globals share

**Binaries:** DSN-5000 `gzip` (9.0x), DSN-5000 and DSN-3200 `tar` (6.3x and 4.2x) and R7000
`unzip` (11x), which have no allocator. It also inflates the cause-1 binaries.

**The bug.** Ghidra puts some `unique` and `register` varnodes (temps holding a global's value)
into a global's high variable. `convert_vnode` (`frontends/ctadl-pcode/src/lib.rs:1353`) keys
every varnode in `global_vnodes` on `vnode_data.address` or `constant_offset`. For those varnodes,
that is a unique-space offset or a register number, not the global's ram address.

- **Fake cells.** In `gzip`, 736 of the 909 distinct `$globals.[A]` are fake (711 unique, 25
  register).
- **Shared by unrelated globals.** `$globals.[76]` stands for 21 different globals, `[4]` for 16;
  `tar`'s `[28]` for 40-42, and `unzip`'s `[32]` for 43. `chat`, `GpioForCrond` and `nginx` show
  the same pattern.
- **The blowup.** `$globals` is one variable inside a function, so callee summaries close
  transitively over the shared cells. These join about 500 global paths into one SCC; with the
  fix the largest has 3. In base `gzip`, 83% of `locals` rows have a fake unique cell as their
  source, 5% a register cell, and 12% a real global.
- **Both of the old leads are this bug.** The "global-to-global phi copies" are MULTIEQUAL and
  COPY outputs that are temps of the same global: 2,050 of `gzip`'s 2,080 touch a fake cell, and
  46 remain with the fix. The "tiny addresses" (`$globals.[44]`) are MIPS register numbers, not
  field offsets whose base was lost.

**Experiment.** An env-gated patch (`CTADL_EXP_GLOBAL_HVAR=addr`, not committed:
`/Volumes/Shampoo/nb-work/r-glob/out/r-glob.patch`, worktree `/Volumes/Shampoo/nb-work/r-glob/src`)
keys each such varnode on the ram address of its high variable's ram member.

| Binary | Before (`locals`, peak, x budget) | With the fix |
|---|---|---|
| `gzip` | 6.66 M, 1.73 GB, 8.8x | 0.13 M, 0.10 GB, **0.55x** |
| DSN-3200 `tar` | 41.3 M, 2.17 GB, 4.3x | 0.21 M, 0.17 GB, **0.34x** |
| DSN-5000 `tar` | 54.7 M, 3.43 GB, 6.6x | 0.24 M, 0.18 GB, **0.34x** |
| `unzip` | 107.5 M, 7.53 GB, 12x | 0.67 M, 0.31 GB, **0.51x** |

- **Sharing is the multiplier, not the extra cells.** Keeping a private fake cell per global
  gives `gzip` 0.30 M rows and 0.15 GB. Fixing only the register cells (2.0x) or only the unique
  ones (1.3x) is not enough.
- **Effect on cause-1 binaries** (allocator present): `chat` 1.00 GB to 0.42 GB,
  `captival_usockc` 4.27 to 2.43 GB, `wifitool` 2.86 to 2.34 GB. The other causes' residual
  numbers would move if this is fixed first. The larger binaries were not measured.
- **Precision.** The fix should gain precision, not lose it:
  - All 2,403 non-identity flows of the private-cell variant are in the fixed build.
  - The 10,691 rows the fix drops go through cells shared by different globals, so they were
    spurious.
  - Findings not compared: no sinks match in `gzip`, and the regression suite was not run.

### 8. Ghidra `INDIRECT` on globals is lowered as a reload and store at every call (a constant factor)

Ghidra puts an `INDIRECT` on every global a function touches at every call site. The frontend
lowers it as `COPY` (`ctadl-pcode/src/lib.rs:951`), which gives
`%t = load $globals.[A].deref; store $globals.[A].deref := %t`. That is an identity, since
`$globals` is already threaded through every call.

- **Size.** `gzip`'s hub has 7,691 of them: half its IR. Before cause 7's fix, the temps held
  35-39% of the cause-7 hubs' `locals`. Whether cause 8 still matters after cause 7's fix is not
  measured: the four binaries are under budget without it.
- **Experiment** (an env-gated frontend patch dropping them, in
  `/Volumes/Shampoo/nb-work/p3locals/src`, not committed):

| Binary | IR | `locals` | Against budget |
|---|---|---|---|
| DSN-3200 `tar` | -19% | 41.3 M to 18.5 M | 4.3x to 2.2x |
| `gzip` | -19% | 6.7 M to 4.0 M | 8.9x to 6.0x |
| `unzip` | -21% | 107.5 M to 60.7 M | 11x to 9.5x |
| `chat` | -9% | 13.3 M to 10.7 M | 8.5x to 6.0x |

- **Findings not compared.**

### 9. Escaped stack slots: sound, but not affordable

Not a blowup at the default, but the gap P1 leaves. `window:8` guesses that an escaped address
reaches 8 slots; nothing bounds that. `--mem2reg above`, the only policy that cannot lose a flow,
costs `androidudpbus` 268.5 M `locals`, 699 s and 14.3 GiB, against 42.5 M, 58 s and 2.26 GiB.

- **Five functions hold 96% of the 226 M extra rows.** In four of them, 90-99% of the rows sit on
  the frame's versions: every write to a kept slot makes a version carrying every kept slot's
  contents. In `FUN_00156cb8` the escape is a stack array indexed by a counter (`&buf[i]`).
- **The lowest escape is at or near the bottom of the frame.** So `above` keeps 90-100% of slots,
  and a bound by escape position cannot help. A size for each escaped address, or versioning kept
  slots apart, could.

### 10. `call_target_assign_like` joins on the function alone (time, not memory)

The rule joins on the function alone and tests the path prefix afterwards: the pattern the `locals`
rules replaced with `edge_split`.

- **`androidudpbus`:** 11.6 of 58 s at the default, 446 of 699 s under `above`.
- **`fir.tube`:** 14.3% of rule time.
- **Elsewhere:** 0-2.6% of rule time on the apps, and absent from the firmware sweep.

### 11. MIPS uClibc's `memcpy` and `memset` store the destination's address into it

**Binaries:** what `kcairplay`, `wifitool` and `iptables` have left after cause 1. It looked like a
uClibc stdio hub (`fputs_unlocked`, `__stdio_fwrite`). Stdio only spreads it: the source is
`memcpy` and `memset`.

**The self-link.** Ghidra lowers the unaligned head store (`swl`/`swr`) as a masked
read-modify-write with the shift `(addr & 3) * 8`. That arithmetic is lowered as a merge, so the
stored word carries the address, and the summary gets `dst.deref <- dst`. This is cause 5's
self-link, made by a libc primitive, from the same merge lowering as cause 1.

**How stdio spreads it into strings.**
1. In `fputs_unlocked`, `strlen` (`p - s`, merged) makes the count point into `s`.
2. In `__stdio_fwrite`, `bufpos + n` makes the `FILE` buffer pointer alias the string.
3. `memcpy(bufpos, s, n)` then self-links the string and the return value.

**The blocks.**
- **They are complete bipartite:** deref chains x string offsets. In `kcairplay`, `puts` has
  `p0 <- p0` at 102 x 610 and `ret <- p0` at 118 x 610.
- **The size follows the program's `path_set`, not the libc.** `kcairplay`'s `__stdio_fwrite`
  block is 83 x 67 against 44 x 30 in `wifitool`.
- **The rows are not on `$globals`** (0.35%), so callers do not inherit them. Each call site
  copies them, even when the return is ignored: stdio sites are about 95% of `wifitool`'s
  `assign_like`.
- **`memset` does the same.** `_ppfs_init`'s `memset(ppfs, 0, 152)` self-links `vfprintf`'s
  state.

**Measured** (the other causes removed first):

| Binary | Skipped | Against budget |
|---|---|---|
| `kcairplay` | `malloc` | 9.7x |
| | `malloc` + `fputs_unlocked` | 5.4x |
| | `malloc` + `__stdio_fwrite` | 2.7x |
| | `malloc` + `memcpy` | 1.93x |
| | `malloc` + `memcpy` + `memset`, with `--hybrid-context none` (cause 2) | **0.97x** |
| `wifitool` | allocator | 5.83x |
| | allocator + `__stdio_fwrite` | 0.77x |
| | allocator + `memcpy` | **0.39x** |
| `iptables` | `malloc` | 2.66x |
| | `malloc` + `memcpy` | **0.95x** |

- **The residuals that looked like cause 3 were this.** `__encode_dotted` is
  `memcpy(dest + used, dotted, strlen(dotted))`; in `kcairplay` its `locals` fall from 2.58 M to
  6 K with `memcpy` skipped. `iptables`' "cause 3" functions (`do_command`, `__encode_dotted`,
  `getopt`) are therefore in doubt. `vfprintf`'s 3.3 M fall to 0.36 M with `memcpy` and `memset`
  skipped, and the 0.36 M left is offsets on the format string (cause 3).
- **It depends on the `memcpy` build.** `signalc` (ARM) has a 149-row `memcpy` with no
  self-link. `GpioForCrond` and `testI2C` (MIPS-II) have self-link-free `memcpy` and `memset`;
  why was not examined.
- **The firmware model declares `memcpy`'s propagation, but its body is analyzed as well.**

### 12. A forwarding thunk turns its body's summary into self-loops, which the wild join multiplies

**Binary:** `a2050`, 0.3x of budget on top of cause 3. It is the one place the wild relations
(P6) cost something.

- **The thunk.** `__emplace_back_slow_path<nlohmann::detail::value_t>@002a5020` is a 12-line
  function that calls the real body (`@001b5f24`) with its own arguments.
- **The body's summary.** It is an offset cross product (cause 3): 33.7 K rows
  `@p0.deref.[a] <- @p0.deref.[b]`, 92 x 346 offsets, from libc++'s backward move loop
  (`p = p.[-16]`). The new buffer pointer `new + (end - begin) * 16` is lowered as a merge, so it
  carries `begin` and `end`.
- **The self-loops.** Instantiated in the thunk, every row becomes a self-loop on one argument
  vertex that is the thunk's own formal. That vertex also has 217 reached paths ending in an
  offset (`.deref.[m]`).
- **The wild join.** The wild rule `locals <-- ext_fml` pairs every self-loop with every offset
  path: 33.8 K x 217 = 7.23 M of the binary's 8.16 M `ext_fml`. That is 1.69 billion derivations
  for 291 K distinct `locals` (99.98% duplicates), 84.9 s and 21.8% of rule time. The body itself
  has 1,284 `ext_fml` rows.
- **Thunks inflate summaries generally.** A thunk's summary is the closure, 96.7 K rows against
  the body's 33.7 K. Across 477 thunk/body pairs, thunks hold 874 K summary rows against 355 K
  for the bodies, 41% of `summary` (`vector<float>::__append` 351x). Only the `value_t` thunk
  also has the offset-ending paths that set off the wild join; why its six siblings lack them was
  not found.

**Measured** (default 9.21 GiB, 1.72x, 407 s):

| Skipped | Peak | Against budget |
|---|---|---|
| the 14 nlohmann `emplace` functions | 7.58 GiB | 1.42x (`ext_fml` 0.92 M; the rule 84.9 s to 6.8 s; `summary` -21%) |
| the 30 `std::__sort` copies (cause 3) | 6.18 GiB | 1.16x (`locals` -51.0%) |
| both | 5.44 GiB | **1.02x** (the savings add) |

- **The `__sort` claim holds.** Its rows are each body's own pile-up: callers don't inherit them,
  and 14 of the 15 instantiations are never called. Each body calls itself through its thunk (a
  cause-6 loop), but skipping only the thunks removes 10.6%.
- **A skip-model pitfall.** `signature_match` `name` matches only the full `name@addr`
  form; an address-free name matches nothing, silently.

## Not yet explained

What is left after the causes above are removed:

| Binary | What is left |
|---|---|
| `hnapserver-combo` | 1.17x with the whole allocator skipped, 0.78x with `none` too. What is left is real context sensitivity, not cause 2: 73% of `context_locals` is under gSOAP callback decisions on `struct soap` function pointers (`http_post_header`, `http_parse`), and only 11% duplicates `locals`. Mild; not pursued. |

The old problem 8 (path concatenation slowing `ntfs-3g` under `--frame store --mem2reg off`) is not
independent. It is the 44x row growth from turning promotion off, which is the stack problem P1
fixed. There, 2.16 billion concatenation probes, 82% of them misses, at about 70 ns each through
linked path segments take 150 s of 237 s. One probe per `(path, rest)` would cut it.

## Ideas, not tried

- **Model the allocator** (cause 1): recognize `malloc`, `realloc`, `calloc` and `free` by name,
  or by shape when stripped, and give them models (a fresh object, no flow from the size)
  instead of their bodies. Measured upper bound: the "whole allocator skipped" column.
- **Let unconditional rows subsume conditional ones** (cause 2), and make no hybrid decision on
  `$globals`. Estimated: the `--hybrid-context none` numbers, with no precision loss.
- **Collapse offset-only variants of a formal source** (causes 3 and 4): about 50x on `nginx`,
  54x on `glxy`'s hub, 84x on `a2050`'s sorts, 3x on `inflate` (all estimated from dumps).
- **Do not lower integer arithmetic as a merge of its operands** (causes 1, 3 and 4): `size`
  would not become a pointer in `malloc`, and libntfs's field sums would not mix fields.
- **Key a global's `unique`/`register` temps on the global's ram address** (cause 7): measured
  8.8x to 0.55x on `gzip`, and every cause-7 binary under budget. Fix it first; it shifts the
  other causes' numbers.
- **Bound a function's `$globals` summary** (causes 1 and 6), for example field-insensitive
  above a threshold, since every transitive caller inherits it.
- **Drop identity `INDIRECT` reloads of globals** (cause 8): measured 1.2-2x. Findings not checked.
- **Instantiate a pure forwarding thunk's callee summary as its own summary** (cause 12), instead
  of closing it over the thunk's formals. Or make the wild join skip self-loop edges.
- **Model Lua's `reallymarkobject`** (cause 5), or see the `o->tt` switch.
- **Use the models of `memcpy`, `memset`, `memmove` and `str*` instead of their bodies** (cause 11).
  The firmware model already declares `memcpy`'s propagation.

## Benchmarks

### The firmware sweep

P1's sweep: 45 binaries, imported with P1's step-3 build (`bdcc187e`) and indexed under both
`--frame` settings with `window:8`, the firmware model
(`<ct-bench>/firmware-eval/models/cmdi-firmware.json5`), a 32 GiB cap and a 3,600 s timeout, six
at a time beside the ct-bench daemon (so times are loose; peaks are per process).

- Everything is in `/Volumes/Shampoo/s29/sweep`: `one.sh` (import, index, query one binary),
  `list.txt` and `list-rest.txt` (id and path), `summary.py` and `summary.txt`, and per run
  `runs/<id>-<frame>/` (`index.log`, `profile.json`, `notifications.log`, `results.sarif`).
- **The imports are kept** in `stores/<id>`, so an index can be re-run without Ghidra:
  `ctadl --store /Volumes/Shampoo/s29/sweep/stores/<id> index bin -m $M`. They predate #168's
  import format bump, but this branch still indexes them. Clone a store before indexing in it
  (`/bin/cp -Rc`, cheap on APFS): the index writes into the store.
- P1's app store `/Volumes/Shampoo/p1-full/store` is import format 9, which this branch refuses.
  `CTADL_REUSE_FACTS=1 ctadl import --force` on a clone re-imports it in seconds, without Ghidra.
- The binaries are in the Operation Mango dataset under
  `/Volumes/Shampoo/datasets/operation-mango-public/firmware/`; the four `fw-*` ids are copies in
  `/Volumes/Shampoo/p1-fw/`.

Every binary at 1.5x or more under the default (`--frame update`, `window:8`), worst first.
"Kept" is the stack accesses `window:8` leaves in memory:

| Binary | Id | IR | `locals` | Peak | Against budget | Kept | Cause |
|---|---|---|---|---|---|---|---|
| D-Link DSP-W115 `bin/hnapserver-combo` | `d738ae5e79e14d2e` | 5.2 MB | | killed | ≥ 32 GiB cap | 1,242 | 1 |
| TP-Link Archer MR200 `usr/bin/adb` | `d5f74925e458fe40` | 6.1 MB | | killed | ≥ 32 GiB cap | 484 | 1 |
| D-Link DSP-W115 `bin/GpioForCrond` | `8a43cb2628af359e` | 2.8 MB | 74.9 M | 27.30 GiB | 104x | 367 | 1, 2 |
| D-Link DSP-W115 `bin/testI2C-r-20ms` | `166cf67fa15e58b6` | 2.7 MB | 69.5 M | 23.65 GiB | 94x | 357 | 1, 2 |
| D-Link DCS-8200LH `usr/sbin/mxcam` | `456ff91aceac80d5` | 4.1 MB | 44.9 M | 22.14 GiB | 58x | 380 | 1, 2 |
| D-Link DCS-8200LH `usr/sbin/iperf` | `223ec66c3895aeb3` | 4.4 MB | 22.7 M | 16.99 GiB | 41x | 551 | 1, 2 |
| Netgear LG6100D `sbin/upnpd` | `6e72b8bb6fc3b70b` | 6.0 MB | 65.4 M | 21.81 GiB | 39x | 1,038 | 1, 2 |
| Netgear WNDR3700v5 `usr/sbin/KC_PRINT` | `49771b7a04154cc1` | 3.9 MB | 37.9 M | 13.87 GiB | 38x | 209 | 1, 2 |
| Netgear WNDR3700v5 `usr/sbin/KC_BONJOUR` | `54176f7a0ae96cb0` | 3.8 MB | 34.1 M | 12.58 GiB | 36x | 150 | 1, 2 |
| Netgear R7000 bitdefender `bin/bdupdater` | `b86f194233058e50` | 4.6 MB | 37.5 M | 13.08 GiB | 30x | 415 | 1 |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/ipsWeb` | `9324d6e0552657c5` | 2.2 MB | 26.6 M | 4.56 GiB | 22x | 3 | 5 |
| Netgear WN3500RP `usr/sbin/kcairplay` | `acd465c4f94c9c49` | 4.4 MB | 37.6 M | 9.04 GiB | 22x | 263 | 1, 2, 11 |
| D-Link DIR-140L `usr/bin/iperf.mips` | `7979c6fefe0f7ce6` | 4.1 MB | 15.9 M | 8.28 GiB | 22x | 304 | 1, 2 |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/evLua` | `630864a12f63b09a` | 1.6 MB | 19.2 M | 3.18 GiB | 22x | 0 | 5 |
| D-Link DSP-W115 `bin/file` | `3bd3ad33d39671ac` | 3.9 MB | 130.8 M | 7.27 GiB | 20x | 1,489 | 1 |
| D-Link DAP-2690 `usr/sbin/captival_usockc` | `fw-captival` | 2.1 MB | 22.9 M | 3.84 GiB | 20x | 258 | 1 |
| ASUS RT-AC85U `usr/sbin/fsck_hfs` | `588678379986258b` | 4.5 MB | 47.8 M | 7.95 GiB | 19x | 487 | 1 |
| TP-Link TD-W8970 `usr/bin/iptables` | `f5e27ca6c7116225` | 4.0 MB | 62.0 M | 6.15 GiB | 17x | 1,413 | 1, 11 |
| Netgear MVBR1210C `usr/sbin/sdkmips` | `fw-sdkmips` | 2.8 MB | 7.6 M | 4.28 GiB | 16x | 300 | 1, 2 |
| D-Link DAP-2310B `sbin/wifitool` | `351dcf58c862ed67` | 1.8 MB | 32.7 M | 2.66 GiB | 15x | 92 | 1, 11 |
| D-Link DCS-6045L `bin/alphapd` | `74db1b9a9ec5e33d` | 4.8 MB | 33.4 M | 6.68 GiB | 15x | 392 | 1 |
| Netgear MBR1200 `usr/sbin/aptestmips` | `fcc150f6d9c057cc` | 4.1 MB | 16.1 M | 5.12 GiB | 13x | 1,571 | 1, 2 |
| Netgear R7000 `usr/sbin/unzip` | `ed6f3e813d05fcad` | 6.1 MB | 107.5 M | 6.43 GiB | 11x | 2,014 | 7, 8 |
| Netgear MVBR1210C `usr/sbin/aptestmips` | `c17a666eb5b696fc` | 4.6 MB | 15.9 M | 4.65 GiB | 11x | 1,735 | 1, 2 |
| Netgear MVBR1210C `usr/sbin/SwiRelayAgentmips` | `eaa93781df62d04e` | 2.4 MB | 6.0 M | 2.33 GiB | 10x | 222 | 1, 2 |
| D-Link DIR-868L `mydlink/signalc` | `817d77ed2df42ecc` | 3.3 MB | 13.3 M | 3.07 GiB | 10x | 45 | 1 |
| Netgear MBR1200 `usr/sbin/SwiRelayAgentmips` | `e0e71fc1a02c2b12` | 2.7 MB | 6.9 M | 2.49 GiB | 9.8x | 669 | 1, 2 |
| D-Link DSN-5000 `bin/gzip` | `32a8fd8ec075ee3e` | 2.0 MB | 6.7 M | 1.64 GiB | 9.0x | 2 | 7, 8 |
| Netgear MBM621 `bin/chat` | `fw-chat` | 1.2 MB | 13.3 M | 0.93 GiB | 8.5x | 80 | 1 |
| D-Link DSN-5000 `bin/tar` | `c6c8ca0b29fadb65` | 5.2 MB | 54.7 M | 3.03 GiB | 6.3x | 14 | 7, 8 |
| Tenda AC15 `usr/bin/nginx` | `fw-nginx` | 11.2 MB | 318.9 M | 5.23 GiB | 5.0x | 539 | 3 |
| D-Link DSN-3200 `bin/tar` | `f10a8a500d6fe5d5` | 5.0 MB | 41.3 M | 1.96 GiB | 4.2x | 14 | 7, 8 |
| Linksys EA6350v4 `bin/ntfsdebug` | `120cca2b628a34d5` | 5.5 MB | 12.8 M | 0.92 GiB | 1.8x | 1,188 | 4 |
| Linksys EA6900v2 `bin/ntfslabel` | `d9bd68cc9006e841` | 5.1 MB | 12.1 M | 0.83 GiB | 1.7x | 1,110 | 4 |

Re-indexing `fw-chat`, `fw-sdkmips` and `gzip` on this branch gives the same `locals` and
`edge_split` and peaks within 1%, so the table still stands. Every row is sorted; "Cause" is the
section above, and the residual each leaves is in "Not yet explained".

### `uclited`

`firmware-sweep` benchmark `40c87ee3d0be0b74`: TP-Link `TL_WPA8630P_EU_UK_V2.1_171117`
`fs/usr/bin/uclited` in the Operation Mango `large_dataset` (MIPS32 big-endian, stripped, zlib
linked in), SHA-256 `40c87ee3d0be0b7429e9c4bf2ef067223ef7fa3340ef9d8e24dee7f2699e7320`, 2.0 MB IR.
The import takes about 18 s, the index 10 s.

### Apps

All from F-Droid (`https://f-droid.org/repo/<file>`):

| App | Version | File | SHA-256 |
|---|---|---|---|
| `com.kaeruct.glxy` | 2.3 | `com.kaeruct.glxy_23.apk` | `05838298c9d70edff22dcc3590c287a36bf9cd62fc73b64beb2bbc24ba281bca` |
| `com.github.msx80.retrodrawing` | 0.0.4 | `com.github.msx80.retrodrawing_4.apk` | `ebe6d46ae45d16ba78b6ea5196d69251caec0b6327da13a6ed518a5cf357f414` |
| `app.halma` | 15.0 | `app.halma_15.apk` | `b994337244e2136b54f96f75038fe70d9562635cce50af6387d906ee048d0ec0` |
| `fir.tube` | 11.0 | `fir.tube_11.apk` | `4e88fc405193043e0b09d9111e1f833c4668d803fba2cabedef048e934598f8b` |
| `com.zoffcc.applications.avifview` | 1.0.4 | `com.zoffcc.applications.avifview_10004.apk` | `f17aafcaf79285557051aa09e380051a0f4a0e10e60fda7b49608f92ed8ed8da` |
| `org.mattvchandler.a2050` | 1.0.10 | `org.mattvchandler.a2050_190010010.apk` | `98a0e75e589c319093db56cf98bfa32d920b9436a9cbe7c30b32dcf7a4a6d284` |
| `org.vi_server.androidudpbus` (under budget; control) | 1.0 | `org.vi_server.androidudpbus_1.apk` | `e6085f344fcd764625a9f4491c0aa6803256d754feb3bf1d0797ad2169cfc87f` |

Not on F-Droid: `com.whatsapp_2.17.79-451676_minAPI14.apk` (29 MB, 4.1 MB `libwhatsapp.so`),
SHA-256 `84444d1d222dcc9b8b7aceccf59bd71038be5e84336d888047705da56cedc00a`.

## How to reproduce

### One firmware binary from the sweep's import

```sh
cargo build --release
M=<ct-bench>/firmware-eval/models/cmdi-firmware.json5
S=/Volumes/Shampoo/s29/sweep/stores/8a43cb2628af359e        # GpioForCrond
/usr/bin/time -l target/release/ctadl --store $S index bin -m $M   # under a memory cap
python3 -c 'import json,sys; r=json.load(open(sys.argv[1]))["relations"];
[print(f"{v:>12,} {k}") for k,v in sorted(r.items(), key=lambda kv:-kv[1])[:10]]' \
  $S/projects/bin/index/profile.json
```

### Find the function that holds the rows

Skip one function's body and see how far the rows fall, as P1 did for `inflate` and
`FUN_0001f97c`:

```sh
cat > tmp/skip.json5 <<'EOF'
{ "model_generators": [ { "find": "methods",
    "where": [{ "constraint": "signature_match", "name": "FUN_0001f97c" }],
    "model": { "modes": ["skip-analysis"] } } ] }
EOF
target/release/ctadl --store $S index bin -m $M -m tmp/skip.json5
target/release/ctadl --store $S inspect bin --dump-ir --function FUN_0001f97c
```

The best candidates come from a per-function count of `summary` rows (with their call sites)
and of `locals` rows, from a relation dump (`CTADL_DUMP_RELATIONS=<dir>`, one `<relation>.tsv`
each, by content). The function holding the most `locals` is often downstream of the hub
(`chat`'s `clean` and `file`'s `regex_compile` call `malloc`), so confirm with a skip.

- **A run that does not finish:** `CTADL_INDEX_TIMEOUT_SECS=<n>` stops it and still writes
  `profile.json` and the dump. On `main`, the running scc's relations (`locals`, `edge_split`,
  `assign_like`, `locals_key`, `ext_dst`) then read 0 and dump empty. The patched `ascent_macro`
  (`b47ed139`) keeps them: a build with it is `/Volumes/Shampoo/cargo-target/nb-p4/release/ctadl`.
- **Find a stripped `malloc`:**
  `python3 -I /Volumes/Shampoo/nb-work/p4/rankalloc.py <(ctadl --store $S inspect bin --dump-ir)`.

### `uclited`

```sh
export GHIDRA_HOME=...                    # Ghidra, JDK 21
target/release/ctadl --store tmp/nb/store import -l pcode -n uclited <path>/uclited
target/release/ctadl --store tmp/nb/store index uclited -m $M
# the hub: skip inflate's body and locals falls ~400x
```

### Apps

Need Ghidra to disassemble the `.so` files, and a memory cap:

```sh
export XDG_STATE_HOME=$PWD/tmp/nb/state
target/release/ctadl import -l apk --name glxy com.kaeruct.glxy_23.apk
~/.claude/skills/memory-guard/memguard.sh 32 /usr/bin/time -l target/release/ctadl index glxy
```

### How to check a fix

Every flow kept: `cargo xtask regression -j 4` (the last full run on P1 passed 250 of 253, the
expected failures being `AndroidIcc:droidbench-activity-communication-2`, `C:defaultmodels` and
`Jvm:ClassIdMergedTryFlow`). Compare findings on the benchmarks by rule, message and logical
location name; the messages' site numbers change with the IR. Then peak against budget, the
largest relations, and the share held by the largest function.
