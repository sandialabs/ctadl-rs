# Native index blowups that are not the stack - DO-NOT-MERGE

What is left over budget on native code after P1 (`problem/p1-stack-slots`). That branch fixed the
stack frame as a mixing pool: S3's promotion (`window:8`), S29's frame updates (`--frame update`,
the default) and the frontend fix that stopped writing taken frame addresses into the frame. They
brought `androidudpbus` and `ntfs-3g` under budget and changed no finding on 49 binaries and the
regression suite. Everything below survives them. In almost every case the evidence says the stack
is not the cause: few stack accesses stay in memory, `--mem2reg full` hardly moves the rows, or the
largest relations are the context ones rather than `locals`.

The budget is 100x the imported IR throughout, as on P1. Peaks are from `time -l` on macOS
(arm64). P1's `problem.md` (at `7bfe1342`, in #174's history) has the full record of how each
number below was measured; this file keeps only what is still open.

## Starting point

- This branch is `main` at `a617c790` (P1, #174), with only this file added.
- The defaults are P1's: `ctadl index --frame update --mem2reg window:8`. `--frame store` and
  `--mem2reg off|exact|above|full[:N]` are there for comparison.
- **Measurement tooling not on `main`** (see P1's Starting point for the cherry-picks; expect
  conflicts): a patched `ascent_macro` that keeps a timed-out run's running scc (`b47ed139`),
  per-relation and per-index sizes (`5858fa78`), per-rule row counters (`918c771a`) and
  `scripts/blowup-rank.py` (`c1ee3d61`). `main` writes `profile.json`, which has relation counts
  and scc and rule times for a run that reaches the fixpoint.
- **`ctadl index --lsm`** (experimental, #155) moves the large relations to memory-mapped files.
  It changes where the rows live, not how many there are, and has not been tried on any of these.

## Open problems

### 1. The context relations dominate most over-budget firmware

The largest group. In the 45-binary sweep (Benchmarks), 26 binaries are still at least 10x over
budget under the default. In most of them the largest relations are not `locals` alone but the
context-sensitive ones: `edge_split`, `ctx_edge_split` and `context_locals`, together about
twice `locals`. They keep only tens to hundreds of stack accesses in memory under `window:8`, and
`--frame update` moved none of them by more than noise.

Largest relations under the default (`profile.json` from the sweep; all reached the fixpoint):

| Binary | `edge_split` | `locals` | `ctx_edge_split` | `context_locals` | Peak | Against budget |
|---|---|---|---|---|---|---|
| D-Link DSP-W115 `bin/GpioForCrond` | 85.7 M | 74.9 M | 52.8 M | 43.2 M | 27.30 GiB | 104x |
| D-Link DSP-W115 `bin/testI2C-r-20ms` | 57.6 M | 69.5 M | 29.3 M | 31.6 M | 23.65 GiB | 94x |
| D-Link DCS-8200LH `usr/sbin/mxcam` | 54.1 M | 44.9 M | 35.1 M | 39.7 M | 22.14 GiB | 58x |
| D-Link DCS-8200LH `usr/sbin/iperf` | 40.1 M | 22.7 M | 29.9 M | 18.4 M | 16.99 GiB | 41x |
| Netgear LG6100D `sbin/upnpd` | 69.2 M | 65.4 M | 31.6 M | 28.2 M | 21.81 GiB | 39x |
| Netgear WNDR3700v5 `usr/sbin/KC_PRINT` | 32.4 M | 37.9 M | 19.7 M | 24.9 M | 13.87 GiB | 38x |
| Netgear WNDR3700v5 `usr/sbin/KC_BONJOUR` | 28.9 M | 34.1 M | 16.7 M | 21.2 M | 12.58 GiB | 36x |
| Netgear MVBR1210C `usr/sbin/sdkmips` | 13.4 M | 7.6 M | 7.2 M | 5.0 M | 4.28 GiB | 16x |

- **`captival_usockc` and `sdkmips` were looked at more closely** (before P1's frontend fix): the
  `locals` and `context_locals` joins on `edge_split` take the most rule time in both.
- **`GpioForCrond`'s query also times out** at 600 s (P1's sweep).
- **Not yet checked:** which functions hold the rows, and whether it is a few hubs (as `inflate`
  and `FUN_00144f04` were for P1) or spread out. The `skip-analysis` bisection in How to
  reproduce is the first step.

### 2. Wild relations next to `locals`: `edge_split_wild` and `assign_like`

A second shape: `edge_split` leads with `edge_split_wild` and `assign_like` next, and the context
relations are small. That is P6 ("Wild relations", `things-to-improve.md` on
`improve-native-handling`: the offset-keyed relations of the wildcard match), so far seen in
`a2050` and `conscryptprovider`.

| Binary | `edge_split` | `locals` | `edge_split_wild` | `assign_like` | Peak | Against budget |
|---|---|---|---|---|---|---|
| Netgear R7000 bitdefender `bin/bdupdater` | 105.0 M | 37.5 M | 32.3 M | 26.6 M | 13.08 GiB | 30x |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/ipsWeb` | 38.0 M | 26.6 M | 11.9 M | 10.5 M | 4.56 GiB | 22x |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/evLua` | 25.9 M | 19.2 M | 8.2 M | 7.0 M | 3.18 GiB | 22x |

`ipsWeb` keeps 3 stack accesses in memory and `evLua` none, so the stack is out for them.

### 3. `locals` alone: loops that carry pointers, and offsets that pile up

`uclited` (TP-Link TL-WPA8630P) is the studied case: 3.6x over budget under the default (40.7 M
`locals`, 0.67 GiB). zlib's `inflate` holds 98% of `locals` even with every slot promoted
(`--mem2reg full`: 11.8 M rows, still 2.9x over). P1's Analysis took it apart; its causes 2 and 3
are what remain:

- **The state-machine loop is one cycle through most of the function.** With every slot promoted,
  `inflate`'s `assign_like` graph (variables only) has one SCC of 4,941 of its 8,313 variables,
  and 79% are downstream of it. Loop-carried values merge in the head's phis, and a binary
  operator is lowered as a merge of its operands (`x = a op b` becomes `assign x = a, b`), so a
  pointer combined with an integer (a table indexed by the bit buffer) shares paths with it. Of
  364 sources, the median reaches 54% of `inflate`'s 65,750 `(v, p)`.
- **Offsets pile up around the loop.** 242 of the 364 sources are four shapes (`@p0.deref.[k]`,
  `@p0.deref.[k].deref`, `@p0.deref.deref.[k]`, `@p0.deref.deref.[k].deref`) at about 60 offsets
  each, because the input pointer is incremented in the loop (`__src' = __src.[1]`) and each trip
  adds 1 to the source path's offset. The only bound is the program-wide `path_set`. Every
  variant of a shape reaches the same 64,746 `(v, p)`. Collapsing them would cut `locals` about
  3x (estimated, not tried).

Binaries where `locals` leads by far, not yet checked for this shape:

| Binary | `locals` | next | Peak | Against budget | Stack accesses kept |
|---|---|---|---|---|---|
| D-Link DSP-W115 `bin/file` | 130.8 M | `edge_split` 43.6 M | 7.27 GiB | 20x | 1,489 |
| Netgear R7000 `usr/sbin/unzip` | 107.5 M | `edge_split` 33.1 M | 6.43 GiB | 11x | 2,014 |
| Tenda AC15 `usr/bin/nginx` | 318.9 M | `locals_key` 2.6 M | 5.23 GiB | 5.0x | 539 |
| D-Link DAP-2690 `usr/sbin/captival_usockc` | 22.9 M | `edge_split` 12.1 M | 3.84 GiB | 20x | 258 |
| Netgear MBM621 `bin/chat` | 13.3 M | `edge_split` 5.7 M | 0.93 GiB | 8.5x | 80 |
| D-Link DAP-2310B `sbin/wifitool` | 32.7 M | | 2.66 GiB | 15x | 92 |

- **`unzip` and `fsck_hfs` likely link a decompressor**, as `uclited` links zlib; `file` may too.
  Unchecked.
- **`nginx` is the odd one:** 319 M `locals` rows but nothing else above 3 M, and only 5x over.
  Before P1's frontend fix and #170 it reached a 16 GiB cap. `window:8` promotes 77,878 of its
  81,135 stack accesses.
- **`chat`:** `--mem2reg full` moves nothing, and any promotion adds rows (`off` has 30% fewer
  than `window:8`, measured before P1's fix).
- **`wifitool`:** `full` cut it 24% before P1's fix (`FUN_0040d740` and three uClibc functions).

### 4. Two binaries do not finish under 32 GiB

| Binary | IR | Result | Stack accesses kept |
|---|---|---|---|
| D-Link DSP-W115 `bin/hnapserver-combo` | 5.2 MB | killed at the 32 GiB cap, both `--frame` settings | 1,242 |
| TP-Link Archer MR200 `usr/bin/adb` | 6.1 MB | killed at the 32 GiB cap, both `--frame` settings | 484 |

Nothing is known about what grows: a killed run leaves no `profile.json`. Run with
`CTADL_INDEX_TIMEOUT_SECS` well before the cap, with the patched `ascent_macro` so the running scc
is kept.

### 5. `libgdx`: `glxy` and its siblings

`com.kaeruct.glxy` is 1.5x over budget under the default (76.6 M `locals`, 2.12 GiB, 15.1 MB IR).
`FUN_0010b408` holds about 90% of `locals`, but promoting it fully cuts only 16%, so its rows are
not from the stack. P6 is the suspect; unchecked. `retrodrawing` and `halma` carry the same library
(two builds of it), and before P1 all three peaked at 20-23 GB whatever the rest of the app was.

### 6. Apps not measured since P1

Before S3 (pruned SSA on `main`, each APK imported whole, Dex and arm64 libraries):

| App | IR | Peak then | Against budget then |
|---|---|---|---|
| `com.github.msx80.retrodrawing` (`libgdx`) | 15.4 MB | 20.0 GB | 13.0x |
| `com.zoffcc.applications.avifview` | 51.7 MB | 59 GB or more | 11.5x (guard hit) |
| `app.halma` (`libgdx`) | 31.5 MB | 22.6 GB | 7.2x |
| `org.mattvchandler.a2050` | 60.2 MB | 33.9 GB | 5.6x (also P6) |
| `fir.tube` | 27.1 MB | 12.4 GB | 4.6x |
| WhatsApp 2.17.79 with native libraries | | 7.34 GB, 484 s | 186 M `locals`, 90 rows per path |

None has been run with S3, S29 or the frontend fix. Measure them first: some may already be
under budget, and the rest sort into the shapes above.

### 7. Escaped stack slots: sound, but not affordable

Not a blowup at the default, but the gap P1 leaves. `window:8` guesses that an escaped address
reaches 8 slots; it loses nothing on the suite, but nothing bounds it. `--mem2reg above`, which
keeps every slot at or above an escape, is the only policy that cannot lose a flow, and under
`--frame update` it costs what skipping mem2reg cost: `androidudpbus` 266.9 M `locals`, 680 s and
13.77 GiB (5.2x over) against 41.9 M, 44 s and 2.25 GiB under `window:8`. Making it affordable
needs a better bound on what an escaped address reaches than "every slot above it" (S4's escape
counts in P1's Ideas are the starting data: in `androidudpbus`, 1,714 frame addresses passed to
calls, 3,118 stored to memory, 1,291 copied).

### 8. Path concatenation slows `ntfs-3g` with `--mem2reg off`

Since #170 (interning), `ntfs-3g` (D-Link COVR-2600R `sbin/ntfs-3g`) under `--frame store
--mem2reg off` takes 230 s against 147 s (and 223 against 142 on a second pair), with the same
rows. All of it is the formal-side `locals` rule, the `edge_split` join that concatenates
`p4 · rest` through the path set: 83 s to 157 s. The rule is the same on `main`. No default run
shows it, but any shape with many long paths will.

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
  import format bump (P1, "After merging `main`'s interning"), so re-import with
  `CTADL_REUSE_FACTS=1 ctadl import --force` if `index` refuses them.
- The binaries are in the Operation Mango dataset under
  `/Volumes/Shampoo/datasets/operation-mango-public/firmware/`; the four `fw-*` ids are copies in
  `/Volumes/Shampoo/p1-fw/`.

Every binary at 1.5x or more under the default (`--frame update`, `window:8`), worst first.
"Kept" is the stack accesses `window:8` leaves in memory:

| Binary | Id | IR | `locals` | Peak | Against budget | Kept | Problem |
|---|---|---|---|---|---|---|---|
| D-Link DSP-W115 `bin/hnapserver-combo` | `d738ae5e79e14d2e` | 5.2 MB | | killed | ≥ 32 GiB cap | 1,242 | 4 |
| TP-Link Archer MR200 `usr/bin/adb` | `d5f74925e458fe40` | 6.1 MB | | killed | ≥ 32 GiB cap | 484 | 4 |
| D-Link DSP-W115 `bin/GpioForCrond` | `8a43cb2628af359e` | 2.8 MB | 74.9 M | 27.30 GiB | 104x | 367 | 1 |
| D-Link DSP-W115 `bin/testI2C-r-20ms` | `166cf67fa15e58b6` | 2.7 MB | 69.5 M | 23.65 GiB | 94x | 357 | 1 |
| D-Link DCS-8200LH `usr/sbin/mxcam` | `456ff91aceac80d5` | 4.1 MB | 44.9 M | 22.14 GiB | 58x | 380 | 1 |
| D-Link DCS-8200LH `usr/sbin/iperf` | `223ec66c3895aeb3` | 4.4 MB | 22.7 M | 16.99 GiB | 41x | 551 | 1 |
| Netgear LG6100D `sbin/upnpd` | `6e72b8bb6fc3b70b` | 6.0 MB | 65.4 M | 21.81 GiB | 39x | 1,038 | 1 |
| Netgear WNDR3700v5 `usr/sbin/KC_PRINT` | `49771b7a04154cc1` | 3.9 MB | 37.9 M | 13.87 GiB | 38x | 209 | 1 |
| Netgear WNDR3700v5 `usr/sbin/KC_BONJOUR` | `54176f7a0ae96cb0` | 3.8 MB | 34.1 M | 12.58 GiB | 36x | 150 | 1 |
| Netgear R7000 bitdefender `bin/bdupdater` | `b86f194233058e50` | 4.6 MB | 37.5 M | 13.08 GiB | 30x | 415 | 2 |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/ipsWeb` | `9324d6e0552657c5` | 2.2 MB | 26.6 M | 4.56 GiB | 22x | 3 | 2 |
| Netgear WN3500RP `usr/sbin/kcairplay` | `acd465c4f94c9c49` | 4.4 MB | 37.6 M | 9.04 GiB | 22x | 263 | |
| D-Link DIR-140L `usr/bin/iperf.mips` | `7979c6fefe0f7ce6` | 4.1 MB | 15.9 M | 8.28 GiB | 22x | 304 | |
| D-Link DCS-6818 `usr/local/ipsWeb/bin/evLua` | `630864a12f63b09a` | 1.6 MB | 19.2 M | 3.18 GiB | 22x | 0 | 2 |
| D-Link DSP-W115 `bin/file` | `3bd3ad33d39671ac` | 3.9 MB | 130.8 M | 7.27 GiB | 20x | 1,489 | 3 |
| D-Link DAP-2690 `usr/sbin/captival_usockc` | `fw-captival` | 2.1 MB | 22.9 M | 3.84 GiB | 20x | 258 | 3 |
| ASUS RT-AC85U `usr/sbin/fsck_hfs` | `588678379986258b` | 4.5 MB | 47.8 M | 7.95 GiB | 19x | 487 | |
| TP-Link TD-W8970 `usr/bin/iptables` | `f5e27ca6c7116225` | 4.0 MB | 62.0 M | 6.15 GiB | 17x | 1,413 | |
| Netgear MVBR1210C `usr/sbin/sdkmips` | `fw-sdkmips` | 2.8 MB | 7.6 M | 4.28 GiB | 16x | 300 | 1 |
| D-Link DAP-2310B `sbin/wifitool` | `351dcf58c862ed67` | 1.8 MB | 32.7 M | 2.66 GiB | 15x | 92 | 3 |
| D-Link DCS-6045L `bin/alphapd` | `74db1b9a9ec5e33d` | 4.8 MB | 33.4 M | 6.68 GiB | 15x | 392 | |
| Netgear MBR1200 `usr/sbin/aptestmips` | `fcc150f6d9c057cc` | 4.1 MB | 16.1 M | 5.12 GiB | 13x | 1,571 | |
| Netgear R7000 `usr/sbin/unzip` | `ed6f3e813d05fcad` | 6.1 MB | 107.5 M | 6.43 GiB | 11x | 2,014 | 3 |
| Netgear MVBR1210C `usr/sbin/aptestmips` | `c17a666eb5b696fc` | 4.6 MB | 15.9 M | 4.65 GiB | 11x | 1,735 | |
| Netgear MVBR1210C `usr/sbin/SwiRelayAgentmips` | `eaa93781df62d04e` | 2.4 MB | 6.0 M | 2.33 GiB | 10x | 222 | |
| D-Link DIR-868L `mydlink/signalc` | `817d77ed2df42ecc` | 3.3 MB | 13.3 M | 3.07 GiB | 10x | 45 | |
| Netgear MBR1200 `usr/sbin/SwiRelayAgentmips` | `e0e71fc1a02c2b12` | 2.7 MB | 6.9 M | 2.49 GiB | 9.8x | 669 | |
| D-Link DSN-5000 `bin/gzip` | `32a8fd8ec075ee3e` | 2.0 MB | 6.7 M | 1.64 GiB | 9.0x | 2 | |
| Netgear MBM621 `bin/chat` | `fw-chat` | 1.2 MB | 13.3 M | 0.93 GiB | 8.5x | 80 | 3 |
| D-Link DSN-5000 `bin/tar` | `c6c8ca0b29fadb65` | 5.2 MB | 54.7 M | 3.03 GiB | 6.3x | 14 | |
| Tenda AC15 `usr/bin/nginx` | `fw-nginx` | 11.2 MB | 318.9 M | 5.23 GiB | 5.0x | 539 | 3 |
| D-Link DSN-3200 `bin/tar` | `f10a8a500d6fe5d5` | 5.0 MB | 41.3 M | 1.96 GiB | 4.2x | 14 | |
| Linksys EA6350v4 `bin/ntfsdebug` | `120cca2b628a34d5` | 5.5 MB | 12.8 M | 0.92 GiB | 1.8x | 1,188 | |
| Linksys EA6900v2 `bin/ntfslabel` | `d9bd68cc9006e841` | 5.1 MB | 12.1 M | 0.83 GiB | 1.7x | 1,110 | |

Binaries with an empty Problem column have not been sorted: check their `profile.json` in the
sweep's `runs/<id>-update/`.

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

The candidates are the functions `--mem2reg full` names in `notifications.log` (the largest by
stack accesses), or, better, a per-function count of `locals` rows from a relation dump
(`CTADL_DUMP_RELATIONS`, or `CTADL_INDEX_TIMEOUT_SECS=<n> CTADL_INDEX_DUMP=<dir>` for a run that
does not finish).

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

## Ideas, not explored

- **Collapse offsets that reach the same thing** (problem 3): the source-path variants that differ
  only in a loop-incremented offset. About 3x on `inflate`, estimated.
- **Stop lowering a binary operator as a merge of its operands** where one side is an integer, so a
  pointer indexed by a counter does not share the counter's paths (problem 3).
- **A field-insensitive collapse for the few hub functions** that hold most of a relation, behind
  a size threshold: bounded precision loss for bounded rows.
- **Summarize a hub coarsely and skip its body**, as S6 (`skip-exclusive-callees`, on
  `improve-native-handling`) does for libc++abi's demangler.
- **Model `memcpy` and similar copies precisely** (offset ranges rather than whole-slot mixing).
- **For the context relations** (problem 1): nothing yet. First find whether the rows are a few
  hubs or spread out, and whether `ctx_edge_split` and `context_locals` grow with the same
  functions as `edge_split` and `locals`.
