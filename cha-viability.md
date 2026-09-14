# Can CHA/RTA be the main technique, with the expensive calls configured out? - DO-NOT-MERGE

Measured with `ctadl report` over every app we have: **61 APKs** — 38 TaintBench malware samples,
9 F-Droid apps built from source, and 14 large release apps from `~/apps` (10 distinct apps; three
Telegram versions, two Messenger and two WhatsApp Messenger builds are in there too). Together
that is 64 programs, 7.5 million functions and 35.9 million call sites. Every app was measured
twice and the two runs matched byte for byte, so nothing below is noise.

## The short answer

**Yes, and the config is much smaller than expected.** On a real app, a handful of method
signatures cause nearly all of the damage. Set those aside and what is left is almost a normal
call graph.

Two numbers carry the whole argument. On TikTok, plain CHA produces **1.45 billion** call edges.
Hand the worst **100 signatures** to some other technique and the rest of the program drops to
**12.0 million** edges — 121 times smaller. Across the large apps, the average call goes from
**53 possible targets to 1.9**.

The catch is honest and small: those 100 signatures cover 7–25% of the virtual call sites, so
that is how much work you are handing to the expensive technique.

**And "some other technique" is three techniques, not one.** Looking at each of those signatures
individually (the second half of this page): about half the imprecision is `Object.toString`,
`equals` and `hashCode`, which should be *modelled* at the call site rather than resolved at all;
a tenth is closures and callbacks, which is what hybrid inlining is actually good at; and the
largest population of sites is collections and iterators, which should also be modelled. Doing
that, TikTok's 1.45 billion edges become **6.0 million**, and hybrid inlining runs on a median
**1.7%** of an app's virtual call sites instead of the **27%** it runs on today.

Three things that sound like levers are not: RTA, picking on individual call *sites*, and picking
on hot *callees*. Details below.

## How bad is plain CHA to start with?

A "call site" is one call instruction. A "target" is a method the call might go to. CHA says a
virtual call can reach every method in the hierarchy that could implement it.

| | TaintBench (38) | F-Droid (9) | `~/apps` (14) |
| --- | --- | --- | --- |
| calls that are virtual | 65% | 61% | 54% |
| virtual calls with exactly one target | 90% | 77% | 71% |
| median targets per call | 1 | 1 | 1 |
| 99th-percentile targets per call | 4 | 1,809 | 976 |
| worst single call | 12 | 2,141 | 7,832 |
| **average** targets per call | 1.1 | 31.0 | **52.7** |

(Medians across apps in each corpus.)

Most calls are already fine. The median call has exactly one target. But the average is 53,
because a small number of calls have thousands. That gap between the median and the average is
the entire problem, and it is why the average alone tells you nothing useful.

## The carve-out: how few signatures do you need?

Rank method signatures by the *extra* targets they cause — the targets beyond the one real call
each site has to have. A method with one target causes zero extra targets no matter how often it
is called, so this ranking ignores calls that are already exact.

| signatures needed to cover… | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| 50% of the extra targets | 6 | **2** | **2** |
| 80% | 43 | 6 | **3** |
| 90% | 90 | 10 | **7** |
| 95% | >100 | 23 | **12** |

(Medians across the apps in each corpus. TaintBench needs more than 100 signatures to reach 95%
on 21 of its 38 apps, for the reason in the last section.)

On real apps, **two signatures cause half the damage** and about **ten cause 90%**. That is a
config file a person can read.

## What is left after you carve them out?

| | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| extra targets left after the top 10 | 40% | 10% | **5.4%** |
| after the top 100 | 8.9% | 1.9% | **1.0%** |
| targets per call, before | 1.14 | 31.0 | **52.7** |
| targets per call, after the top 100 | 1.05 | 1.59 | **1.88** |
| virtual calls handed to the other technique | 4.3% | 12.8% | **16.0%** |

App by app, the largest ones:

