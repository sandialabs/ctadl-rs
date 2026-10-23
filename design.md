# Design: full stack-slot promotion, behind a flag

## Where we are

- **S3** (`b1b52de8`, mem2reg): promotes a slot unless an escaped frame address reaches it, with
  a fixed guess of how far an escape reaches (`window:8`). Not on `main`; cherry-pick it first.
- **S4** (`aa0b4cda` on `improve-native-handling`): promoted the kept slots too, writing stores
  through to memory. It doubled `androidudpbus`'s `locals` (P24: memory was dropping deep paths,
  and write-through brings them back for every slot). `problem.md` is out of date on this.
- On `uclited`, `window:8` gives 41 M rows and `exact` gives 15 M.

## Proposal

`--mem2reg=full` (off by default; the default stays `window:8`): promote every directly accessed
slot, whatever escapes. Direct stores go only to the local, so memory holds only what callees and
copies of the address write. After a call that is passed an address reaching slot `k` (under
`window:8`), reload the local from the slot: `__stack_mK = load __stack_top.[k].deref`.

**Scope.** With the flag on, a function is promoted fully only when it has more than N direct
stack accesses (`--mem2reg=full:N`, default about 1,000; mem2reg already counts them), and
gets `window:8` otherwise. The functions that hold most of `locals` (`inflate`, about 3.5 k
accesses; `FUN_00144f04`, about 160 k) are far above any function of ordinary size, so the
unsoundness stays confined to them. A model mode overrides the threshold for one function:
`mem2reg-full` forces full promotion, `mem2reg-window` forbids it.

The reload costs little (memory holds only callee writes, not every direct store) and keeps the
common out-parameter case, which is `example.c`:

```c
struct foo x; transfer(&x.b, source()); sink(x.b.d);   // transfer writes out->d; the reload brings it back
```

### What it loses

The callee, or anything else holding the escaped address, can no longer see values the function
stored directly:

- **Inputs passed by pointer:** `s.a = source(); f(&s);`, where `f` reads `s->a`.
- **Stored or copied addresses** (`p = &buf; ...; *p`): reads see only writes through the address, not
  direct stores, and the reverse.

There is no write-back before calls, by choice. It would restore the inputs-by-pointer case, but
memory is flow-insensitive, so a value written back before one call reaches every read of the
slot. That is S4's write-through again, and it would bring back S4's cost (P24). The
notification names each function where this happens (kind `call input`).

### Notifications

The notifications log is high signal and low noise, like `log_once::warn_once!` (already used for
unsupported pcode mnemonics): each fact is stated once, never once per site.

- **One line per function, and only when it matters:** the function was promoted fully *and* some
  promoted slot is reached (under `window:8`) by an escape that full promotion stops modeling:
  a call input, a stored or copied address, a return. Escapes that the reload covers (callee
  writes) are not reported.
- **A fixed message key,** for example `mem2reg full: <function>: frame address escapes (call
  input, stored); flows through those slots may be missed`. The kinds are a sorted set, with no
  counts or sites in the key, so it dedupes like `warn_once!`. Counts and sample sites go in
  the debug log only.
- **Written to `index/notifications.log`** next to `profile.json`, and copied by `ctadl query`
  into SARIF's `toolExecutionNotifications` at warning level. A missing flow can then be traced
  to a function the analysis treated unsoundly, without reading through noise.

## Measurement

- **Baseline:** S3 (`window:8`) cherry-picked onto `main` `076f6a60`, re-measured first, since #150
  and #154 landed after the earlier numbers.
- **Apps:** `uclited`, `androidudpbus` and `glxy`, each under a memory cap. Record peak memory,
  `locals` rows, the share held by the largest function, and the notifications.
- **Keep it if:** `uclited` comes within about 1.5x of `exact`'s 15 M `locals` rows, and
  `androidudpbus` peaks well below S3's 15.8 GB. `glxy` is only reported (its suspect is P6, not the
  stack).
