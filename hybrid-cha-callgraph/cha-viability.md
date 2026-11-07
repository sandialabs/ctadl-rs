# Can CHA/RTA be the main technique, with the expensive calls configured out? - DO-NOT-MERGE

Measured with `ctadl report` over every app we have: **61 APKs** — 38 TaintBench malware samples
(linked out of the nix store and verified by the sha256 in each `app.json`), 9 F-Droid apps, and
14 large release apps from `~/apps`. They import as **64 programs** — an `.xapk` is one program
per split — carrying **7.45 million functions**, **35.9 million call sites** and **2.66 billion**
CHA call edges. Every app was measured twice over the same import and the two reports matched
byte for byte, so nothing below is noise. Nothing here needs an index: CHA resolution is a
function of the import.

**How to read the per-app numbers.** One app is one artifact, and an artifact may be several
programs with separate class hierarchies. Every count below is summed over an app's programs and
every distribution is rebuilt exactly from the per-program signature tables, which is possible
because a CHA target count is a function of the static signature: the complete signature table
*is* the targets-per-site histogram. (Two traps worth naming, because both were live in the
previous version of this page. The report's `top_by_excess` list silently drops every signature
whose excess is zero — 4,855 of antennapod's 29,365 — so any site percentage computed from it is
wrong; `top_by_targets` at `--top 1000000` is the complete table. And taking "the largest
program" understates a split app: Chrome's biggest split has 52,386 functions and 174,292 virtual
calls, while the three splits together have 118,964 and 338,331.)

`ctadl report` lives on the **`report` branch** (`/Volumes/Shampoo/ct-report-wt`), not on this
one, and the sweep used that build. It is based on `main`, and this branch changes nothing under
`frontends/`, `ctadl-ir/` or `ctadl-import/`, so the CHA numbers are the same ones this branch
would produce. The index A/B below uses this branch's own binary, which has its own import
format.

## The short answer

**Yes, and the config is much smaller than expected.** On a real app, a handful of method
signatures cause nearly all of the damage. Set those aside and what is left is almost a normal
call graph.

Two numbers carry the whole argument. On TikTok, plain CHA produces **1.45 billion** call edges.
Hand the worst **100 signatures** to some other technique and the rest of the program drops to
**12.8 million** edges — 113 times smaller. Across the large apps, the average call goes from
**53 possible targets to 1.7**.

The catch is honest and small: those 100 signatures cover 7–25% of the virtual call sites, so
that is how much work you are handing to the expensive technique.

**And "some other technique" is three techniques, not one.** Looking at each of those signatures
individually (the second half of this page): about half the imprecision is `Object.toString`,
`equals` and `hashCode`, which should be *modelled* at the call site rather than resolved at all;
an eighth is closures and callbacks, which is what hybrid inlining is actually good at; and the
largest population of sites is collections and iterators, which should also be modelled — and
whose propagation models CTADL already ships, pointed at the wrong end of the call. Doing that,
TikTok's 1.45 billion edges become **7.1 million**, and hybrid inlining runs on a median **1.6%**
of an app's virtual call sites instead of the **27%** it runs on today.

Three things that sound like levers are not: RTA, picking on individual call *sites*, and picking
on hot *callees*. Details below.

## How bad is plain CHA to start with?

A "call site" is one call instruction. A "target" is a method the call might go to. CHA says a
virtual call can reach every method in the hierarchy that could implement it.

| | TaintBench (38) | F-Droid (9) | `~/apps` (14) |
| --- | --- | --- | --- |
| calls that are virtual | 65% | 60% | 54% |
| virtual calls with exactly one target | 90% | 77% | 71% |
| median targets per call | 1 | 1 | 1 |
| 90th-percentile targets per call | 1 | 12 | 18 |
| 99th-percentile targets per call | 4 | 1,809 | 976 |
| worst single call | 12 | 2,141 | 7,832 |
| **average** targets per call | 1.1 | 31.0 | **52.7** |

(Medians across apps in each corpus.)

Most calls are already fine. The median call has exactly one target. But the average is 53,
because a small number of calls have thousands. That gap between the median and the average is
the entire problem, and it is why the average alone tells you nothing useful.

## What plain CHA actually costs to index

The edge counts above are the input to the index, not its cost. So: three F-Droid apps indexed
both ways, under a 24 GiB memory guard and a one-hour timeout (`ab.sh`, `ab-main.sh`, guard from
the `memory-guard` skill, `-j 8`). Both binaries are here because they answer different halves of
it — this branch's, which is mid-rework, and `main`'s, which is not:

| app | virtual sites | CHA edges | `mixed` (this branch) | `cha` (this branch) | `mixed` (`main`) | `cha` (`main`) |
| --- | ---: | ---: | --- | --- | --- | --- |
| antennapod | 145,272 | 740,263 | **19.4 s, 3.9 GiB** | killed at 24 GiB, 599 s | **15.1 s, 2.2 GiB** | **still running at 1 h**, under the cap |
| newpipe | 172,011 | 2,303,387 | killed at 24 GiB, 1,283 s | killed at 24 GiB, 350 s | killed at 27 GiB, 1,717 s | — |
| schildi | 310,764 | 26,306,094 | killed at 24 GiB, 139 s | killed at 24 GiB, 124 s | — | — |

Two things fall out of that table, and they are the two halves of the question this page is
about.

**Plain CHA does not fit on the smallest F-Droid app in the corpus.** 740,263 edges, 145,272
virtual sites — and the index either eats 24 GiB in ten minutes (this branch) or is still going
after an hour (`main`). That is the scalability half answered outright: "just turn CHA on" is not
a plan, which is why the rest of this page is about what to hand the expensive calls to.

