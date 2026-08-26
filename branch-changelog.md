# Branch changelog - DO-NOT-MERGE

## Query engine: stop re-exploring the same code under different calling contexts

This implements `QUERY_CONTEXT_SUBSUMPTION_PLAN.md` §5. It makes the query phase use much
less time and memory. It does not change which flows are found.

### What was wrong

Commit `56728caf` taught the query search to follow edges that only make sense under a
particular calling context. To do that, each search state had to start carrying a call
string alongside the node it was visiting.

The problem is that the search treats `(node, call string)` as the thing it has already
visited. So once a call string is attached, the same piece of code gets walked again for
every distinct call string that reaches it. On the `fw_pppd` benchmark that turned a
6.2-million-state search into a 14-million-state one, and pushed peak memory from 1.48 GB
to 4.10 GB. Almost 8 million of those extra states were just re-visits of code the search
had already covered with no call string at all.

### The fix

A call string is a *restriction*: it can only ever stop the search from taking an edge,
never let it take one it otherwise couldn't. So if the search has already visited a node
carrying no call string, visiting that same node again while carrying one can't find
anything new — everything the second visit could reach, the first one already reaches.

More generally, a shorter call string is weaker than a longer one that ends the same way.
`[s2]` says "we're in the frame entered at s2"; `[s1,s2]` says that *and* "our caller was
entered at s1". So the search can skip a state whenever it has already visited the same
node under a weaker call string, walking one frame at a time: `[s1,s2]` → `[s2]` → empty.

Two details that matter:

- The other half of the annotation (the call/return bookkeeping bit) is compared exactly.
  There is a valid simplification available there too, but that bit gets written into the
  `taint` table, so collapsing it would change stored output. Left alone.
- This only helps if the weaker state got there first. So the search now drains all its
  context-free work before touching any context-carrying work. Without that ordering the
  savings are down to luck.

### What changed

- `ctadl-ir/src/graph/mod.rs` — new `LazyAnnotation::generalization()` hook that returns
  the next-weaker annotation. It defaults to `None`, so anything that doesn't opt in
  behaves exactly as before. The search consults it at both points where it checks whether
  it has already visited something, and now keeps two queues so weaker states run first.
- `ctadl-ascent/src/query_engine/search.rs` — `PathState` implements the hook by dropping
  the outermost frame of its call string. The reasoning for why this is safe is written out
  on the method, edge type by edge type.
- `ctadl-ascent/src/facts.rs` — `CallString::drop_outermost`, which trims the caller end.
  (The existing `pop` trims the other end.)
- The per-label debug log line now also prints how many states are carrying a call string,
  which is the number to watch if this ever stops working.
- Four new tests in `ctadl-ir/src/graph/tests.rs`.

### Results

`fw_pppd`, the benchmark the regression was originally bisected on. Both binaries query the
exact same indexed store, so nothing from the import phase can leak into the comparison:

| | before | after |
| --- | ---: | ---: |
| states in the `argv_input` search | 14,029,871 | 6,471,144 |
| of those, carrying a call string | 7,835,320 | 276,593 |
| peak memory | 4.09 GB | 2.17 GB |
| wall clock | 10.45 s | 6.99 s |

97% of the redundant states are gone. The remaining gap to main's 1.48 GB is a separate
issue (each state is simply bigger now) — see "Not done here".

### Testing

- **All 19 benchmarks produce identical findings.** Each was imported and indexed once,
  then queried by both the old binary and the new one. Same finding counts, same rules,
  messages, locations and flow endpoints, and the same code-flow path lengths, on every
  single one. The corpus is 12 TaintBench APKs, 4 Operation Mango binaries, and 3 firmware
  binaries.
- **`cajino_baidu` still reports 353**, which was the specific number to protect — those
  findings are what the context support bought in the first place. It also still has real
  context-carrying states (8% of its search survives pruning), which is the case where the
  contexts are actually earning their keep.
- **Full workspace test suite passes.**
- **The context-dispatch regression cases pass**: `C:funcptrcallee{source,sink,frame}` and
  the three `Lua:resolved-callee-*` cases. Whole Lua suite (28 cases) and whole tree-sitter
  C suite (26 pass, 2 pre-existing expected failures).
- **Cross-checked against the old closure engine** (`CTADL_QUERY_DATALOG=1`) on `fakedaum`,
  where 43% of the search carries call strings. The two engines agree to exactly the same
  degree before and after this change.

Two things I found along the way that are worth knowing, neither caused by this change:

- **The import phase is not deterministic.** Importing the same `cajino_baidu.apk` twice
  and querying both with the *same unmodified* binary gave 357 findings one time and 353
  the other. This is why every comparison above reuses one store for both sides.
- **The closure engine's reported paths are not deterministic either.** Same binary, same
  store, two runs: the findings were identical but the code-flow step counts differed.

### Known effects on output

The plan predicted two ways this could shift SARIF bytes without dropping findings: which
source endpoint a node gets attributed to, and which route a reported path takes. Neither
showed up anywhere in the corpus, but both remain possible on other inputs.

### Not done here

The plan's §6 — the other half of the regression — is deliberately left for a separate
change: storing a call string as a 32-bit id rather than a slice reference, and moving the
edge label out of the search state. Those shrink each state rather than reducing the number
of states, are worth roughly another third of peak memory, and have no semantic component.
That is what would close the remaining gap to main.
