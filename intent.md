# Intent: ctadl report command for getting detailed information about program under analysis - DO-NOT-MERGE

These are requirements for the `ctadl report` feature. Use these to

- Add a `ctadl report` subcommand that takes an import name and produces a report of statistics and
  other relevant program analysis info
- The first POC for this feature has to do with call graph analysis


Call graph measumerements and report output:

- Measure number of call sites, distinguishing direct and indirect/virtual calls
- For Dex+Java: Measure, for each virtual call, the number of CHA+RTA resolvents of that call. This
  is type-based resolution. Report on the top 10 call sites that have the most such resolvents.
- The difficult cases for CHA: `Object.equals/hashCode()` methods which have lots of
  implementations; Lambdas in Kotlin where every lambda in the program implements one interface, so
  want to measure things relevant for this
- Report what fraction of virtual call sites resolve to exactly one target, since those are the cheap
  ones and the percentage tells you how much work is actually left.
- Report the whole distribution of resolvents per call site (median, 90th/99th percentile, max), not
  just the average, because a handful of huge call sites hides behind a small mean.
- Measure how much of the total edge count comes from the top 10 or 100 worst call sites, to justify
  special-casing them instead of making the whole analysis more precise.
- Count call sites that resolve to zero targets, which usually means missing library code, native
  methods, or reflection, and tells you where the graph is unsound.
- Measure fan-in as well as fan-out, because a method called from thousands of sites is the thing
  that makes inlining-based approaches blow up.
- For receivers that trace back to an allocation, measure how many call frames away that allocation
  is, which directly tells you how deep hybrid inlining has to go to pay off.
- Separate interface calls from class-virtual calls in every measurement, because CHA behaves much
  worse on interfaces and the two should not be averaged together.
- For Kotlin, measure how many call sites go through a lambda/functional interface and how many of
  those have a single reachable lambda body, to see whether treating lambdas specially would help.
- Count how many CHA targets get thrown away once you restrict to types the program actually
  allocates, which is the direct payoff of RTA over plain CHA.
- Measure recursion and strongly connected components in the call graph, since those are the places
  where inlining cannot terminate on its own.
- Record analysis time, peak memory, and inlined-code size growth alongside the precision numbers, so
  the precision/cost tradeoff is visible in one place.

Evaluate on the taintbench apks in ../ct-taintbench and the apps in ~/apps (which are real, large apks)