**But today's `mixed` does not fit either, and that is not a rework artifact.** newpipe — 73,029
functions, 22.7% of its virtual sites deferred to hybrid inlining — blew the cap on *both*
binaries, at 1,283 s on this branch and 1,717 s and 27 GiB on `main`. schildi died in 139 s. The
rework does cost something (antennapod: 19.4 s at 3.9 GiB against `main`'s 15.1 s at 2.2 GiB) but
it is not what makes newpipe fail. The expensive thing is how much of the program hybrid inlining
is asked to cover, and that is exactly what the design below cuts by a factor of sixteen.

## The carve-out: how few signatures do you need?

Rank method signatures by the *extra* targets they cause — the targets beyond the one real call
each site has to have. A method with one target causes zero extra targets no matter how often it
is called, so this ranking ignores calls that are already exact.

| signatures needed to cover… | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| 50% of the extra targets | 6 | **2** | **2** |
| 80% | 43 | 6 | **3** |
| 90% | 90 | 10 | **8** |
| 95% | 171 | 23 | **18** |

(Medians across the apps in each corpus.)

On real apps, **two signatures cause half the damage** and about **ten cause 90%**. That is a
config file a person can read.

## What is left after you carve them out?

| | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| extra targets left after the top 10 | 40% | 9.5% | **7.4%** |
| after the top 100 | 8.9% | 1.9% | **1.0%** |
| targets per call, before | 1.14 | 31.0 | **52.7** |
| targets per call, after the top 100 | 1.04 | 1.53 | **1.70** |
| virtual calls handed to the other technique | 9.9% | 12.8% | **16.0%** |

App by app, the largest ones:

| app | functions | virtual calls | CHA edges | after top 10 | after top 100 | calls moved |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| TikTok | 1,868,349 | 5,211,407 | 1,449,294,468 | 72,583,918 | **12,778,833** | 15.3% |
| WhatsApp Business | 379,725 | 1,088,334 | 211,763,533 | 8,031,594 | 2,085,957 | 23.7% |
| Messenger 570 | 435,612 | 1,111,029 | 150,710,148 | 8,554,759 | 2,269,719 | 21.6% |
| DuckDuckGo | 459,247 | 616,909 | 89,136,056 | 5,663,049 | 1,327,566 | 15.4% |
| Telegram 12.9 | 226,606 | 578,285 | 21,994,285 | 2,561,932 | 881,022 | 13.6% |
| VLC | 242,243 | 380,849 | 13,537,066 | 2,048,023 | 841,145 | 13.8% |
| Chrome (3 splits) | 118,964 | 338,331 | 3,128,798 | 612,370 | 416,876 | 7.9% |

Even ten carve-outs get you most of the way — TikTok goes from 1.45 billion to 72.6 million. The
next ninety are worth another factor of six.

## Can you ship one list, or must you profile each app?

Mostly you can ship one. Take the 20 signatures that carry the most of an app's excess, summed
over all 61 apps as a share of each app's own excess, and apply that fixed list unchanged:

| | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| a fixed 20-signature list covers | 37% | **89%** | **87%** |
| each app's own best 20 would cover | 70% | 95% | 96% |

So a shipped default gets about 88% of the benefit on real apps, and per-app profiling is worth
another 6–9 points. Worth supporting both, but the default is most of the value.

It survives obfuscation because the worst offenders live in classes the obfuscator cannot rename —
`java.lang.Object`, `java.util.Iterator`. The list is:

`Object.toString` (in 47 apps), `Activity.onCreate` (59), `Object.equals` (48), `Object.hashCode`
(47), `Runnable.run` (44), `Service.onCreate` (46), `Function1.invoke` (21), `Iterator.next` (37),
`Iterator.hasNext` (37), `TLObject.serializeToStream` (3), `Function0.invoke` (21),
`Service.onDestroy` (40), `Parcelable$Creator.createFromParcel` (41), `Function2.invoke` (15),
`ContentObserver.onChange` (16), `AsyncTask.onPreExecute` (12), `Application.onCreate` (28),
`Thread.start` (25), `Application.attachBaseContext` (11), `Activity.onDestroy` (43).

Ranking by how many apps a signature *appears* in instead would put `StringBuilder.append` first —
present in all 61 and monomorphic in every one of them. Excess is the ranking that matters.

Three methods on `Object` account for 1.21 billion extra targets across the 23 real apps by
themselves — as many edges as 45% of the entire corpus's call graph.

## Three things that are not levers

**RTA is not worth turning on for this.** Restricting targets to classes the program actually
creates removes a median of only **3.7%** of edges on the large apps and **2.1%** on F-Droid. (It
removes 33% on TaintBench, but those are small apps that ship almost no library code.) Large apps
allocate nearly everything they declare, so there is little for RTA to drop. It is also a lower
bound — it cannot see objects made by reflection or by library code we did not import — so the
real saving is smaller still. Keep it as a measurement, not as the strategy.

**Configuring individual call sites does nothing.** The ten worst call *instructions* own
**0.002%** of all call edges on a large app, and never more than 0.02% on any real app (0.02% on
the TaintBench toys too, where there are only a few thousand sites to begin with). Ten
instructions out of five million cannot matter. The unit that works is the method signature,
which covers every call that dispatches on it.

**Hot callees are not the place to configure either.** The 10 busiest methods receive **0.52%** of
all call edges on a large app; the busiest 100 receive **2.1%**. This surprised us, and the reason
is worth knowing: `Object.toString` has thousands of targets, so its edges are spread thin across
thousands of different callees rather than piling onto a few. The damage is concentrated on the
calling side and diffuse on the receiving side. **Configure by call-site signature, not by
callee.** (On TaintBench the same measurement reads 23% and 43%, which is what a corpus with no
library code looks like — another reason not to tune against it.)

## Two more knobs worth having

**Interface calls are a different population and should be configurable separately.** They are
only 11–18% of virtual calls, but on real apps just **8.7–16%** of them resolve to a single
target, against **81–91%** for ordinary class-virtual calls. Pooling the two describes neither.
They own **31–42%** of the excess.

Interface calls also create most of the apparent recursion. Deleting interface edges takes the
functions sitting inside a call cycle from a median of **37.6% of a large app down to 6.4%**, and
DuckDuckGo's largest cycle from 115,062 functions to 5,067. If inlining has to terminate against
a giant cycle, most of that cycle is an artifact of how interface calls are resolved rather than
something the program actually does.

**`invoke-super` is nearly free precision that is currently being wasted.** Super calls are about
1.2% of virtual calls, and only **4.4%** of them resolve to a single target — against 81% for
ordinary virtual calls. That is not a property of super calls. The instruction names the class to
start from, and CTADL resolves it as though the receiver type were unknown. On TikTok that is
49,774 sites of which 311 resolve exactly. Fixing it is a resolution change needing its own
soundness argument, but it is the cheapest precision left, and the IR support it needs already
exists on the `report` branch: `CallStyle::JavaCall` there carries a `JavaDispatch` telling
virtual from interface from super, which this branch's IR does not have.

## Things any design has to handle

- **0.04–0.3% of virtual calls resolve to nothing at all.** Missing library code, native methods
  or reflection. Small, but it is where the graph is silently unsound.
- **Half of interface calls name a type the app never declares** (median 50–54%; 432,334 such
  sites on TikTok alone) — `java.util.Iterator` in an app that does not ship the framework. Any
  per-type configuration only covers the rest.
- **TaintBench behaves differently and that is expected.** Those apps are small malware samples
  that do not ship the framework classes, so their worst signatures are app-specific and a shared
  default list only covers 37%. They are a good crash test, not a good guide to the config.

## What the carved-out calls should be handed to

Carving a signature out is half a design. The other half is what it is handed *to*, and the
answer is not one technique. It is three, and the choice is made by **what the callee is**, not by
how many targets it has.

Every signature in the corpus was classified and ranked — the ranked list is `sigstudy/rank.txt`,
the classifier that groups them is `sigstudy/buckets.py`:

| what the signature is | median % of an app's excess | median % of its virtual sites | median % of what `mixed` defers today | should get |
| --- | ---: | ---: | ---: | --- |
| the `Object` contract — `toString`, `hashCode`, `equals`, `clone` | **52.1%** | 4.70% | 3.6% | a model |
| closures and callbacks — `FunctionN.invoke`, `Runnable.run`, `Provider.get`, `invokeSuspend` | **12.8%** | 1.94% | 7.6% | hybrid inlining |
| collections and iterators — `Iterator.next`, `List.get`, `Map.put`, … | 7.3% | **10.92%** | **33.2%** | a model |
| serialization — `createFromParcel`, `ProtoAdapter.decode`, `TypeAdapter.read` | 0.2% | 0.07% | 0.1% | hybrid inlining |
| Android lifecycle and `super` dispatch | 0.5% | 1.30% | 3.8% | a resolution fix, not a fallback |
| resource `close`/`dispose` | 0.1% | 0.15% | 0.5% | a model, and *not* an empty one |
| everything else, app-specific | 3.1% | **79.59%** | **49.2%** | CHA |

(Medians over the 23 real apps; ranges and pooled totals in `sigstudy/buckets.txt`. The same table
for TaintBench is there too and looks different — 33.9% of its excess is app-specific and 27.0%
is lifecycle/`super` — for the reason already given.)

The rows those buckets are made of: the signatures that own the most excess across the 23 real
apps, each with what this page concludes it should be handed to. This is the list the question
"what should be done with the calls that have thousands of targets" is actually about, so it is
worth reading one row at a time.

| signature | apps | sites | CHA targets (median) | median % of that app's excess | pooled excess | dispatch | handled by |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| `java.lang.Object.toString` | 23 | 75,892 | 2,141 | 17.7% | 633,889,337 | virtual | model |
| `aop.a.invokeSuspend` | 1 | 16,534 | 21,257 | 24.3% | 351,446,704 | virtual | inline (by name) |
| `java.lang.Object.hashCode` | 23 | 37,798 | 1,809 | 10.7% | 340,535,138 | virtual | model |
| `kotlin.jvm.functions.Function0.invoke` | 21 | 32,991 | 780 | 1.2% | 284,811,625 | interface | inline (SAM) |
| `aop.a.create` | 1 | 15,117 | 15,754 | 16.5% | 238,138,101 | virtual | inline (over K) |
| `java.lang.Object.equals` | 23 | 34,318 | 1,813 | 14.5% | 232,567,509 | virtual | model |
| `kotlin.jvm.functions.Function1.invoke` | 21 | 45,512 | 971 | 4.2% | 160,111,550 | interface | inline (SAM) |
| `kotlin.jvm.functions.Function2.invoke` | 15 | 9,624 | 674 | 1.8% | 77,052,454 | interface | inline (SAM) |
| `java.lang.Runnable.run` | 23 | 9,913 | 982 | 0.6% | 45,164,225 | interface | inline (by name) |
| `java.util.Iterator.hasNext` | 23 | 267,543 | 80 | 2.3% | 36,256,446 | interface | model (empty) |
| `com.squareup.wire.ProtoAdapter.decode` | 2 | 17,669 | 1,420 | 2.8% | 35,040,610 | virtual | inline (over K) |
| `java.util.Iterator.next` | 23 | 232,739 | 82 | 2.0% | 34,290,226 | interface | model |
| `dagger.internal.Provider.get` | 3 | 12,866 | 273 | 1.0% | 29,995,381 | interface | inline (by name) |
| `org.telegram.tgnet.TLObject.serializeToStream` | 3 | 7,435 | 2,887 | 34.4% | 21,406,143 | virtual | inline (over K) |
| `android.os.Parcelable$Creator.createFromParcel` | 22 | 5,652 | 159 | 0.1% | 7,693,201 | interface | inline (by name) |
| `com.google.gson.TypeAdapter.read` | 1 | 4,553 | 1,153 | 0.4% | 5,245,056 | virtual | inline (over K) |
| `java.util.List.size` | 23 | 83,897 | 54 | 0.4% | 5,041,976 | interface | model (empty) |
| `java.util.List.get` | 23 | 60,384 | 75 | 0.6% | 4,347,764 | interface | model |
| `X.01D3.getValue` | 1 | 81,162 | 54 | 0.3% | 4,301,586 | interface | inline (over K) |
| `java.util.Map.put` | 23 | 148,176 | 17 | 0.1% | 3,871,056 | interface | model |
| `java.util.List.iterator` | 23 | 105,996 | 27 | 0.1% | 3,721,910 | interface | model |
| `java.lang.Object.clone` | 23 | 20,462 | 70 | 0.1% | 2,975,306 | virtual | model |
| `java.util.Map.get` | 23 | 55,765 | 24 | 0.1% | 2,262,093 | interface | model |
| `java.lang.Iterable.iterator` | 23 | 26,355 | 46 | 0.2% | 2,069,538 | interface | model |
| `java.util.AbstractCollection.size` | 18 | 21,550 | 42 | 0.1% | 1,580,476 | virtual | model |
| `com.bytedance.assem.arch.core.UIAssem.onViewCreated` | 1 | 1,013 | 1,543 | 0.1% | 1,562,046 | **super** | resolution fix |
| `X.09A.invoke` | 1 | 496 | 1,989 | 0.5% | 986,048 | interface | inline (SAM) |
| `org.telegram.messenger.Utilities$Callback.run` | 3 | 1,658 | 584 | 1.6% | 972,889 | interface | inline (SAM) |

Reading down that list, the population sorts itself into five kinds and nothing else shows up:

1. **The `Object` contract** (rows 1, 3, 6, 22): four signatures, every class in the program
   overrides them, and the target set is therefore "every class". They are also the ones with a
   *contract*, which is why they are modellable — see the IR walk below.
2. **Closures** (`FunctionN.invoke`, `Runnable.run`, `invokeSuspend`, `Provider.get`, the
   obfuscated `X/09A.invoke`): a one-method interface whose implementations are unrelated
   fragments of the program. No contract, nothing to model, and the receiver's identity is
   exactly what a context-sensitive technique can recover. This is what hybrid inlining is for.
3. **Containers and iterators** (rows 10, 12, 17, 18, 20, 21, 23, 24, 25): individually cheap —
   17 to 82 targets — but they are where the *calls* are. `Iterator.hasNext` alone is 267,543
   sites, more than any other signature in the corpus.
4. **Generated serializers** (`ProtoAdapter.decode`, `TypeAdapter.read`, `createFromParcel`,
   `serializeToStream`): one abstract method per wire type, hundreds to thousands of generated
   implementations. Concentrated in the apps that use that library at all — `serializeToStream`
   is **34.4%** of Telegram's entire excess and does not exist anywhere else.
5. **`super` and lifecycle** (`UIAssem.onViewCreated`, 100% super-dispatched, 1,543 targets for an
   instruction that names its class): not a fallback problem at all. A resolution bug.

Two of those rows are the reason a shipped name list cannot be the whole answer. `aop.a` is
TikTok's obfuscated `kotlin.coroutines.jvm.internal.BaseContinuationImpl`; between `invokeSuspend`
and `create` it is 41% of TikTok's excess, and no default list will ever contain the name `aop.a`.
The structural test below catches that class of thing: across the 23 real apps it flags **237** of
the apps' 100-worst signatures as single-abstract-method interfaces — `LX/09A;->invoke` (1,989
targets), `LX/0sSp;->invoke` (946), `LX/SJG;->LB` (637), `LX/0z2;->get` (24,571 sites) among them —
without knowing a single name.

