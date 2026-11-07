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

Three things that sound like levers are not: RTA, picking on individual call *sites*, and picking
on hot *callees*. Details at the end.

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
```

The `--top 100` pass took 4 minutes for all 61 apps because it reuses the kept imports. That is
the argument for keeping them.
