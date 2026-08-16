# The TaintBench false positives: what causes them

The TaintBench run at commit `8baae049` finds 92 of 191 ground-truth flows. It
also reports the callee pair of 20 of the suite's 45 findings marked
`isNegative` — flows that look real but are not. By TaintBench's own rule all 20
are false positives. The suite counts 8 of them: the other 12 have the same
source and sink callees as a positive the run also detects, and DEX SARIF carries
no line information to tell the two call sites apart, so the suite's
shadowed-negative rule drops them. See
[`taintbench-head-to-head-results.md`](taintbench-head-to-head-results.md) for
the run itself and both counts side by side.

This document says what causes the 8 the suite counts, then what is and is not
known about the 12 it does not. All 8 are written down in the apps'
`expected.json`, so the suite passes. They are measured imprecision, not forgiven
imprecision.

| app | findings | pair |
| --- | --- | --- |
| `phospy` | #3 | `TelephonyManager.getDeviceId → DataOutputStream.write` |
| `phospy` | #4, #5 | `FileInputStream.<init> → DataOutputStream.writeUTF` |
| `proxy_samp` | #16 | `File.<init> → BufferedWriter.write` |
| `proxy_samp` | #20 | `File.<init> → Log.i` |
| `save_me` | #26, #27, #28 | `SQLiteDatabase.query → ContentValues.put` |

## The short version

Two things in the engine cause all eight. A third thing, in the suite, decides
which negatives get counted — it is why the number is 8 and not 20.

1. **Copies are treated as two-way.** When the program does `x = y`, ctadl puts
   `x` and `y` in the same group and shares taint across the whole group, in both
   directions. Whole methods end up in one group, so anything tainted anywhere in
   the method looks tainted everywhere in it. On its own it explains 5 of the 8.
2. **A model can let an object take taint in and hand it back.** A model says,
   for example, that `writeUTF` moves data from its argument into the stream it
   is called on. Call-argument edges run both ways, so what one call writes into
   an object, a later call on the same object can read back out. It explains the
   other 3.
3. **The suite matches method names, not call sites.** TaintBench's negatives
   name one specific line. The suite only checks that ctadl reported *some* path
   whose source method and sink method match, so a real flow somewhere else in
   the app gets charged against the negative.

Split by app: `phospy` #4 and #5 and `save_me` #26–28 come from the first
mechanism, `phospy` #3 from the second, and `proxy_samp` #16 and #20 need both —
the model puts taint on the `File` object, and the grouping carries it to the
sink.

Neither engine behaviour is a wrong rule. Each is deliberately generous, and the
same generosity buys the 43 extra true findings over the Souffle engine. The
false positives are what that costs.

Two things that are *not* causes, despite looking like good suspects:

- **Summaries computed from method bodies.** ctadl builds a summary for a method
  either by analysing its body or from a model. The body-derived kind is the bulk
  of the index — 1221 of 1382 rows in `phospy`, 17160 of 17516 in `save_me` — but
  none of it is on any of the eight paths. The methods those paths run through
  (`myService.log`, `CO.getContacts`, `DatabaseOperations.putInformation`,
  `DatabaseOperations.getInformation`) have **no summary rows at all**.
- **Missing call-site context.** Distinguishing the two call sites of
  `putInformation` would not remove `save_me`'s three, because no summary of
  `putInformation` is involved. See "save_me, worked through" below.

## How we know: turning each mechanism off

Both mechanisms can be switched off, one at a time, without changing anything
else. The table says whether each false positive is still reported.

| configuration | phospy #3 | phospy #4, #5 | save_me #26–28 | proxy_samp #16, #20 |
| --- | --- | --- | --- | --- |
| as shipped | yes | yes | yes | yes |
| no propagation models at all | yes | yes | yes | **no** |
| two-way copies off | yes | **no** | **no** | **no** |
| both off | **no** | **no** | **no** | **no** |

With both off, all three apps report zero flows of any kind. So every flow these
apps produce — true or false — rides on one of these two mechanisms, and none of
them rides on a summary computed from a method body.

Turning two-way copies off is not free. Distinct source→sink pairs reported drop
from 4 to 2 in `phospy`, 6 to 2 in `save_me`, and 9 to 4 in `proxy_samp`, and the
losses include real findings: `phospy` #2 (file bytes → `write`), `save_me`'s
`SQLiteDatabase.query → HttpClient.execute`, and all of `proxy_samp`'s
`File.<init>` positives. This is a precision/recall dial, not a bug to fix.