Two rows carry the whole design. **The imprecision and the call volume are in different places.**
The `Object` contract plus the closures are a median **7.4% of an app's virtual call sites and
85.5% of its excess targets** (3.9–13.3% of sites, 51–96% of excess). Collections are the mirror
image: **10.9% of the sites, 7.3% of the excess** — and **a third of everything `mixed` defers
today**. Any rule that hands "expensive-looking calls" to one expensive technique gets one of
those two badly wrong.

### No — not hybrid inlining for every non-monomorphic call

That is exactly what `--strategy mixed` does today
(`ctadl-ascent/src/codegen/mod.rs:535`): CHA when the site resolves to exactly one target, and
`callee_info` — hybrid inlining — for every other virtual call. Measured over the same imports,
that rule defers **15.8–40.6% of virtual call sites, median 27.0%**:

| app | virtual calls | resolve to 1 target | deferred to hybrid inlining today |
| --- | ---: | ---: | ---: |
| TikTok | 5,211,407 | 70.3% | **29.5%** |
| WhatsApp Business | 1,088,334 | 59.9% | **40.1%** |
| Messenger 570 | 1,111,029 | 59.3% | **40.6%** |
| DuckDuckGo | 616,909 | 65.0% | **34.6%** |
| Telegram 12.9 | 578,285 | 73.0% | **27.0%** |
| VLC | 380,849 | 63.2% | **36.7%** |
| Chrome | 338,331 | 84.0% | **15.8%** |

