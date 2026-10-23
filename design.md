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