| app | functions | virtual calls | CHA edges | after top 10 | after top 100 | calls moved |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| TikTok | 1,868,340 | 5,211,407 | 1,449,294,468 | 72,316,199 | **11,982,162** | 15.3% |
| WhatsApp Business | 379,725 | 1,088,334 | 211,763,533 | 7,971,769 | 1,827,644 | 23.7% |
| Messenger 570 | 435,612 | 1,111,029 | 150,710,148 | 8,465,632 | 2,029,937 | 21.6% |
| DuckDuckGo | 459,247 | 616,909 | 89,136,056 | 5,626,230 | 1,232,727 | 15.4% |
| Telegram 12.9 | 226,606 | 578,285 | 21,994,285 | 2,547,678 | 802,276 | 13.6% |
| VLC | 242,243 | 380,849 | 13,537,066 | 2,026,585 | 788,721 | 13.8% |
| Chrome | 52,386 | 174,292 | 1,418,193 | 233,178 | 185,574 | 7.4% |

Even ten carve-outs get you most of the way — TikTok goes from 1.45 billion to 72 million. The
next ninety are worth another factor of six.

## Can you ship one list, or must you profile each app?

Mostly you can ship one. Taking the 20 signatures that show up most often across all 61 apps and
applying that fixed list unchanged:

| | TaintBench | F-Droid | `~/apps` |
| --- | --- | --- | --- |
| a fixed 20-signature list covers | 31% | **92%** | **88%** |
| each app's own best 20 would cover | 70% | 95% | 97% |

So a shipped default gets about 90% of the benefit on real apps, and per-app profiling is worth
another 5–9 points. Worth supporting both, but the default is most of the value.

It survives obfuscation because the worst offenders live in classes the obfuscator cannot rename —
`java.lang.Object`, `java.util.Iterator`. The list is:

`Object.toString` (47 apps), `Object.equals` (46), `Object.hashCode` (43), `Runnable.run` (38),
`Iterator.next` (37), `Iterator.hasNext` (37), `Activity.onCreate` (24), `List.get` (24),
`List.size` (24), `Parcelable$Creator.createFromParcel` (21), `Map.put` (18), `Function0.invoke`
(16), `Function1.invoke` (16), `Set.iterator` (14), `List.iterator` (13), `Map.get` (12),
`List.add` (12), `Service.onCreate` (11), `Handler.handleMessage` (11), `Function2.invoke` (11).

Three methods on `Object` account for 1.2 billion extra targets across the corpus by themselves.

## Three things that are not levers

**RTA is not worth turning on for this.** Restricting targets to classes the program actually
creates removes a median of only **3.7%** of edges on the large apps and **2.1%** on F-Droid. (It
removes 33% on TaintBench, but those are small apps that ship almost no library code.) Large apps
allocate nearly everything they declare, so there is little for RTA to drop. It is also a lower
bound — it cannot see objects made by reflection or by library code we did not import — so the
real saving is smaller still. Keep it as a measurement, not as the strategy.

**Configuring individual call sites does nothing.** The ten worst call *instructions* own 0.18% of
all edges on a large app. Ten instructions out of five million cannot matter. The unit that works
is the method signature, which covers every call that dispatches on it.

**Hot callees are not the place to configure either.** The 10 busiest methods receive only 0.52%
of all call edges on a large app; the busiest 100 receive 2.1%. This surprised us, and the reason
is worth knowing: `Object.toString` has 20,524 targets, so its edges are spread thin across 20,524
different callees rather than piling onto a few. The damage is concentrated on the calling side
and diffuse on the receiving side. **Configure by call-site signature, not by callee.**

## Two more knobs worth having

**Interface calls are a different population and should be configurable separately.** They are
only 15–18% of virtual calls, but on real apps just **8.7–16%** of them resolve to a single
target, against **81–91%** for ordinary class-virtual calls. Pooling the two describes neither.

Interface calls also create most of the apparent recursion. Deleting interface edges takes the
functions sitting inside a call cycle from a median of **38% of a large app down to 6.4%**, and
DuckDuckGo's largest cycle from 115,062 functions to 5,067. If inlining has to terminate against
a giant cycle, most of that cycle is an artifact of how interface calls are resolved rather than
something the program actually does.

