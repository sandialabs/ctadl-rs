# P28: the globals slot at every call in a large static initializer - DO-NOT-MERGE

Codegen passes the global heap to every call as one pseudo-argument, the globals slot. In a
generated static initializer (`<clinit>`) that sets hundreds of static fields and makes a call or
more per field, every call's globals argument is reached by every one of those fields: `locals`
rows ≈ calls × fields, in one function. It is the one Java blowup found in the second corpus, and
the JVM twin of P1's stack mixing pool on native code.

This branch is off `main`, which has the problem. The measurements are on
`improve-native-handling`; `things-to-improve.md` there has the full record (P28, S34, M11).

## Starting point

No fix for P28 has been tried, so there is nothing to bring in except measurement tooling. The
cascade table below came from `CTADL_ITER_LOG`, and the per-function `locals` breakdown from
`CTADL_LOCALS_CENSUS`; both need the commits below.

Hashes are on `improve-native-handling` as rebased onto `main` `076f6a60`; the subjects are given
too, since a later rebase changes the hashes. Each cherry-pick below was tried on `076f6a60`.

**Optional measurement tooling** (none of it is on `main`):

```sh
git cherry-pick b47ed139   # vendors a patched ascent_macro (a timed-out run keeps the running
                           # scc's contents) and adds CTADL_LOCALS_CENSUS and CTADL_DEAD_PHIS
git cherry-pick 5858fa78   # "Log the size of every relation and index" ([idxsizes])
git cherry-pick 918c771a   # "Count each rule's rows derived and new, and log each iteration"
                           # (per-rule counters, CTADL_ITER_LOG)
git cherry-pick c1ee3d61 f9e5b24a   # scripts/blowup-rank.py, and the Windows memguard/ladder
```