So hybrid inlining runs today on a sixth to two fifths of every virtual call in the program, to
buy precision that lives in about 2% of them. Four reasons not to keep that shape:

**It is roughly fifteen times more hybrid inlining than the imprecision justifies.** A
`List.get` site with 75 targets and a `Function1.invoke` site with 971 are both
"non-monomorphic", and today both defer. One has a two-row summary that never varies across its
targets; the other is a lambda whose body is the rest of the program. A third of what `mixed`
defers is the container contract.

**It does not fit.** newpipe — 172,011 virtual sites, 22.7% of them deferred — did not index
under a 24 GiB cap on *either* binary (1,283 s on this branch, 1,717 s and 27 GiB on `main`), and
schildi died in 139 s. Whatever else is true, the current deferral rate is not affordable today.

**The propagation is transitive and the call graph is one big cycle.** Rule 1.2 in
`ctadl-ascent/src/index_engine/mod.rs:1425` pushes a critical summary from a deferred site up
through every caller; rule 2.2 (`:1455`) pushes resolvents back down, one row per decision. A
median **37.6% of all functions on one of the 14 large apps sits in a non-trivial SCC** (28.6%
over all 23 real apps), and the largest single cycle is **693,372 functions on TikTok**, 179,172
on WhatsApp Business, 115,062 on DuckDuckGo. A critical summary raised anywhere inside such a
cycle reaches all of it. Deleting interface edges drops that median to 6.4% — the cycles are
largely an artifact of how interface calls resolve, which is an argument for resolving fewer of
them by inlining, not more.