**`invoke-super` is nearly free precision that is currently being wasted.** Super calls are about
1.2% of virtual calls, and only **4.4%** of them resolve to a single target — against 81% for
ordinary virtual calls. That is not a property of super calls. The instruction names the class to
start from, and CTADL resolves it as though the receiver type were unknown. Fixing it is a
resolution change needing its own soundness argument, but it is the cheapest precision left.

## Things any design has to handle

- **0.02–0.5% of virtual calls resolve to nothing at all.** Missing library code, native methods
  or reflection. Small, but it is where the graph is silently unsound.
- **Half of interface calls name a type the app never declares** (median 50–54%) — `java.util.Iterator`
  in an app that does not ship the framework. Any per-type configuration only covers the rest.
- **TaintBench behaves differently and that is expected.** Those apps are small malware samples
  that do not ship the framework classes, so their worst signatures are app-specific and a shared
  default list only covers 31%. They are a good crash test, not a good guide to the config.

## What the carved-out calls should be handed to

Carving a signature out is half a design. The other half is what it is handed *to*, and the
answer is not one technique. It is three, and the choice is made by **what the callee is**, not by
how many targets it has.

Every signature in the corpus that owns a measurable share of the imprecision was looked at
individually — the ranked list is `sigstudy/rank.txt`, the classifier that groups them is
`sigstudy/buckets.py`:

| what the signature is | median % of an app's excess | median % of its virtual sites | should get |
| --- | ---: | ---: | --- |
| the `Object` contract — `toString`, `hashCode`, `equals`, `clone` | **51.9%** | 0.84% | a model |
| closures and callbacks — `FunctionN.invoke`, `Runnable.run`, `Provider.get`, `invokeSuspend` | **12.4%** | 0.51% | hybrid inlining |
| collections and iterators — `Iterator.next`, `List.get`, `Map.put`, … | 7.2% | **7.76%** | a model |
| serialization — `createFromParcel`, `ProtoAdapter.decode`, `TypeAdapter.read` | 0.1% | 0.05% | hybrid inlining |
| Android lifecycle and `super` dispatch | 0.4% | 0.16% | a resolution fix, not a fallback |
| everything else, app-specific | 2.4% | 3.68% | CHA |

(Medians over the 23 real apps; ranges and pooled totals in `sigstudy/buckets.txt`. The same table
for TaintBench is there too and looks different, for the reason already given.)

The rows those buckets are made of — the 24 signatures that own the most excess across the 23 real
apps, each with what this page concludes it should be handed to:

| signature | apps | sites | CHA targets (median) | median % of that app's excess | dispatch | handled by |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| `java.lang.Object.toString` | 23 | 75,671 | 2,141 | 17.7% | virtual | model |
| `aop.a.invokeSuspend` | 1 | 16,534 | 21,257 | 24.3% | virtual | inline (over K) |
| `java.lang.Object.hashCode` | 23 | 37,079 | 1,809 | 10.2% | virtual | model |
| `kotlin.jvm.functions.Function0.invoke` | 20 | 32,989 | 784 | 1.2% | interface | inline (name) |
| `aop.a.create` | 1 | 15,117 | 15,754 | 16.5% | virtual | inline (over K) |
| `java.lang.Object.equals` | 23 | 33,648 | 1,813 | 7.6% | virtual | model |
| `kotlin.jvm.functions.Function1.invoke` | 18 | 45,100 | 1,103 | 4.8% | interface | inline (name) |
| `kotlin.jvm.functions.Function2.invoke` | 15 | 9,624 | 674 | 1.8% | interface | inline (name) |
| `java.lang.Runnable.run` | 23 | 9,790 | 1,175 | 0.6% | interface | inline (name) |
| `java.util.Iterator.hasNext` | 23 | 266,263 | 80 | 2.3% | interface | model |
| `com.squareup.wire.ProtoAdapter.decode` | 2 | 17,669 | 1,420 | 2.8% | virtual | inline (over K) |
| `java.util.Iterator.next` | 23 | 231,469 | 82 | 2.0% | interface | model |
| `dagger.internal.Provider.get` | 3 | 12,866 | 273 | 1.0% | interface | inline (name) |
| `org.telegram.tgnet.TLObject.serializeToStream` | 3 | 7,435 | 2,887 | 34.4% | virtual | inline (over K) |
| `android.os.Parcelable$Creator.createFromParcel` | 19 | 5,531 | 233 | 0.1% | interface | inline (name) |
| `com.google.gson.TypeAdapter.read` | 1 | 4,553 | 1,153 | 0.4% | virtual | inline (over K) |
| `java.util.List.size` | 23 | 83,017 | 54 | 0.4% | interface | model |
| `java.util.List.get` | 23 | 59,327 | 75 | 0.6% | interface | model |
| `X.01D3.getValue` | 1 | 81,162 | 54 | 0.3% | interface | inline (over K) |
| `java.util.Map.put` | 22 | 147,921 | 20 | 0.1% | interface | model |
| `java.util.List.iterator` | 23 | 105,721 | 27 | 0.1% | interface | model |
| `java.lang.Object.clone` | 18 | 20,373 | 91 | 0.1% | virtual | model |
| `java.util.Map.get` | 22 | 55,466 | 28 | 0.1% | interface | model |
| `java.lang.Iterable.iterator` | 23 | 26,174 | 46 | 0.2% | interface | model |