## Mechanism 1: copies are treated as two-way

When ctadl answers a query, it first groups variables. Any two variables joined
by a plain assignment (no field, no offset) go in the same group
(`compute_copy_alias`, `query_engine/mod.rs:337`, used by the search engine at
`query_engine/search.rs:159`). Taint on any member of a group is then given to
every other member (`query_engine/mod.rs:492-511`).

The grouping ignores direction. `x = y` puts `x` and `y` together, so taint on
`x` flows to `y` even though the program only ever copied the other way. For two
names of the same object that is reasonable — writing through one name is visible
through the other. But the same grouping also swallows two things that are *not*
aliases:

- **SSA merge points.** Where two branches meet, ctadl writes `v3 = v1` and
  `v3 = v2`. That does not mean `v1` and `v2` are the same object; it means one
  or the other arrives here. Grouping them together links values that never
  coexist.
- **Every call's result.** The dex frontend writes every call's return value into
  one per-method pseudo-register named `retval`, and every call's exception into
  `throwval` (`languages/dex/mod.rs:896-902`, `decode_call`). Most blocks also
  carry an extra edge to a catch block — 86 of `getContacts`'s 109 — so the merge
  points chain those versions together.

The result is one enormous group per method:

| method | variables in plain copies | groups | biggest group |
| --- | --- | --- | --- |
| `save_me` `CO.getContacts()` | 681 | 16 | **539 (79%)** |
| `phospy` `myService.log()` | 552 | 37 | **397 (72%)** |

This is not universal. Across all methods with at least 50 such variables, the
biggest group holds a median of 26% of them in `phospy` and 31% in `save_me`
(`proxy_samp` is small and dense: 75%). It is the *long* methods that saturate,
and both false-positive sites are long methods.

### save_me, worked through

TaintBench's negative is `SQLiteDatabase.query` (in
`DatabaseOperations.getInformation`, line 55) reaching `ContentValues.put` (in
`DatabaseOperations.putInformation`, lines 42–44). `CO.getContacts()` calls
`putInformation` twice. Here is the first call, in ctadl's own IR:

```
assign %v5 = <const: "PHONE APP">
assign %v6 = <const: "PHONE APP">
assign %v7 = <const: "PHONE APP">
java-call %v13.<DatabaseOperations.putInformation(...)>(%v13, %v5, %v6, %v7)
```

All three string arguments are constants, and ctadl reports:

```
insn 97436  labels: DatabaseData, DeviceData, LocalStorage
   DatabaseData tainted at: call-arg(97436, 2), call-arg(97436, 3)
```

Arguments 2 and 3 are `%v5` and `%v6` — the constants. The registers are reused
later in the method, but SSA gives each definition its own version, and the
version passed here is defined by the constant and by nothing else.

The group explains it. `call-arg(97436, 2)` sits in the 539-member group, and so
does `call-arg(97682, -1)`, the cursor that `getInformation` returned. The chain
of copies between them is six links long:

```
call-arg(97436,2) — %L0_3 — %L0_53 — %L2_14 — %L2_69 — %L2_49 — call-arg(97682,-1)
```

`%L0` is register `v5` and `%L2` is `retval`. The first and last links are
call-site edges: an argument vertex and the variable passed to it are the same
thing. `%L0_53` and `%L2_69` are merge points, each with two incoming copies. The
important link is `%L0_53 ← %L0_3`: the constant flows *into* the merged value,
never out of it. Taint has to travel that link **backwards** to reach the
constant, and only the two-way grouping lets it.

That is also why call-site context would not help here. The taint never passes
through `putInformation` on its way to the constant. It arrives inside
`getContacts`, from a copy chain in the same method.

Once the constant is tainted, the rest is ordinary: the call passes it into
`putInformation`, and `putInformation` calls `ContentValues.put`, which is the
sink.

### phospy, worked through

`phospy` does everything in one method, `myService.log()`. It opens a socket,
writes the IMEI to it, then streams a list of JPEGs to it:

```java
String imei = ((TelephonyManager) getSystemService("phone")).getDeviceId();  // line  92
...
DataInputStream fis = new DataInputStream(
        new BufferedInputStream(new FileInputStream(f.getPath())));          // line 106
dataOutputStream.writeUTF("\\" + imei + "\\");                               // line 113
dataOutputStream.writeUTF(f.getName());                                      // line 114
...
dataOutputStream.write(buf, 0, read);                                        // line 131
```

