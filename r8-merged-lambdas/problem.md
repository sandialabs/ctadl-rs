# P9 / P24: R8-merged Kotlin lambdas

## The problem

R8 merges many small classes, above all Kotlin lambdas and coroutine `SuspendLambda`s, into one
class: a `final int` field holds a class id set by the constructor, and each method that differed
becomes a `switch` on it, with the captured values in shared `Object` fields.

```java
final class M implements Function0 {          // two lambdas merged: id 0 logs, id 1 uploads
    final int id; final Object l, k;
    M(int id, Object l, Object k) { ... }
    public Object invoke() {
        switch (id) {
            case 0: Log.d("app", ((User) l).getName()); break;
            case 1: ((Server) k).send(((File) l).getBytes()); break;
        }
        return Unit.INSTANCE;
    }
}
```

The analysis cannot see that the id never changes, so each merged method is the union of every
arm: a call to `logName.invoke()` takes on the upload's flows, and the virtual calls inside resolve
against whatever any lambda stored in `l` and `k`. These methods become hubs of false flows. When
first measured (P9), they kept `cpuinfo` from converging (past 1800 s and 41 GB). Splitting the
classes apart at import fixes that, but done naively each split method stores every arm once per
id (P24).

## What this branch changes

All in the dex frontend (`frontends/ctadl-dex/src/merged.rs`, `lib.rs`):

1. **Split merged classes (S10).** A class with a `final int` field that every constructor stores
   from an argument or a constant, and that methods `switch` on, gets one subclass `C$r8id<k>` per
   id, with clones of the switching methods narrowed to arm `k`. Construction sites with a known id
   allocate the subclass; a site with an unknown id keeps allocating `C`.
2. **Prune each clone (S29).** A clone keeps only its own arm (`prune_unreachable_nodes`), so the
   split adds about 12% to the stored program instead of storing every arm per id.
3. **Empty the merged methods of closed classes (S30).** When `C` is `final` and every allocation
   of it builds a clone, no object is ever `C` itself, so its merged methods never run; they are
   lowered with empty bodies. They stay in the dispatch table, so which calls the index resolves by
   class hierarchy does not change. (Leaving them out of the table instead pushed 4-id classes'
   calls from call-target tags to class-hierarchy resolution and added false results.)
4. **Match a `new-instance` with its constructor across a block boundary.** Inside a `try`, a
   `new-instance` ends its basic block; the scan dropped the object there and missed the constant id
   in the next block's constructor call (96 allocations in `cpuinfo`, 229 in NewPipe).
5. **Don't retag a subclass's object built through a merged superclass's constructor.** R8 drops
   a subclass's constructor and calls the superclass's; the scan retagged that object as the
   superclass's clone. A constructor call is now a site only when it initializes an object of its
   own class.

With these, 96-98% of split classes are closed. The index CLI and `xtask` gain a guardrail and two
regression checks, below.

## Results

On `main` (`7c98b73e`) and this branch, each app imported Dex only and indexed under a 32 GB cap;
results from a broad query (10 source families, 9 sink families), compared with instruction ids
masked.

| App | Merged classes (ids) | Tainted instructions | Results gained | Call-target tags | Main loop | Peak |
|---|---|---|---|---|---|---|
| `cpuinfo` 6.6.1 | 620 (3,562) | 20,904 → 617 | 0 | 456 k → 411 k | 337 → 192 it | 1.73 → 1.73 GB |
| NewPipe 0.29.1 | 955 (4,889) | 33,177 → 7,553 | 0 | 1.65 M → 1.62 M | 467 → 467 it | 4.02 → 4.06 GB |
| CENO 2.11.7 | 996 (5,877) | 29,728 → 10,237 | 0 | 1.82 M → 0.78 M | 366 → 215 it | 3.87 → 3.45 GB |
| `com.noto` | 137 (528) | 2,997 → 2,997 | 0 | 756 k → 758 k | 251 → 251 it | 2.19 → 2.19 GB |

Sources are identical everywhere; the few sinks lost are sink endpoints expanded over access paths
that existed only through merged-method edges. Apps with little merging (OsmAnd~, Element) are
unaffected.