Two of those rows are the reason a shipped name list cannot be the whole answer. `aop.a` is
TikTok's obfuscated `kotlin.coroutines.jvm.internal.BaseContinuationImpl`; between `invokeSuspend`
and `create` it is 41% of TikTok's excess, and no default list will ever contain the name `aop.a`.
The structural test below catches that class of thing: across the corpus it flags 371 of the
apps' worst signatures as single-abstract-method interfaces — `LX/09A;->invoke` (1,989 targets),
`LX/0sSp;->invoke` (946), `LX/SJG;->LB` (637) among them — without knowing a single name.

Two rows carry the whole design. **The imprecision and the call volume are in different places.**
The `Object` contract plus the closures are a median **1.9% of an app's virtual call sites and
84.8% of its excess targets** (0.5–4.1% of sites, 50–96% of excess). Collections are the mirror
image: **7.8% of the sites, 7.2% of the excess.** Any rule that hands "expensive-looking calls" to
one expensive technique gets one of those two badly wrong.

### No — not hybrid inlining for every non-monomorphic call

That is exactly what `--strategy mixed` does today
(`ctadl-ascent/src/codegen/mod.rs:535`): CHA when the site resolves to exactly one target, and
`callee_info` — hybrid inlining — for every other virtual call. Measured over the same stores,
that rule defers **14.0–40.6% of virtual call sites, median 27.0%**:

| app | virtual calls | resolve to 1 target | deferred to hybrid inlining today |
| --- | ---: | ---: | ---: |
| TikTok | 5,211,407 | 70.3% | **29.5%** |
| WhatsApp Business | 1,088,334 | 59.9% | **40.1%** |
| Messenger 570 | 1,111,029 | 59.3% | **40.6%** |
| DuckDuckGo | 616,909 | 65.0% | **34.6%** |
| Telegram 12.9 | 578,285 | 73.0% | **27.0%** |
| VLC | 380,849 | 63.2% | **36.7%** |
| Chrome | 174,292 | 85.9% | **14.0%** |

So hybrid inlining runs today on a fifth to two fifths of every virtual call in the program, to
buy precision that lives in about 1.9% of them. Three reasons not to keep that shape:

**It is roughly fifteen times more hybrid inlining than the imprecision justifies.** A
`List.get` site with 75 targets and a `Function1.invoke` site with 1,103 are both
"non-monomorphic", and today both defer. One has a two-row summary that never varies across its
targets; the other is a lambda whose body is the rest of the program.

**The propagation is transitive and the call graph is one big cycle.** Rule 1.2 in
`ctadl-ascent/src/index_engine/mod.rs:1098` pushes a critical summary from a deferred site up
through every caller; rule 2.2 pushes resolvents back down with a call string. A median **38% of
all functions on one of the 14 large apps sits in a non-trivial SCC** (28.6% over all 23 real
apps), and the largest single cycle is **693,372 functions on TikTok**, 179,172 on WhatsApp
Business, 115,062 on DuckDuckGo. A critical summary raised anywhere inside such a cycle reaches
all of it. Deleting interface edges drops that median to 6.4% — the cycles are largely an artifact
of how interface calls resolve, which is an argument for resolving fewer of them by inlining, not
more.