**On Java, deferring deletes edges rather than adding precision.** The `Mixed` arm pushes
`callee_info` and no `call` rows, so a deferred site ends up with exactly the callees hybrid
inlining can find for it — and it finds one only when a `call_target_assign` object actually
reaches the receiver (`emit_callee_resolvents`, `codegen/mod.rs:1060`, keys resolvents on the
*allocated* class symbol). Where no allocation reaches it, the site has no callees at all. Half of
interface calls name a type the app never declares (median 50–54%; 432,334 such sites on TikTok
alone) and those receivers routinely arrive from library code we never imported. The Lua arm of
the same `match` refuses to do this and says why, with numbers, at `codegen/mod.rs:599`: deferring
alone took Prosody from 2,865 matched sinks / 806 tainted paths down to 2,145 / 263. Nothing makes
Java safer here. It is just unmeasured.

### Yes — model them, and for the contract methods the models are already written

`ctadl-ascent/src/models/defaults/java-index.jsonl:24` carries a model matching **any** method
named `toString` that returns a `String`, giving it `Argument(*) → Return`. It is correct, it
applies to all of an app's `toString` bodies, and it buys nothing for scalability: a model
attaches a summary to the *callee*, and CHA still enumerates every callee at the site, so the site
still gets thousands of `call` rows, each instantiating that summary. Adding
`modes: ["skip-analysis"]` does not help either — that drops the bodies, not the edges.

The same file already carries the container contract too — `Iterator.next`, `List.get`, `Map.put`,
`Collection.add`, `iterator()`, `Map$Entry.getKey`/`getValue`, with real access paths
(`Argument(0).\[] → Return`) — at lines 39, 75–96, matched by `parents` lists that name the same
concrete classes the sites declare. **The propagation semantics for the two modellable buckets
are written and shipped. What is missing is only the ability to attach them to a call site
instead of to a callee.**

For the `Object` contract that is not merely cheap, it is faithful. Walking the imported IR of
every override in two apps, one heavily obfuscated and one not (`sigstudy/purity.py`, dumps in
`sigstudy/ir/`):

| app | method | overrides | writes a named field | writes a static | writes only an array | calls out |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Chrome | `equals` | 389 | **0 (0.0%)** | 0 | 0 | 69% |
| Chrome | `hashCode` | 344 | 11 (3.2%) | 0 | 0 | 88% |
| Chrome | `toString` | 298 | 6 (2.0%) | 0 | 0 | 84% |
| Chrome | `clone` | 12 | 3 (25.0%) | 0 | 0 | 67% |
| NewPipe | `equals` | 866 | **1 (0.1%)** | 0 | 0 | 79% |
| NewPipe | `hashCode` | 854 | 19 (2.2%) | 0 | 37 (4.3%) | 82% |
| NewPipe | `toString` | 991 | 5 (0.5%) | 0 | 37 (3.7%) | 86% |
| NewPipe | `clone` | 26 | 3 (11.5%) | 0 | 0 | 92% |
| NewPipe | `invoke` | 790 | **98 (12.4%)** | 4 | 11 (1.4%) | 96% |

Then every one of those field writes was traced to where its base object came from
(`sigstudy/purity-other.txt`). Over both apps and all four contract methods there are **204**
named-field writes: **181** into an object reached from the receiver's own field, **13** into an
object allocated inside the body, **10** whose base the walk could not attribute — and **zero**
into an object that arrived as an argument or into a static. Read by hand, the unattributed ones
are the same pattern one level deeper: `Lcp1;->hashCode` memoises into `@p0.a.q`, a Guava-style
`toStringHelper` pushes the receiver's fields into a helper it just built, and
`ConstraintSet$Constraint.clone` copies the receiver into the clone it is about to return.

`Argument(0) → Return` is therefore a complete over-approximation of what the real bodies do, and
it costs one summary row per site instead of up to 21,257 call edges.

The last row is the control. `invoke` bodies write named fields 12.4% of the time, 73 of those to
an object that is *not* the receiver, they write statics, and they call something 96% of the time:
arbitrary code, no contract, nothing to model. That is the line between the two mechanisms, and it
is visible in the IR rather than argued from taste.

Collections are the same argument with a different payoff. `Iterator.hasNext` alone is 267,543
call sites across the corpus — more than any other signature — for a median 2.3% of an app's
excess. Modelling it removes the largest single population of sites from *both* CHA and hybrid
inlining, which is worth more than the edges it saves.

**One consequence that has to be stated, because it is the real cost of this mechanism.** A
dispatch model does not merely replace a propagation: it removes the callee subtree from the
analysis *at that site*. Any source or sink the user's models declare inside those callees can
never match through it. For `toString`/`equals`/`hashCode`/`clone` and the container contract that
is harmless — nobody writes a sink model on `Map.put`. It is not harmless in general, and the
classifier must therefore refuse to model a signature whose CHA target set intersects a matched
source or sink; the matches are in hand at codegen time (`ProgramModelMatches`), so this is a set
intersection and a warning, not a research problem.

### Yes — a few can be skipped, but say it as an empty model, and the list is shorter than it looks

The honest form of "skip this call" is a dispatch model with an empty propagation list: the
signature is recognised, the target set is discarded, and nothing flows. The candidates are the
predicates and the sizes, where the only output is a primitive derived from the receiver's *shape*
rather than its contents:

`Iterator.hasNext`, `Collection.size` / `isEmpty` / `contains` / `containsAll`,
`Map.containsKey` / `containsValue`, `List.indexOf` / `lastIndexOf`, `Comparable.compareTo`,
`Comparator.compare` — named on the interfaces and on the abstract bases, since a site declaring
`java.util.AbstractCollection.size` is the same call (`AbstractCollection.size` alone is 21,550
sites across the corpus, and the simulation below counts it as modelled rather than skipped
because the shipped list as written names only the interfaces).

That list is measurable: it is a median **2.6%** of an app's virtual call sites (up to 4.5%),
and it is what the `of those, skipped` column of the policy table below counts.

**`close()` and `dispose()` came off this list.** They were on it in the previous version of this
page on the strength of a `void` return. Walking their bodies says otherwise: 17% of Chrome's 110
`close` overrides and 32% of NewPipe's 182 write a named field, and they call out 39% and 68% of
the time. None of those writes escapes to an argument or a static — but that is not the risk. The
risk is that a `close()` implementation is where buffered data is finally handed to a file or a
socket, which is exactly where a sink model lives, and an empty model at the site makes that sink
unreachable. `close` should be *modelled* — its target set discarded and no propagation emitted,
which is not the same as pretending the call is not there — or simply left on CHA: across the real
apps it has a
median of 19 targets and a worst case of 176, so a threshold of 32 already leaves most of its
sites there. The simulation below treats it that way.

Three cautions, in order of how much they matter:

- **`equals` and `hashCode` are not on the skip list.** They also return a primitive, but one
  derived from the receiver's *contents*, and a taint configuration that follows an integer is
  entitled to follow it. Model them as `Argument(*) → Return`; do not drop them.
- **A `void` return is not evidence that nothing happens.** `Runnable.run()V` and
  `Activity.onCreate(Bundle)V` return nothing and do everything; `close()V` is the case above. For
  the record, signatures that are primitives-in and primitive-or-void-out are **20.8% of the
  excess and 31.2% of the sites** — a bucket far too big to take wholesale, and the reason the
  classifier is keyed on the contract rather than on the descriptor. (`sigstudy/desc.txt`.)
- **Skipping is a configuration, not a default posture.** It is the one lever here that can
  silently lose a finding, so it should be the shortest of the three lists and every entry should
  be defensible on its own.

### The algorithm

One classifier, evaluated per call site at codegen time, in this order. Everything it needs — the
declared class, the simple name, the descriptor and the CHA resolvent set — is already in hand at
`ctadl-ascent/src/codegen/mod.rs:535`.

1. **Dispatch model.** The static signature matches a shipped or user-supplied dispatch model, and
   no target of that site is a matched source or sink: emit the model's summary at the site and
   *no* targets. An empty propagation list is the "skip" case.
2. **Threshold.** `resolvents.len() <= K`: emit CHA edges. This is the common case, and it is
   where the ordinary program lives.
3. **Hybrid inlining.** Everything left — more than `K` targets, no model — defers, exactly as
   `Mixed` does today.

**Step 1 before step 2 is a deliberate choice and it is worth knowing what it costs.** Checking
the threshold first would leave a monomorphic `toString` resolved exactly and model only the
imprecise sites. That is more precise and still cheap: the graph grows by a median **1.35x** (23x
→ 21x smaller than plain CHA) and the modelled share falls from 16.1% of sites to 5.1%
(`sigstudy/model-order.md`). Model-first is the recommendation, because the IR walk above says the
model is a faithful over-approximation of those bodies and because taking 16% of call sites out of
the engine entirely is the point; threshold-first should be the flag for a precision-sensitive
run, not the default.

The obvious alternative was measured and rejected: gating step 3 on the receiver being
closure-shaped (a single-abstract-method interface, or a name on a shipped callback list) and
leaving everything else on CHA. It holds up on the small apps and collapses on the large ones,
because the residue is not noise — on TikTok it is 175,598 sites and **302 million edges** against
the policy's 7.06 million, mostly `aop.a.create` (an obfuscated Kotlin coroutine continuation
dispatched on an abstract *class*, so no interface test can see it), `ProtoAdapter` / `TypeAdapter`
/ `Parcelable$Creator` serializers, and `X/01D3.getValue`. A plain threshold has no such hole.
Full comparison in `sigstudy/policy-all.txt` and `sigstudy/residue-tiktok.txt`.

The closure-shaped test is still worth having, as a diagnostic and as a way to name what step 3 is
for, and it is available two ways:

- **Structurally**, for interfaces the app itself declares: an interface whose abstract-method set
  is a singleton. The report already computes this (`report/callgraph.rs`, `TypeFacts::from_vmt`,
  on the `report` branch), and it survives obfuscation where a name list cannot — it is what flags
  `LX/0sSp;->invoke`, `LX/09A;->invoke` and `LX/SJG;->LB`, names no default list could hold. On
  TikTok, SAM-interface call sites are 64,532 — 1.24% of virtual sites — and own 401M of its 1.45B
  excess edges. It does *not* reach `aop.a.invokeSuspend`, which dispatches on an abstract class
  rather than an interface; that is the hole the threshold in step 2 exists to cover.
- **By name**, for framework interfaces the app never declares and therefore cannot be tested
  structurally: `Runnable`, `Callable`, `FunctionN`, `java.util.function.*`, the RxJava `Observer`
  / `Subscriber` family, `Provider`, `Parcelable$Creator`, and the `invokeSuspend`/`create` pair
  that Kotlin's compiler emits on every continuation. These are the names an obfuscator cannot
  touch, which is the same reason the shipped carve-out list works at all.

Two things the classifier deliberately does not key on, both measured earlier: individual call
*sites* (the ten worst own 0.002% of edges) and *callees* (`Object.toString`'s targets
spread its edges across thousands of receivers). The unit is the call site's static signature.

### How it hooks up

Almost none of this is new machinery. The pieces exist and are already wired to each other; what
is missing is a way to say "this *call site*" instead of "this function".

**Where the decision goes.** One function, called from the `CallStyle::JavaCall` arm of
`CodegenVisitor::visit_statement` (`ctadl-ascent/src/codegen/mod.rs:535`), replacing the
`resolvents.len() == 1` test that is there now:

```rust
enum SiteAction { Model(DispatchModelId), Cha, Defer }

fn classify(&self, cls: &JavaClass, name: &JavaSimpleName, desc: &JavaSignature,
            resolvents: usize) -> SiteAction
```

The `Cha` arm is the existing `CallResolutionStrategy::Cha` body (`:511`) and the `Defer` arm is
the existing `Hi` body (`:522`) — both already written, a few lines apart. Only `Model` is new.

**How a model replaces a target set, with no engine change.** `IndexFacts::summary`
(`index_engine/mod.rs:106`) is a *base* relation, and phase 2 of codegen already writes matched
propagations into it (`codegen/model_matches.rs:106`, `codegen_propagations`). So a dispatch model
is: intern one synthetic function per modelled signature, emit its `summary` rows once, and at
each matching call site emit a single `facts.call(site, synth_fn)` row instead of the resolvent
loop. Everything downstream is ordinary summary instantiation and no inference rule needs to know
a dispatch model was involved. The module header of `codegen/model_matches.rs` currently says
"nothing synthesizes a function"; this would be the first thing that does, and it is one
`get_or_add_function` call. An empty propagation list needs no synthetic function at all — emit
nothing, and count it.

**How it is configured.** The model-generator DSL already has the matcher this needs
(`ctadl-ascent/src/models/ctadl-model-generator.schema.json`): `signature_match` with
`name`/`names` and `parent`/`parents`, `extends` for "a supertype of the owning class satisfies
this", `signature_pattern` for the descriptor. It also already has a `find: "callsites"` — but it
is not this. That one selects sites by *the callee they resolve to*, which is the wrong end of the
question (resolving is what we are trying to avoid), and it explicitly rejects `propagation`:
`models/json.rs:1755` errors with "'propagation' is not supported with find: callsites", because
a summary is a property of a function. What is needed is a sibling, `find: "dispatch"`, whose
`where` is evaluated against the *site's declared class, name and descriptor* and whose `model`
carries the usual `propagation` list. That is the whole surface, and the error at `json.rs:1755`
is where it announces itself today.

The defaults then ship in `models/defaults/java-index.jsonl` beside the models already there —
in most cases as a second copy of an existing entry with `find` changed, since the propagation
lists for `toString` and the containers are already written — and a user's `--models` file
overrides or extends them per app, which is the same knob per-app carve-out profiling wanted.

**The threshold and the closure list.** `K` is one CLI flag and one field on the index config. The
framework-callback list is data in the same JSONL. The structural single-abstract-method test
needs the class hierarchy, which `ClassHierarchyAnalysis` already builds from the
`VirtualMethodTable`; the `report` branch computes exactly this predicate in `report/callgraph.rs`
and that code can move down into `ctadl-ir`'s `call` module so the report and codegen read one
definition — with the fix noted below, that it has to close over super-interfaces.

**What comes along with the `report` branch.** Two things this design wants are already
implemented there and nowhere else: the `JavaDispatch` kind on `CallStyle::JavaCall` (which is
what an `invoke-super` fix needs, and what lets the classifier treat interface sites separately),
and the SAM predicate. That branch's import format is 7 against this branch's 6, so they cannot
share a store — merging it is a prerequisite, not an optional extra.

**What has to be counted.** Every site should land in exactly one of four buckets — modelled,
skipped, CHA, inlined — and the counts belong on the index's summary line next to the existing
`models: N summary row(s), … M function bod(ies) not analyzed`
(`ctadl-ascent/src/cli/mod.rs:252`). Without it, a mis-scoped dispatch model silently swallows a
signature and the only symptom is a missing finding.

### What it buys

Simulated over the *complete* signature tables — every signature in every app, not the top 100.
Target count is a function of the signature, so a full signature list is also the exact
targets-per-site histogram, which is what a threshold has to be chosen against. Tables in
`out/*/full/`, simulation in `sigstudy/policy.py`, at **K = 32**:

| app | virtual calls | plain CHA edges | edges under the policy | smaller by | vs `mixed` today | sites modelled | of those, skipped | sites inlined | inlined today |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| TikTok | 5,211,407 | 1,449,294,468 | **7,057,082** | 205x | 1.9x | 15.7% | 2.7% | **4.50%** | 29.5% |
| WhatsApp Business | 1,088,334 | 211,763,533 | 2,134,421 | 99x | 3.3x | 18.4% | 3.9% | 1.36% | 40.1% |
| Messenger 570 | 1,111,029 | 150,710,148 | 2,441,023 | 62x | 3.7x | 20.1% | 3.7% | 1.68% | 40.6% |
| DuckDuckGo | 616,909 | 89,136,056 | 1,116,744 | 80x | 2.8x | 13.8% | 2.6% | 4.40% | 34.6% |
| Telegram 12.9 | 578,285 | 21,994,285 | 947,617 | 23x | 2.2x | 14.0% | 1.1% | 2.87% | 27.0% |
| VLC | 380,849 | 13,537,066 | 812,850 | 17x | 3.4x | 16.1% | 2.6% | 2.29% | 36.7% |
| Chrome | 338,331 | 3,128,798 | 428,068 | 7x | 1.5x | 14.8% | 1.8% | 0.94% | 15.8% |
| **median over all 23 real apps** | | | | **23x** | **2.2x** | **16.1%** | **2.6%** | **1.63%** | **27.0%** |

TikTok is the number to hold on to. The top-100 signature carve-out in the earlier section got it
to 12.8 million edges with 15.3% of its virtual calls handed to the expensive technique. This gets
it to **7.06 million edges with 4.5%** — a third of the expensive work — and the 15.7% that went to
models costs one summary row per site regardless of how many targets the signature had.