These apply in that order; `f9e5b24a` conflicts only in `things-to-improve.md`, which `main`
does not have (drop it). `b47ed139` is titled "WIP-EXPERIMENT: dead-phi pass, locals
census, SSA/stack census examples"; its experiment hooks are off unless their variables are set.
On Linux, `systemd-run --user --scope -p MemoryMax=<N>G -p MemorySwapMax=0` stands in for
`scripts/memguard.ps1`. `main` already writes `profile.json` (#159), which covers relation counts
and scc/rule times without the patched macro.

## The problem

- **Every call passes the global heap.** Codegen (`ctadl-ascent/src/codegen/mod.rs`, "pass
  globals") gives each call site an argument in the `GLOBALS_INDEX` slot (call-arg index -32768)
  and writes the version after the call back to the globals formal. So the globals argument of a
  call is reached by every static field written before it in the function, and, as measured, after
  it too (see "Not checked").
- **Conversations: jemoji's generated emoji classes.** `EmojiPersonActivity.<clinit>` sets 698
  constants and makes 2,093 calls. Each of the 2,093 `call-arg(site, -32768)` vertices is reached at
  699 paths, `.<EmojiPersonActivity;->X>` for every constant `X`, all from one source,
  `.<EmojiManager;->EMOJI_UNICODE_TO_EMOJI>.[]`: 1.46 M rows in that one function. The 468 jemoji
  functions hold 5.24 M of the app's 7.79 M `locals` rows, and globals call-args hold 5.63 M (72%).
- **The rows arrive late and all at once.** At iteration 74 the main loop is at 3.37 GB and nearly
  settled (every rule under 10 k new rows per iteration); then a five-iteration cascade adds about
  5.16 M rows to each relation in turn:

  | Iteration | Rule with the most rows inserted | Inserted | Memory |
  |---|---|---|---|
  | 74 | (settled: every rule under 10 k) | | 3.37 GB |
  | 75 | `ext_dst <-- locals_key delta, assign_like` | 5,166,402 | 3.5 GB |
  | 76 | `locals <-- ext_dst delta, locals` | 5,161,458 | the 4 GB cap |
  | 77 | `reach_vp <-- locals delta` | 5,161,770 | (6 GB run) |
  | 78 | `locals_key <-- reach_vp delta` | 5,162,349 | |
  | 79 | `ext_dst <-- locals_key delta, assign_like` | 5,162,217 | |
  | 80 | `locals <-- ext_dst delta, locals` | 2,075 | |

  Under a 4 GB cap the run dies in iteration 76; under 6 GB it converges at 4.83 GB, with `reach_vp`
  (586 MB), `locals` (538 MB) and `ext_dst` (11 M rows, 508 MB) its largest relations.
- **OpenKeychain has the same shape, with many sources instead of many paths.**
  `OperationResult$LogType.<clinit>`, an enum of about 520 constants, has 520 globals call-args,
  each reached from about 519 sources (one per constant's field): 271 k rows. With BouncyCastle's
  PQC `Utils.<clinit>` (154 k), globals call-args are 1.08 M of OpenKeychain's 3.98 M `locals` rows
  (27%). It still converges under 4 GB.

**Not checked:** why a call sees fields written *after* it as well as before (every call-arg in
`EmojiPersonActivity.<clinit>` has all 699 paths), and whether the callees (`Emoji.<init>`, list
builders) read globals at all. Both decide which fix is right, so start there.

## Measurements

Second corpus, ten F-Droid apps of 8-14 MB of dex, Dex only, on a machine with 7.9 GB of RAM under
a 4 GB cap (6 GB for the ones that hit it). Conversations was the only blowup of the ten: the other
three over 4 GB were big (K-9) or carried too many call-target tags (DAVx⁵, Amaze), with growth
spread over hundreds of iterations.

| App | Functions | IR | Peak | Main loop | Result under 4 GB |
|---|---|---|---|---|---|
| `eu.siacs.conversations` 2.20.4 | 53,189 | 51.0 MB | 4.83 GB | 245 iterations, 94.8 s | cap, iteration 76 |
| `org.sufficientlysecure.keychain` 6.0.4 | 60,127 | 48.9 MB | 3.42 GB | 395 iterations, 21.2 s | fixpoint |

(The 6 GB runs paged on that machine, so their times are inflated.)

## How to reproduce

On `main`, Dex only:

```sh
cargo build --release
export XDG_STATE_HOME=$PWD/tmp/p28/state
target/release/ctadl import -l apk --no-native-libs --name conv eu.siacs.conversations_4218104.apk
# Under 4 GB the run died in the cascade; it converged at 4.83 GB (on Windows) given room
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=0 \
  env RUST_LOG='warn,ctadl=debug,ctadl_ir::ssa=info' \
  target/release/ctadl index conv 2> tmp/p28/conv.log
```

Linux's cgroup cap counts page cache, so the exact cap that kills it may differ from the Windows
numbers above; what matters is the late cascade, not the GB figure.

What to look for:

- `profile.json` in the index directory (`$XDG_STATE_HOME/ctadl/projects/<name>/index/`): every
  relation's tuple count, each scc's iterations and rule times, and whether the run reached the
  fixpoint or stopped at `CTADL_INDEX_TIMEOUT_SECS`. The same numbers are in the debug log.
- The relation counts (`profile.json`, or the census in the log): `locals` and `ext_dst` around
  8 M and 11 M rows, `reach_vp` the largest plain relation.
- The static initializer: `ctadl inspect conv --dump-ir --function 'EmojiPersonActivity;-><clinit>'`
  shows the 698 `sput`s and 2,093 calls; each call's arguments include the globals slot.
- The cascade itself (rows inserted per rule per iteration) needs `CTADL_ITER_LOG=1` with the
  vendored `ascent_macro` on `improve-native-handling`; a breakdown of `locals` by function needs
  `CTADL_LOCALS_CENSUS=<dir>` (also `b47ed139`; see Starting point). On `main`, a
  `CTADL_INDEX_TIMEOUT_SECS` ladder around the cascade (the run settles after about 74 iterations)
  shows the relation sizes jumping between the rungs' `profile.json` files, though a timed-out run on `main` loses the
  running scc's contents.

OpenKeychain shows the shape without blowing up: same steps, `org.sufficientlysecure.keychain_60400.apk`,
look at `OperationResult$LogType.<clinit>`.

## Ideas for solutions

**S34, not started. Two directions, not yet told apart by a measurement:**

- **Pass the globals slot only where it can matter.** Pass it at a call only when the callee can
  read or write globals: its summary, or its model, says so. The constructors and list builders
  called from a generated `<clinit>` are likely not among them, but that is not checked. Needs the
  callee's globals behavior before codegen, or a two-phase approach (summaries first).
- **Version the global heap per static-field write.** Give the global heap a version per `sput`,
  so a call sees only the fields written before it. That would at least halve the rows (each call
  sees the fields before it, not all of them), and it is P1's mem2reg idea (S3, stack-slot
  promotion) applied to the global heap. It does not by itself stop a call from seeing hundreds of
  earlier fields; combined with the first direction it would.

**Other directions:**

- **Treat the globals slot field-sensitively at calls:** pass to a callee only the static fields
  its body (or its transitive callees) reads, computed from the program before the index.
- **A model for generated tables:** recognize `<clinit>`s that only populate constants (enum
  constructors, emoji tables) and summarize them, as a stopgap.

**How to check a fix:** Conversations' peak (4.83 GB) and the late cascade gone (no iteration
inserting millions of rows after the loop has settled); jemoji's 5.24 M `locals` rows;
OpenKeychain's 1.08 M globals call-arg rows; and no lost flows through static fields in the
regression suite (static-field flows are exactly what the globals slot carries).

## Samples

From F-Droid (`https://f-droid.org/repo/<file>`).

| App | Version | File | SHA-256 | Size |
|---|---|---|---|---|
| `eu.siacs.conversations` | 2.20.4+free | `eu.siacs.conversations_4218104.apk` | `3fae646e33b59b95f049ce9bd828022e2edbb374fa82e517bd782aced71ba980` | 30.7 MB |
| `org.sufficientlysecure.keychain` | 6.0.4 | `org.sufficientlysecure.keychain_60400.apk` | `f88374b9113aa9ddba865258bd989eb3ac2ca1577d92e59d6e84228e080674ce` | 12.0 MB |

- Conversations 2.20.4 is the version measured. F-Droid lists several builds of 2.20.4+free
  (per-ABI splits, different version codes); the measurement's version code was recorded on the
  other machine (`tmp/corpus2/fetch.log`). The jemoji classes are Java code, the same in every
  split.
- OpenKeychain 6.0.4 is the version measured.

```sh
mkdir -p samples && cd samples
for f in eu.siacs.conversations_4218104.apk org.sufficientlysecure.keychain_60400.apk; do
  curl -fLO https://f-droid.org/repo/$f
done
sha256sum *.apk   # compare against the table
```