**On Java, deferring deletes edges rather than adding precision.** The `Mixed` arm pushes
`callee_info` and no `call` rows, so a deferred site ends up with exactly the callees hybrid
inlining can find for it — and it finds one only when a `call_target_assign` object actually
reaches the receiver (`emit_callee_resolvents`, `codegen/mod.rs:1060`, keys resolvents on the
*allocated* class symbol). Where no allocation reaches it, the site has no callees at all. Half of
interface calls name a type the app never declares (median 50–54%; 432,334 such sites on TikTok
alone) and those receivers routinely arrive from library code we never imported. The Lua arm of
the same `match` refuses to do this and says why, with numbers, at `codegen/mod.rs:613`: deferring
alone took Prosody from 2,865 matched sinks / 806 tainted paths down to 2,145 / 263. Nothing makes
Java safer here. It is just unmeasured.

### Yes — model them, and for `toString` the model is already written

`ctadl-ascent/src/models/defaults/java-index.jsonl` opens with a model matching **any** method
named `toString` that returns a `String`, giving it `Argument(*) → Return`. It is correct, it
applies to all 20,524 of TikTok's `toString` bodies, and it buys nothing for scalability: a model
attaches a summary to the *callee*, and CHA still enumerates every callee at the site, so the site
still gets 20,524 `call` rows, each instantiating that summary. Adding `modes: ["skip-analysis"]`
does not help either — that drops the bodies, not the edges.

What is missing is a model attached to the **call site's static signature**, replacing the target
set instead of decorating it.

For the `Object` contract that is not merely cheap, it is faithful. Walking the imported IR of
every override in two apps, one heavily obfuscated and one not (`sigstudy/purity2.py`, dumps in
`sigstudy/ir/`):

| app | method | overrides | writes a named field | writes only a varargs array | calls out |
| --- | --- | ---: | ---: | ---: | ---: |
| Chrome | `equals` | 389 | **0 (0.0%)** | 0 | 69% |
| Chrome | `hashCode` | 344 | 11 (3.2%) | 0 | 88% |
| Chrome | `toString` | 298 | 6 (2.0%) | 0 | 84% |
| NewPipe | `equals` | 866 | **1 (0.1%)** | 0 | 79% |
| NewPipe | `hashCode` | 854 | 19 (2.2%) | 37 (4.3%) | 82% |
| NewPipe | `toString` | 991 | 5 (0.5%) | 37 (3.7%) | 86% |
| NewPipe | `invoke` | 790 | **100 (12.7%)** | 10 (1.3%) | 97% |

Every one of those named-field writes, read by hand, is a memo cache into the receiver's own field
— a `hash` slot, a cached `String`, `CallableReference.reflected` — and every array write is a
local `Object[]` built for `Objects.hash(…)` or `String.format(…)`. Not one moves data to another
object, to a static, or to an argument. `Argument(0) → Return` is therefore a complete
over-approximation of what the real bodies do, and it costs one summary row per site instead of up
to 17,499 call edges.

The last row is the control. `invoke` bodies write named fields 12.7% of the time and call
something 97% of the time: arbitrary code, no contract, nothing to model. That is the line between
the two mechanisms, and it is visible in the IR rather than argued from taste.

Collections are the same argument with a different payoff. `Iterator.hasNext` alone is 266,263
call sites across the corpus — more than any other signature — for a median 2.3% of an app's
excess. Modelling it removes the largest single population of sites from *both* CHA and hybrid
inlining, which is worth more than the edges it saves.

### Yes — a few can be skipped, but say it as an empty model

The honest form of "skip this call" is a dispatch model with an empty propagation list: the
signature is recognised, the target set is discarded, and nothing flows. The candidates are the
predicates and the sizes, where the only output is a primitive derived from the receiver's shape
rather than its contents:

`Iterator.hasNext`, `Collection.size` / `isEmpty` / `contains` / `containsAll`,
`Map.containsKey` / `containsValue`, `List.indexOf` / `lastIndexOf`, `Comparable.compareTo`,
`Comparator.compare`, `Closeable.close`, `AutoCloseable.close`.

Three cautions, in order of how much they matter:

- **`equals` and `hashCode` are not on that list.** They also return a primitive, but one derived
  from the receiver's *contents*, and a taint configuration that follows an integer is entitled to
  follow it. Model them as `Argument(*) → Return`; do not drop them.
- **A `void` return is not evidence that nothing happens.** `Runnable.run()V` and
  `Activity.onCreate(Bundle)V` return nothing and do everything. Descriptor shape is not the test;
  the contract is. For the record, signatures that are primitives-in and primitive-or-void-out are
  16.7% of the excess and 5.0% of the sites — a bucket big enough to be tempting and wrong to take
  wholesale.
- **Skipping is a configuration, not a default posture.** It is the one lever here that can
  silently lose a finding, so it should be the shortest of the three lists and every entry should
  be defensible on its own.

### The algorithm

One classifier, evaluated per call site at codegen time, in this order. Everything it needs — the
declared class, the simple name, the descriptor and the CHA resolvent set — is already in hand at
`ctadl-ascent/src/codegen/mod.rs:535`.

1. **Dispatch model.** The static signature matches a shipped or user-supplied dispatch model:
   emit the model's summary at the site and *no* targets. An empty propagation list is the "skip"
   case.
2. **Threshold.** `resolvents.len() <= K`: emit CHA edges. This is the common case, and it is
   where the ordinary program lives.
3. **Hybrid inlining.** Everything left — more than `K` targets, no model — defers, exactly as
   `Mixed` does today.

The obvious alternative was measured and rejected: gating step 3 on the receiver being
closure-shaped (a single-abstract-method interface, or a name on a shipped callback list) and
leaving everything else on CHA. It holds up on the small apps and collapses on the large ones,
because the residue is not noise — on TikTok it is 189,273 sites and **647 million edges**, mostly
`aop.a.invokeSuspend` and `aop.a.create` (obfuscated Kotlin coroutine continuations dispatched on
an abstract *class*, so no interface test can see them) and `ProtoAdapter` / `TypeAdapter`
serializers. A plain threshold has no such hole. Full comparison in `sigstudy/policy-all.txt`.

The closure-shaped test is still worth having, as a diagnostic and as a way to name what step 3 is
for, and it is available two ways:

- **Structurally**, for interfaces the app itself declares: an interface whose abstract-method set
  is a singleton. The report already computes this (`report/callgraph.rs`, `TypeFacts::from_vmt`),
  and it survives obfuscation where a name list cannot — it is what flags `LX/0sSp;->invoke` and
  `LX/09A;->invoke`, names no default list could hold. On TikTok, SAM-interface call sites are
  64,532 — 1.2% of virtual sites — and own 401M of its 1.44B excess edges. It does *not* reach
  `aop.a.invokeSuspend`, which dispatches on an abstract class rather than an interface; that is
  the hole the threshold in step 2 exists to cover.
- **By name**, for framework interfaces the app never declares and therefore cannot be tested
  structurally: `Runnable`, `Callable`, `FunctionN`, `java.util.function.*`, the RxJava `Observer`
  / `Subscriber` / `Disposable` family, `Provider`, `Parcelable$Creator`. These are the names an
  obfuscator cannot touch, which is the same reason the shipped carve-out list works at all.

Two things the classifier deliberately does not key on, both measured earlier: individual call
*sites* (the ten worst own 0.18% of edges) and *callees* (`Object.toString`'s 20,524 targets
spread its edges across 20,524 receivers). The unit is the call site's static signature.

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

The `Cha` arm is the existing `CallResolutionStrategy::Cha` body and the `Defer` arm is the
existing `Hi` body — both already written, a few lines apart. Only `Model` is new.