The "vs `mixed` today" column is the one that speaks to whether this can be indexed at all: the
policy's graph is **1.4–3.7x (median 2.2x)** the `call` rows `mixed` emits today, while plain CHA
is 4–205x. It is in the range of a configuration that already runs on the smaller apps — the most
that can be said without implementing it. It is not proof, and the A/B above is the reason to say
so plainly: `mixed` itself failed on two of the three F-Droid apps under a 24 GiB cap. Cutting
hybrid inlining from 27% of sites to 1.6% is the largest single lever available, but the index
engine's own cost has to come down as well, and neither this simulation nor that A/B says by how
much.

`K` is not delicate:

| K | CHA graph, vs no carve-out | virtual sites inlined | virtual sites modelled |
| ---: | ---: | ---: | ---: |
| 4 | 36x smaller | 8.18% | 16.07% |
| 8 | 32x smaller | 5.41% | 16.07% |
| 16 | 27x smaller | 3.75% | 16.07% |
| 24 | 24x smaller | 1.92% | 16.07% |
| **32** | **23x smaller** | **1.63%** | **16.07%** |
| 48 | 20x smaller | 1.41% | 16.07% |
| 64 | 19x smaller | 1.33% | 16.07% |
| 128 | 18x smaller | 0.91% | 16.07% |

Between 16 and 32 the inlined share falls by more than half and the graph grows by a sixth; past
32 both flatten. That is the knee, and it is a default, not a constant — it is one flag.

### What this does not cover

- **`super` is a resolution bug, not a fallback problem.** TikTok has 49,774 `invoke-super` sites
  and 311 of them resolve to one target; its worst single super signature,
  `UIAssem.onViewCreated`, carries 1,543 targets for an instruction that *names the class to start
  from*. Fixing the resolution is worth more than any policy applied to them, and is the cheapest
  precision left (see "Two more knobs").
- **The structural SAM test has a known hole.** On DuckDuckGo it detects `javax.inject.Provider`
  and `kotlin.jvm.functions.Function1` but *not* `dagger.internal.Provider` — 2,918 targets over
  10,031 sites, **29.3M excess edges** on its own — nor `dagger.internal.Factory`, because their
  single method is declared on the interface they extend rather than on themselves. The test has
  to close over the transitive interface closure, not just declared methods. The shipped name list
  happens to cover `dagger.internal.Provider`, but only because someone thought of it, which is
  exactly the failure mode the structural test is there to avoid.
- **Hybrid inlining's soundness gap is narrowed, not closed.** Going from 27% of sites to 1.6%
  shrinks the exposure by a factor of sixteen, but a deferred site whose receiver never receives an
  allocation still ends with no callees. Options, cheapest first: emit CHA edges alongside
  `callee_info` when the target set is under some larger cap, which is what the Lua arm already
  does unconditionally; or emit one synthetic unknown-callee edge so the hole shows up in `ctadl
  report` instead of being silent. This should be settled before the strategy is switched, not
  after.
- **A dispatch model hides whatever is inside the callees.** Stated in full above; the mitigation
  is the source/sink intersection check in step 1 of the classifier.
- **TaintBench behaves differently here too.** Its median app puts **7.1%** of its excess in the
  `Object` contract and **33.7%** in app-specific signatures, against 52.1% and 3.1% on the real
  apps; its median collections share is 0.2% and its lifecycle/`super` share is 27.0%. Small
  samples that ship none of the framework. The policy still applies — the shipped defaults just
  carry much less of it, which is the same thing the fixed-list section already found.

## Cost, and where everything is

The report itself is affordable at this scale. TikTok, the largest app, imports in 95 s at an
18.3 GB peak and reports in 1,222 s at a 26.8 GB peak — that is the `--top 1000000` pass, which
writes a 431 MB JSON; the ordinary `--top 10` report is minutes, not tens of minutes. Every
F-Droid app reports in under 11 minutes at the full setting and seconds at `--top 10`. The whole
61-app sweep — four reports per app: the full signature table, a `--top 10` JSON and text pair,
and a second full table compared byte for byte against the first — took about 50 minutes wall
clock at 3–6 way parallelism.

Everything is kept, nothing deleted:

```
/Volumes/Shampoo/ctadl-sweep/
  manifest.tsv              the 61 artifacts, corpus and slug
  corpus/taintbench/        the 38 TaintBench APKs (links into the nix store, hash-verified)
  run-one.sh                import + 3 reports + a byte-stability re-run, per artifact
  ab.sh, ab-main.sh         the index A/B drivers (this branch's binary, and main's)
  logs/                     timings.tsv (import/report seconds, peak bytes, stability), and
                            the per-batch sweep logs
  out/{taintbench,fdroid,apps}/
    stores/                 the imports, kept -- re-reportable without re-importing
    full/                   COMPLETE signature tables, --top 1000000, 2.4 GB
    json/                   per-app JSON and text reports at --top 10
    digest/                 the compact per-app form every analysis below reads
    logs/                   per-app import/report logs, with /usr/bin/time -l peaks
  agg_tables.md             the full descriptive tables this page summarises
  viab_tables.md            the full carve-out tables
  app_rows.json             one row per app, every derived number
  ab/                       the index A/B: --strategy cha vs mixed under a 24 GiB guard
                            (results.tsv, per-run logs)
  sigstudy/
    digest.py, lib.py       loading, and the statistics every table is built from
    tables.py               agg_tables.md, viab_tables.md, app_rows.json
    buckets.py/.txt         the classifier and the bucket tables; fixed20.json is the
                            shipped-default list
    rank.txt                every signature ranked by pooled excess, per corpus
    policy.py               the policy simulation; policy-all.txt, policy-table-K32.md,
                            k-table.md, model-order.md, residue-tiktok.txt
    extra.py                desc.txt (descriptor shapes), fi.txt (SAM), cover.txt, rec.txt
    purity.py               the contract-method body walk; purity.txt, purity-other.txt,
                            purity-skip.txt
    ir/                     the dumped IR those walks read (ctadl inspect --dump-ir)
```

Re-running any table is `python3 sigstudy/<script>.py` over the kept digests — seconds, no
re-import. Re-running the sweep itself is `xargs -P N -n 3 ./run-one.sh < manifest.tsv`.