- **Regression suite:** the default run stays as it is. Its functions are below the threshold, so
  it runs `window:8`. A second run with `full:0` promotes every function fully. It only reports
  and does not gate: it lists each lost flow and checks that a notification names that flow's
  function. A lost flow with no notification is a bug in detection.

## Results

Implemented as designed, with two changes the regression run called for (below). Measured on
`main` `076f6a60` with S3 (`b1b52de8`) cherry-picked, on macOS (arm64, 128 GB), one run at a time.
Peaks are `time -l`'s peak memory footprint. Fixpoint times are from runs with the machine to
themselves. The share of `locals` comes from a separate build that adds `b47ed139`'s
`CTADL_LOCALS_CENSUS`, which is not on this branch. `glxy` and `androidudpbus` are the F-Droid
APKs in `problem.md`, imported whole, with the default models.

| App | `--mem2reg` | `locals` | Fixpoint | Peak | Largest function |
|---|---|---|---|---|---|
| `uclited` | `off` | 188.8 M | 35.4 s | 10.89 GiB | `inflate` 99.8% |
| | `window:8` | 41.4 M | 10.2 s | 1.26 GiB | `inflate` 99.3% |
| | `exact` | 14.9 M | 3.7 s | 0.63 GiB | `inflate` 98.1% |
| | `full` | 12.1 M | 3.1 s | 0.54 GiB | `inflate` 97.7% |
| | `full:0` | 11.5 M | 3.0 s | 0.52 GiB | |
| `androidudpbus` | `window:8` | 339.5 M | 187 s | 13.25 GiB | `FUN_00144f04` 71.7% |
| | `full` | 31.6 M | 21 s | 2.26 GiB | `FUN_00156cb8` 35.8% |
| `glxy` | `window:8` | 105.0 M | 45 s | 4.31 GiB | `FUN_0010b408` 90.2% |
| | `full` | 88.3 M | 37 s | 3.78 GiB | `FUN_0010b408` 88.8% |

- **Keep it.** `uclited` comes in below `exact` (`exact` still keeps the slot an address escapes
  from, and that slot takes every direct store). `androidudpbus` falls 5.9x in peak and 10.7x in
  `locals`. Its hub `FUN_00144f04` goes from 243.4 M rows to 0.21 M. The largest function is now
  `FUN_00156cb8` (11.3 M rows), which is below the threshold and still runs `window:8`.
- **`glxy` is not a stack problem.** `FUN_0010b408` is promoted fully and still holds 78 M rows.
- **The baseline moved.** On today's `main`, `window:8` peaks at 13.25 GiB on `androidudpbus`
  (15.8 GB in `problem.md`) and 4.31 GiB on `glxy` (21.5 GB).
- **Scope.** `full` (threshold 1000) promotes 2 functions fully in `uclited`, 27 in `androidudpbus`
  and 3 in `glxy`. Every one of them gets a notification, mostly `(call input, copied, stored)`.
  The query's SARIF carries them as `CTADL0104.index-notification` warnings.

**Regression suite** (`cargo xtask regression --frontend pcode,jni`, which includes the APK
packagings of the JNI cases): the default passes 46 of 46. With `CTADL_MEM2REG=full:0`, the first
version lost one flow, `Jni:JniWide+summary`, and no notification the query could see named it.
Two causes, both fixed:

- **A returned slot.** The pcode frontend returns a value held in a stack slot as the slot's
  address (`keep` ends in `return %__stack_top.[4]`). Promoting that slot dropped `keep`'s
  argument-to-return flow, and with it `Java_JniWide_nativeHigh`'s summary. Full promotion now
  leaves a slot that a returned address reaches in memory, as `window:8` does. None of the
  large functions above returns a frame address, so their numbers do not change.
- **A summary project's notifications.** `keep`'s notification was in the summary project's
  `notifications.log`, but the query runs on the app's index. An index now carries the
  notifications of each summary project it loads, prefixed `summary project '<name>':`.

After the fixes, `full:0` passes 46 of 46, and 7 cases' indexes name a function (`main` or
`handler`).