**How a model replaces a target set, with no engine change.** `IndexFacts::summary`
(`index_engine/mod.rs:100`) is a *base* relation, and phase 2 of codegen already writes matched
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
this", `signature_pattern` for the descriptor. What it lacks is a `find` that selects *call sites
by their static signature* rather than *methods by their definition* — today `find: "methods"`
matches the callee, which is precisely why the existing universal `toString` model cannot help. A
`find: "dispatch"` whose `where` is evaluated against the site's declared class, name and
descriptor, and whose `model` carries the usual `propagation` list, is the whole surface. The
defaults then ship in `models/defaults/java-index.jsonl` beside the models already there, and a
user's `--models` file overrides or extends them per app — the same knob per-app carve-out
profiling wanted.

**The threshold and the closure list.** `K` is one CLI flag and one field on the index config. The
framework-callback list is data in the same JSONL. The structural single-abstract-method test
needs the class hierarchy, which `ClassHierarchyAnalysis` already builds from the
`VirtualMethodTable`; the report branch computes exactly this predicate in `report/callgraph.rs`
and that code can move down into `ctadl-ir`'s `call` module so the report and codegen read one
definition — with the fix noted below, that it has to close over super-interfaces.

**What has to be counted.** Every site should land in exactly one of four buckets — modelled,
skipped, CHA, inlined — and the counts belong on the index's summary line next to the existing
`models: N summary row(s), … M function bod(ies) not analyzed`
(`ctadl-ascent/src/cli/mod.rs:247`). Without it, a mis-scoped dispatch model silently swallows a
signature and the only symptom is a missing finding.

### What it buys

Simulated over the *complete* signature tables — every signature in every app, not the top 100.
Target count is a function of the signature, so a full signature list is also the exact
targets-per-site histogram, which is what a threshold has to be chosen against; the `--top 100`
tables cannot show it, because the 100th-ranked signature already carries a median of 56 targets
(up to 456 on TikTok) and everything a threshold would keep sits below that cut. Tables in `sigstudy/full/`,
simulation in `sigstudy/policy.py`, at **K = 32**:

| app | virtual calls | plain CHA edges | edges under the policy | smaller by | sites modelled | sites inlined | inlined today |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| TikTok | 5,211,407 | 1,449,294,468 | **6,042,614** | 240x | 20.2% | 4.50% | 29.5% |
| WhatsApp Business | 1,088,334 | 211,763,533 | 1,971,425 | 107x | 19.7% | 1.36% | 40.1% |
| Messenger 570 | 1,111,029 | 150,710,148 | 2,211,243 | 68x | 21.9% | 1.72% | 40.6% |
| DuckDuckGo | 616,909 | 89,136,056 | 1,006,940 | 89x | 14.7% | 4.45% | 34.6% |
| Telegram 12.9 | 578,285 | 21,994,285 | 867,722 | 25x | 14.5% | 2.87% | 27.0% |
| VLC | 380,849 | 13,537,066 | 759,355 | 18x | 16.2% | 2.29% | 36.7% |
| Chrome | 174,292 | 1,418,193 | 184,554 | 8x | 16.3% | 1.15% | 14.0% |
| **median over all 23 real apps** | | | | **25x** | **19.7%** | **1.72%** | **27.0%** |

TikTok is the number to hold on to. The top-100 signature carve-out in the earlier section got it
to 12.0 million edges with 15.3% of its virtual calls handed to the expensive technique. This gets
it to **6.0 million edges with 4.5%** — half the graph, a third of the expensive work — and the
20.2% that went to models costs one summary row per site regardless of how many targets the
signature had.

`K` is not delicate:

| K | CHA graph, vs no carve-out | virtual sites inlined | virtual sites modelled |
| ---: | ---: | ---: | ---: |
| 4 | 41x smaller | 8.19% | 19.7% |
| 8 | 36x smaller | 5.59% | 19.7% |
| 16 | 30x smaller | 3.75% | 19.7% |
| 24 | 26x smaller | 1.95% | 19.7% |
| **32** | **25x smaller** | **1.72%** | **19.7%** |
| 48 | 24x smaller | 1.39% | 19.7% |
| 64 | 22x smaller | 1.33% | 19.7% |
| 128 | 21x smaller | 0.93% | 19.7% |