TaintBench's two positives are `imei → writeUTF` (#1) and file bytes → `write`
(#2). Its three negatives are the crossings: `imei → write` (#3) and file bytes →
`writeUTF` (#4, #5). ctadl reports all four combinations.

The 397-member group in `log()` contains all of them at once: the IMEI
(`call-arg(1718,-1)`, the return of `getDeviceId`), the `FileInputStream`
constructor's arguments (`call-arg(1840,0)` and `(1840,1)`), `writeUTF`'s data
argument (`call-arg(1866,1)`), and `write`'s receiver and data argument
(`call-arg(1918,0)` and `(1918,1)`). Every source in the method is therefore
copy-equivalent to every sink argument in it, and all four pairs follow.

One tempting fix does not work. The model marks these sinks on `Argument(*)`,
which includes the receiver — the stream object itself. Narrowing the port to
`Argument(1)`, the real data argument, leaves the reported source/sink pairs
**unchanged**: all four are still there. The taint is on the data arguments too,
because they are in the group.

`#4` and `#5` need the group. `#3` does not — it survives with grouping off, for
the reason in the next section.

## Mechanism 2: an object takes taint in and hands it back

A method with no body in the APK — every framework method — gets a summary only
if a model gives it one. ctadl computes summaries two ways, and only one of them
applies here:

- **From the body.** The indexer follows the method's own dataflow
  (`index_engine/mod.rs:1206`, plus the aliasing rule at `:1220`). Both rules
  build on `locals`, which grows from the assignments inside the body
  (`:1164`). A method with no body has no assignments, so it gets nothing here.
- **From a model.** A matched propagation model is turned directly into summary
  rows (`codegen/model_matches.rs:104-152`). This is the only source of summaries
  for framework methods.

If nothing models an external method, it is not summarized at all: taint that
reaches it stops there, and the query reports it as an absorbing function
(`C0007`, `query_engine/mod.rs:559`).

So every framework summary in these runs comes from a model file — the built-in
`ctadl-ascent/src/models/defaults/java-index.jsonl`, plus the app's own
`model.json`. Counting summary rows by whether the method has a body:

| app | total summary rows | on bodyless methods (from models) | on methods with bodies |
| --- | --- | --- | --- |
| `phospy` | 1382 | 161 | 1221 |
| `save_me` | 17516 | 356 | 17160 |
| `proxy_samp` | 121 | 100 | 21 |

Re-indexing with `--no-default-models` and no `-m` leaves **zero** rows on
bodyless methods, which confirms the split.

The model rows fall into four shapes:

| shape | meaning | phospy | proxy_samp | save_me |
| --- | --- | --- | --- | --- |
| `arg 0 → return` | reading from an object yields its data | 94 | 41 | 175 |
| `arg N → arg 0` | writing to an object taints the object | 53 | 42 | 139 |
| `arg N → return` | argument passes through to the result | 13 | 17 | 36 |
| `arg 0 → arg N` | e.g. `InputStream.read(buf)` fills the buffer | 1 | 0 | 6 |

Each row is fair on its own. `Cursor.getString` really does return database data,
and `DataOutputStream.write(buf,…)` really does put the buffer's bytes into the
stream. The trouble is what the first two shapes do together, and one more engine
detail makes it worse: at a call site, the argument vertex and the variable
passed to it are linked **both ways** (`index_engine/mod.rs:1189-1190`). Passing
a value in and reading it back out are the same edge.

Put those together and an object becomes a channel. Anything written into it at
one call can be read out of it at any other call on the same object, in any
order.

### phospy #3

`writeUTF` is modelled `Argument(1) → Argument(0)`: writing the IMEI taints the
stream. `write` and `writeUTF` are called on the same stream variable, and the
call-site edges run both ways, so the taint moves from `writeUTF`'s receiver to
the variable and back down into `write`'s receiver. The sink is declared on
`Argument(*)`, so the receiver counts as a sink port.

This is the one false positive that survives with grouping turned off. The
witness is the meet of two searches at the `StringBuilder.append` call that built
the IMEI string, reached forward from `getDeviceId` and backward from `write`,
entirely through model rows (`append: arg1 → arg0`, `writeUTF: arg1 → arg0`).

### proxy_samp #16 and #20

Both are sourced at `ProxyService:460`,
`File out = new File(Environment.getExternalStorageDirectory(), "ProxyLog.out")`,
and sunk at `out.write(logStr)` (line 280, #16) and
`Log.i("post", "out=" + out.exists())` (line 461, #20).

Look at what the model calls a source: `File.<init>` with port `Argument(0)`.
That marks the **`File` object itself** as sensitive, not the file's contents.
Nothing has to read the file for the taint to exist.

From there the copy group does the rest. Nothing models `File.exists`, so this is
not "ctadl thinks `exists()` returns the file's data". One of the code flows
ctadl prints for this pair is three vertices in one method, with no summary
between them:

```
source call-arg(869, 0)  in Ljava/io/File;-><init>(…)
       call-arg(902, -1) in Ljava/io/BufferedReader;->close()V
sink   call-arg(861, 0)  in Landroid/util/Log;->i(…)
```

`BufferedReader.close()` is not modelled either. The three vertices are simply in
the same group.

These two are the only false positives that need a model to exist at all — both
disappear when the index is built with `--no-default-models` and no `-m` — and
they also disappear when the groups are turned off. They need both mechanisms.

## Mechanism 3: the suite matches methods, not call sites

`proxy_samp`'s two negatives name specific lines, but the paths ctadl reports for
`File.<init> → Log.i` are spread across four different methods:

```
ProxyService$ProxyThread.run()
ProxyService.LogFile(String)
ProxyService.LogToFile()
ProxyService.onStartCommand(Intent, int, int)
```

and `File.<init> → BufferedWriter.write` is reported in `ProxyService.LogToFile()`.
The suite counts a finding when ctadl reports *a* connected path whose two
endpoint callees match, without checking that it is the same call site. So these
negatives are charged against ctadl on the strength of flows found elsewhere in
the app.

The README's "shadowed negative" rule is meant for this, but it only fires when a
*positive* finding has the same callee pair. `proxy_samp` has positives from
`File.<init>` to `HttpClient.execute`, and positives from other sources to
`Log.i`, but none from `File.<init>` to `Log.i`. The pair is not shadowed, so the
negative counts. This is the "crossed positives" case the README describes for
`phospy`, one level removed.

## The twelve the suite does not count

The same measurement limit, taken one step further, is what separates the 8 from
the 20. These twelve negatives share a callee pair with a positive that ascent
detects, so ctadl reporting the pair is not evidence it walked the negative's
call site — but it is not evidence it did not, either.

| app | findings | pair | shadowing positives (all detected) |
| --- | --- | --- | --- |
| `backflash` | #9, #10, #18 | `Intent.getStringExtra → OutputStreamWriter.write` | #5, #6, #7, #8, #17 |
| `cajino_baidu` | #11, #13, #14 | `TelephonyManager.getDeviceId → BaiduBCS.putObject` | #7 |
| `proxy_samp` | #19 | `WifiManager.getConnectionInfo → Log.i` | #7 |
| `samsapo` | #5 | `Context.getSystemService → Method.invoke` | #3 |
| `sms_send_locker_qqmagic` | #4, #5 | `SmsMessage.getDisplayMessageBody → Context.startService` | #3, #6, #7, #8 |
| `smssilience_fake_vertu` | #3, #4 | `TelephonyManager.getLine1Number → PrintWriter.write` | #1 |

Souffle is charged 9 of these 12 under the same rule. The two it escapes —
`proxy_samp` #19 and `smssilience_fake_vertu` #3–4 — it escapes by missing their
shadowing positives, not by being more precise about them. That is the general
shape: shadowing rewards an engine in proportion to how many positives it
already finds, so it flatters low-recall engines and understates the precision
of high-recall ones. Which is why the head-to-head reports 20 and 10 first, and 8
and 1 second.

Nothing here is diagnosable without call-site information. Each of the twelve is
one of three things, and the run cannot tell which:

1. ctadl reports only the positive's call site and the negative is a pure
   artifact of matching on callee methods;
2. ctadl reports both, and the negative is a real imprecision of the same kind as
   the eight above;
3. ctadl reports the negative's site and *not* the positive's — a false positive
   and a missed true flow being counted as a hit.

Distinguishing them is the same work as the fourth item below, which is why that
item is worth more than its position suggests: it would settle 12 findings on
ascent and 9 on Souffle, not just `proxy_samp`'s two.

## What would move the numbers

In rough order of value for effort:

- **Keep merge points out of the copy groups.** A merge point (`v3 = v1` where
  another branch says `v3 = v2`) is not an alias, and grouping the two branches
  together is what carries taint backwards into a constant. As a rough test of
  the idea, dropping every copy edge that links two versions of the same register
  collapses the group holding `save_me`'s tainted constant from 539 members to 2,
  and shrinks `phospy`'s big group from 397 to 121. That is a measurement of the
  copy graph, not a working fix — the index does not currently mark which
  assignments came from merge points, so the first step is to record that.
- **Give each call its own result variable.** Reusing one `retval`/`throwval`
  pseudo-register per method is what chains unrelated calls together at merge
  points. Dropping just those links shrinks the `getContacts` group from 539 to
  353 and the `log()` group from 397 to 266, so this is part of the problem but
  not all of it.
- **Sharpen the models.** `proxy_samp` marks `File.<init>`'s `Argument(0)` as the
  source, which says "this `File` object is sensitive" rather than "this file's
  contents are". Sourcing the read instead (`FileInputStream` content, or
  `File.getAbsolutePath` if the path is what matters) is the more faithful model
  and starts the taint later. It is not on its own a fix for #20: whatever is
  tainted in that method still spreads through the group. Nine of the 38 app
  models declare at least one source on `Argument(0)`, so this is a sweep, not a
  one-line change.
- **Report call sites, not just callees.** Mechanism 3 is a *measurement*
  problem, not an analysis problem. If `C0001` carried the source and sink call
  sites and the suite matched on them, `proxy_samp` #16 and #20 would very likely
  stop counting, at no cost to any true positive — and, more than that, the
  shadowed-negative rule could be retired entirely, resolving the twelve findings
  in the section above (and Souffle's nine) into hits or misses instead of
  unknowns. It would also make the positive counts stricter, since a positive
  currently credited on any path with the right callee pair would have to be
  found at its own line. The README already flags DEX SARIF's missing line
  information as the blocker; byte offsets are present and the DEX line map is
  readable (xtask already reads it for other purposes).
- **Do not bother with call-site context here.** It is the classic fix for "in at
  one call site, out at another", but that is not what these apps do. No summary
  of `putInformation` exists, so no amount of context around its two call sites
  changes `save_me`'s three findings. `hybrid-inlining-plateau.md` is the record
  of what context costs in this engine.

## Reproducing this

The three apps were run outside the sandbox, with a `cargo build --release`
binary of the tree at `8baae049` and the same APKs the Nix check fetches:

```sh
export PATH=$PWD/target/release:$PATH
export XDG_STATE_HOME=$PWD/state
ctadl import -l apk -n phospy_tb /nix/store/…-phospy.apk
ctadl index phospy_tb -m taintbench/apps/phospy/model.json
ctadl query phospy_tb -m taintbench/apps/phospy/model.json \
      -o results.sarif --sarif-profile debug
```

The two ablations in the table above:

```sh
# no propagation models: every summary is computed from a method body
ctadl index phospy_nm phospy_tb --no-default-models
ctadl query phospy_nm -m taintbench/apps/phospy/model.json -o nm.sarif

# two-way copies off: this is not a flag. `compute_copy_alias`
# (query_engine/mod.rs:337) was patched to `return Vec::new()` for the run and
# the patch reverted afterwards. Only `query` needs re-running; the index is
# unaffected.
```

`--sarif-profile debug` adds the `C0002.tainted-instruction` results that the
per-instruction listings above come from. The copy groups were rebuilt outside
ctadl by running a union-find over the empty-path rows of `assign.parquet`,
which is the same thing `compute_copy_alias` does. The index is duckdb-readable
(see `docs/debugging.md`):

```sh
cd state/ctadl/projects/phospy_tb/index && duckdb
select * from read_parquet('summary.parquet');            -- method summaries
select * from read_parquet('external_function.parquet');  -- methods with no body
select * from read_parquet('assign.parquet');             -- edges within a method
```

One thing to know before reading a SARIF as ground truth: the `codeFlows`
attached to a `C0001` result are not always the witness for that result's own
source/sink pair. All 12 of `phospy`'s `FileInputStream → writeUTF` results carry
a code flow that starts at `getDeviceId`. The `taintVertices` property is the
reliable part — it names the vertices where the forward and backward searches
actually met.