On current `main` every app converges even without the split, including `cpuinfo` 6.6.1, the
version P9 was found in (38 s, 1.73 GB): some change on `main` since then defused the hubs'
churn (not bisected; candidates #153, #154, #162). So the split is now a precision fix, and a cost
reduction where merging is heavy.

## How it was verified

- **The lost results are union flows.** Each of 29 sampled lost locations (6 `cpuinfo`, 11
  NewPipe, 12 CENO; three more were not traceable this way) was made a sink and queried on both
  indexes: every one had a path through the classes the split separates, and none after it. For
  example, `Cursor.getString` taint entering `ZipFilesKt$$ExternalSyntheticLambda3.invoke`, then
  `Data$$ExternalSyntheticLambda0.invoke`, and stored into `Unit.INSTANCE`. The few merged lambdas
  that recur as junctions include `cpuinfo`'s `Li;`, the hub P9 was found in.
- **S30's premise holds.** For every class whose merged methods are emptied, the index has no
  allocation of the plain class, so the emptied bodies were unreachable.
- **Tests.** `cargo test` passes for `ctadl-ascent` (370), `ctadl-dex` (23), `ctadl-import` and
  `dex-reader`; `cargo xtask regression --frontend dex,jvm` gives 135 passed, 0 failed, 1 known
  failure (below).

## Regression checks

- `nightly/tests/java/ClassIdMergedTryFlow`: a negative case, a merged class's object built inside a
  `try` must not reach every arm. It fails without change 4 and passes with it. The jvm frontend
  does not split merged classes, so `Jvm:ClassIdMergedTryFlow` is in `JVM_KNOWN_FAILURES`.
- `apk:merged-classes`: imports `com.noto` and pins its guardrail line (140 split, 3 not closed, 0
  over the id cap, 3 of 667 allocations with no known id), and fails if the guardrail warns. The
  `apk:report` counts, re-pinned with each change, cover the split's effect on the call graph.

## Guardrail

`merged classes guardrail:` summary lines at `info`, and a warning for each check that trips:

| Where | Warns when | Split working | Broken |
|---|---|---|---|
| import | over 10% of merged classes exceed the 64-id cap | 0 | not seen |
| import | over 10% of split classes keep live merged methods | 2-4% | 12-15% (without change 4) |
| import | over 2% of allocations of split classes have no known id | about 0.1% | 2-4% (without change 4) |
| import | clones hold over 35% of the program's statements | 2-24% | 43% (NewPipe without S29) |
| index | over 300 call edges reach large live merged methods | 0-8 | an estimated 195-1,073 (without S30) |

None warns on the four apps above; with change 4 disabled, the closure and allocation warnings fire
on `cpuinfo` and NewPipe. The import also logs, at `debug`, each allocation it cannot resolve and
why.

## Open

- **Merged classes that are not closed** (2-4%): not `final`, or an allocation with an unknown id.
  Their merged methods stay live (P25).
- **Reflection:** an object of a merged class built by reflection is not seen, so S30 could empty a
  merged method it reaches.
- **Detector coverage:** a scan for classes that compare a constructor-set `int` field of `this`
  with constants, by `switch` or `if`, found no merged class the split misses; shapes outside that
  rule (an id computed by arithmetic, or kept in a non-`int` field) were not looked for.

## Samples

From F-Droid (`https://f-droid.org/repo/<file>`), except `com.noto`, which is in the repo
(`xtask/tests/dex/com.noto_54.apk`).

| App | File | SHA-256 |
|---|---|---|
| `cpuinfo` 6.6.1 | `com.kgurgul.cpuinfo_60601.apk` | `54cc5595b9a07e8c92b9949642794cf2503bbe42d7711a0ab2c6d6097ea0c3e3` |
| NewPipe 0.29.1 | `org.schabi.newpipe_1015.apk` | `1d66d19eedbab969b1be2e44e0ea35f5f8921cbe83b5c5ac810767ca1a84b7e8` |
| CENO 2.11.7 | `ie.equalit.ceno_2020110711.apk` | `60cb96ab48a39c44618f50ad5a56ff0070d0b99f017a44f1308d4bf3e7fe9d7d` |

`cpuinfo` 6.7.0 has no merged classes; use 6.6.1.