Between 16 and 32 the inlined share falls by half and the graph grows by a fifth; past 32 both
flatten. That is the knee, and it is a default, not a constant — it is one flag.

### What this does not cover

- **`super` is a resolution bug, not a fallback problem.** 41% of the edges in DuckDuckGo's
  residue above the threshold are `super` dispatch — `MainProcessLifecycleObserver.onDestroy`,
  `DuckDuckGoActivity.onCreate` — each carrying ~100 targets for a call instruction that *names
  the class to start from*. Fixing the resolution is worth more than any policy applied to them,
  and is the cheapest precision left (see the previous section).
- **The structural SAM test has a known hole.** On DuckDuckGo it detects `javax.inject.Provider`
  and `kotlin.jvm.functions.Function1` but *not* `dagger.internal.Provider` — 29.3M edges on its
  own — nor `dagger.internal.Factory`, because their single method is declared on the interface
  they extend rather than on themselves. The test has to close over the transitive interface
  closure, not just declared methods. The shipped name list happens to cover `dagger.internal.Provider`,
  but only because someone thought of it, which is exactly the failure mode the structural test is
  there to avoid.
- **Hybrid inlining's soundness gap is narrowed, not closed.** Going from 27% of sites to 1.7%
  shrinks the exposure by a factor of fifteen, but a deferred site whose receiver never receives an
  allocation still ends with no callees. Options, cheapest first: emit CHA edges alongside
  `callee_info` when the target set is under some larger cap, which is what the Lua arm already
  does unconditionally; or emit one synthetic unknown-callee edge so the hole shows up in `ctadl
  report` instead of being silent. This should be settled before the strategy is switched, not
  after.
- **TaintBench behaves differently here too.** Its median app puts **6.9%** of its excess in the
  `Object` contract and **24.7%** in app-specific signatures, against 51.9% and 2.4% on the real
  apps; its median collections share is 0%. Small samples that ship none of the framework. The
  policy still applies — the shipped defaults just carry much less of it, which is the same thing
  the fixed-list section already found.

## Cost, and where everything is

The report itself is affordable at this scale. TikTok, the largest app, imports in 89 s and
reports in 112 s at a 26.8 GB peak. Every other app in the corpus reports in under 20 s. The
whole 61-app sweep took 20 minutes.

Everything is kept, nothing deleted:

```
/Volumes/Shampoo/ctadl-sweep/
  corpus/taintbench/        the 38 TaintBench APKs (links into the nix store, by hash)
  out/{taintbench,fdroid,apps}/
    stores/                 the imports, kept — re-reportable without re-importing
    json/                   per-app JSON and text reports (--top 10)
    json100/                the same at --top 100, used for the carve-out curve
    logs/, summary.md       per-app timings, peaks and stability
  agg_tables.md             the full descriptive tables this page summarises
  viab_tables.md            the full carve-out tables
  aggregate.py, viability.py, run.sh, top100.sh
  sigstudy/                 the per-signature study behind the second half of this page
    full/{taintbench,fdroid,apps}/   COMPLETE signature tables (--top 1000000), 2.3 GB
    full.sh                 the third pass that produced them, over the same kept imports
    rows.json, sigs.py      every signature row pulled out of the --top 100 reports
    rank.py/.txt            signatures ranked by excess, per corpus
    buckets.py/.txt         the classifier and the bucket tables
    desc.py/.txt            descriptor-shape breakdown
    policy.py               the policy simulation; policy-all.txt, policy-table-K32.md, k-table.md
    residue-tiktok.txt      what a closure-gated variant would leave behind
    purity2.py/.txt         the equals/hashCode/toString/invoke body walk
    ir/                     the dumped IR that walk reads (ctadl inspect --dump-ir)
    cover.py/.txt, fi.py/.txt, rec.py/.txt   deferral, SAM-interface and SCC tables
    ab.sh, ab/              --strategy cha vs mixed on the same store, under a memory cap
```

The `--top 100` pass took 4 minutes for all 61 apps because it reuses the kept imports. The full
signature pass took 25 minutes for the same reason. That is the argument for keeping them.
