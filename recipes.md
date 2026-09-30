# Characterizing Datalog Rule Blowups

- Run benchmarks under a memory guard and note the time it takes to hit this guard. You need to stay under the guard to get Ascent to print useful information.
- Run the benchmark with an Ascent `generate_run_timeout` so that when it terminates, it still provides useful per-rule profile information.
- Instrument or verify the code is instrumented with tuple counts for each relation.
- Run with a growing sequence of timeouts. Like 10s, 20s, etc up until a reasonable timeout, below the memory guard. Since Ascent has to complete the iteration it's in the middle of when the timeout guard is hit, a small guard let's you calibrate the number appropriately.
- Rank by time and time-per-tuple.
- One thing that should fall out of this is expensive relations. Most analysis have just a few relations that take the most memory, and these should fall out of the ranking above.
- Ascent uses as many indices into a relation as necessary. These may duplicate the entire storage for the relation. Characterize expensive relations by the number and cost of each of their indices. Make sure to repeat the key indicess of the columns, e.g., `reach_0_2` for an index into `reach` that uses column 0 and column 2 as its keys.
- Ascent's `run_timeout` returns false from inside the loop of whichever SCC is running, and that return comes before the SCC’s local total/delta stores are moved back into self, and before the SCC’s time is recorded.  So every earlier `CTADL_INDEX_TIMEOUT_SECS` run lost all of its contents: locals reported 10.8 M rows, but its store held nothing, and scc 4 had no timing. The patched macro is in vendor/ascent_macro, attached through [patch.crates-io]. On a timeout it moves the last delta into total, breaks out of the loop, runs the SCC’s normal epilogue, and only then returns false. The plain run() is unchanged, and cargo test -p ctadl-ascent still passes (357 tests, 0 failures).


# Finding Unproductive Datalog Rules

Ascent plans rules basically in left-to-right order. An exception is that the first two relations in the body of a rule may be re-ordered dynamically depending on their size estimate. The goal of this skill is to find bad plans, i.e., bad join orders that result in degraded performance.

To find such rules:
- Observe a "memory plateau," where the engine runs for a while without consuming much memory
- Set a timeout using Ascent's timeout mechanism (if necessary) and SCC times summary so that you can observe how long each rule variant takes. If the problem converges, you don't need to set a timeout.
- You are looking for rules that take a lot of time relative to the number of tuples they've produced.
- You can patch a local copy of `ascent_macro` to count head insertions per rule variant, since it's some rule variants that may be responsible for the plateau behavior.
- If the program does not, sample several times in the last 1/5 or so of the run to gather runtime data on the rules that may not be productive
- When you find a relatively unproductive relation, sample tuples from the unproductive relation to see if anything jumps out
