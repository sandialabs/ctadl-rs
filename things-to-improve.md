# Things to improve - DO-NOT-MERGE

The 50 APKs of the corpus are in `/Volumes/Shampoo/ct-bigapk/apks/`: the original 15, and 35 more
small apps with native code (see "M7. Small apps with native code"). The measurements are under
`/Volumes/Shampoo/ct-bigapk/small/`. M1 indexes `androidudpbus` and two other apps whole; M2-M6
index the Dex-only imports in `small/r8-general/`; M7-M8 index whole APKs again.

The document has four parts:

- **Problems** (P1-P23): each problem found, what causes it and where it shows up.
- **Solutions** (S1-S28): each fix tried or proposed, which problems it applies to, and whether it
  is done, rejected or undecided.
- **Measurements** (M1-M8): the investigations, in the order they were done, with their tables.
- **Data**: where every investigation's output is, and the experiment switches.

## Status at a glance

| Problem | Solutions | Status |
|---|---|---|
| P1. The stack is a mixing pool | S3 done, S4 | Partly fixed: 5x over budget remains |
| P2. Unpruned SSA | S1 superseded, S2 done | Fixed |
| P3. Live phi operands at interpreter loops | S5 | Open |
| P4. Recursive parsers give cross-product summaries | S6 done, S7, S8 | Fixed for the demangler; open for BoringSSL |
| P5. Native function pointers mint decisions | S9 | Open |
| P6. Wild relations | S27 | Open |
| P7. Summary growth | none | Open, cause unknown |
| P8. Large native libraries fail at import | none | Open |
| P9. R8-merged Kotlin lambdas are dispatch hubs | S10 done, S15 rejected | Fixed |
| P10. Reused parameter registers written back to the formals | S16 done | Fixed for JVM bytecode |
| P11. The C front end treats a pointer as its pointee | S17 | Open |
| P12. Call-target tags skip casts and type tests | S18, S19, S21 | Open; precision only |
| P13. Receiver tags reach CHA overrides they can't dispatch to | S18, S19, S20, S21 | Open; precision only |
| P14. Computed values carry call-target tags | S18, S19, S21, S22 | Open; precision only |
| P15. Decision-set churn in `context_locals` | S14 rejected, S15 rejected | Mitigated by S10 and S16 |
| P16. The unkeyed call-target join | S11 done | Fixed, but S11 causes P17 |
| P17. Call-target tags are stored many times | S12, S13 | Open; S12 measured |
| P18. `reach_vp` is stored three times | S24 | Open |
| P19. `actual_param` outlives its only use | S25 | Open |
| P20. Memory held around the fixpoint | S26 | Open, not investigated |
| P21. Joins that scan a whole relation every iteration | S27 | Open |
| P22. The regression suite hid a lost flow | S28 done | Fixed |
| P23. Nondeterminism | none | Open |

# Problems

Each problem lists the apps where it shows up and the solutions that apply to it. The measurements
behind it are in the M section named.

## Native code, IR and SSA

### P1. Stack slots are not in SSA, so the stack is a mixing pool (`locals` volume)

**Seen in:** `androidudpbus` (`libudphub.so`, Rust/tokio), `glxy`, `retrodrawing` and `halma`
(`libgdx`), `avifview`, `fir.tube`, `a2050`. **Measured in:** M1, M7. **Solutions:** S3 (done),
S4.

- **Every stack access goes through one SSA variable**, `%L3_0`, the stack pointer at entry. In
  `androidudpbus`'s `FUN_00144f04`, an async state machine with 803 blocks, there are 55,775
  stores into 255 slots and 105,050 loads from 285 slots. A typical slot receives about 300
  distinct values and is read by about 590 loads.
- **Each load inherits every value ever stored to its slot**, and loaded values are stored again
  into other slots. The stack becomes a function-wide mixing pool, so every variable in the
  function ends up reached from everything.
- **So `locals` rows ≈ (variables in the function) × (all sources).** `FUN_00144f04` holds 86-92%
  of `locals`. Almost every variable in it is reached from about the same 1-3k sources: paths of
  formal 0 plus paths into global memory.
- **The symptom is volume, not cost per row.** Rule costs are normal, about 0.2 µs per tuple. In
  the small apps of this kind, the four `locals` rules take 80-95% of rule time at 0.01-0.3 µs per
  row, flat across the ladder; the cost is the number of rows (250-530 M). The three `libgdx` apps,
  two different builds of the library, all peak at 20-23 GB, whatever the rest of the app is.
- **What's left after mem2reg (S3) is the escaped slots.** The slots that stay in memory are the
  heavily used ones. In `FUN_00144f04`, 23k stack accesses stay in memory across 35 slots; the
  busiest take about 300 stores and 580 loads each, the same profile as before promotion. In
  `glxy`'s `FUN_0010b408`, three kept slots (-35104, -35080, -35072) take about 765 stores and
  1,550 loads each.
- **Ghidra can't bound an escaped region.** At the escaped offsets in `FUN_00144f04`, Ghidra's
  high variables are 4-8 bytes (`local_5a0` is 8 bytes). It did not recover the Rust structs
  behind them.
- **Where escaped addresses go.** In `androidudpbus`, 1,714 frame addresses are passed to calls.
  Of those, 1,697 go to functions defined in the library (132 of them to `memcpy`) and 17 through
  function pointers. Calls are only 28% of all escapes, though; the rest are frame addresses
  stored to memory (3,118) or copied (1,291) inside the function.

### P2. Unpruned SSA

**Seen in:** `androidudpbus`. **Measured in:** M1. **Solutions:** S1 (superseded), S2 (done).

`ctadl-ir/src/ssa/mod.rs` placed phis with minimal Cytron SSA and no liveness pruning.
`libudphub` got 5.19 M phis, of which 1.1% were live. The phis took the statement count from 880k
to 5.96 M and the assign edges to 14.6 M.

### P3. Live phi operands at interpreter loops

**Seen in:** `reinstead`. **Measured in:** M7. **Solutions:** S5.

`FUN_00142330` in `reinstead`'s `libmain.so` (1,027 blocks, 165 k statements) gets 658,838 phis,
all live, with 47.4 M operands: about 640 phis per block and 72 operands per phi. It is likely the
embedded Lua interpreter loop. Pruning (S2) can't remove them. They become 47.7 M `copy_edge`
rows, 1.04 assignments per byte of IR against 0.02-0.03 in every other app, and the index enters
the fixpoint at 9 GB. At 640 s `locals` holds 509 M rows and `ext_dst` 82 M.

### P4. Recursive parsers give cross-product summaries (the edge-delta join)

**Seen in:** the 16 apps that link libc++abi's demangler, worst in `termux.nix`, `dictionary.fork`,
`scrcpy`, `androidcrypt`, `untracker`; also `openarcade`, `aiyo`, `freezeyou`. Without the
demangler: BoringSSL in `conscryptprovider` and `scrcpy`. **Measured in:** M7, M8.
**Solutions:** S6 (done for the demangler), S7, S8.

- **The rule whose cost per row grows.** In
  `locals(f, v1, p1, a, p43) <-- edge_split(f, v2, key, rest, dst), locals(f, v2, key, a, p4)`,
  the half driven by the `edge_split` delta costs 8-25x the half driven by the `locals` delta.
  Edges that arrive late each enumerate every `locals` row at their source and rederive rows that
  already exist. In the `locals`-volume apps (P1) the two halves cost the same.
- **The late edges are summaries instantiated at static call sites**, not call-target
  resolution: 99.97% of the pairs in `termux.nix`.
- **The summaries are near cross products.** A recursive-descent parser over a state struct gets
  a summary where most of a formal's paths flow to most of another's, instantiated at many
  recursive call sites. In `termux.nix`'s `FUN_001afefc` (libc++abi's demangler, statically linked
  into `liblocal-socket.so`), the summary's 2,223 source paths are all under arg 0, the parser
  (`.deref.[k]`, `.deref.[k].deref`, `.deref.deref.[k].deref.[k]`, ...): every offset at which
  the parser reads its input or its arena. `.deref` and `.[8].deref` are its `First` and `Last`
  cursors. Of its 668 destination paths, 47
  take 2,100-2,304 of those sources each, 83% of the rows. Most of those destinations arrive late,
  on the return value (the node the parse function builds), and each brings its 2,100 sources at
  once, at every call site.
- **Each edge enumerates the same rows.** At a call site, the 93,623 instantiated edges whose
  source starts `.deref` all split at `call-arg(i, 0).deref`, where `locals` holds 2,100 rows: 197 M
  pairs per site. At most 0.06% of the pairs yield a new row.
- **The shape survives without the demangler.** In `conscryptprovider`, BoringSSL's ASN.1 template
  decoder (a recursive cycle `FUN_0020c454 -> FUN_0020cd4c -> FUN_0020d8d0 -> FUN_0020c454`,
  between `ASN1_item_d2i` and `ASN1_item_i2d`) has summaries of 1.8-4.0 M rows and 35 B pairs. In
  `scrcpy`, it is BoringSSL's TLS parsing (`SSL_parse_client_hello`, `CBS_get_*_length_prefixed`,
  called from 37-47 sites each). `scrcpy` also has an unnamed range after `__emutls_get_address`,
  entered from `__gxx_personality_v0` and `__cxa_throw`; it looks like a stripped libunwind, which
  the unwinder model can't match by name.

### P5. Native function pointers mint decisions

**Seen in:** `AnarchRE` (SDL3), `heartratemonitor` (SQLite). **Measured in:** M7.
**Solutions:** S9.

The context machinery of P9 and P15, on p-code: an indirect call through a driver or VFS table
resolves to every function stored into such a table.

- `AnarchRE`: 17,500 decisions, 16,359 of them in `SDL_EnterAppMainCallbacks` (280 k tags, 40 B
  `call_target_assign_like` pairs per full re-derivation). After 640 s, `context_locals` goes from
  0.89 M to 8.4 M rows at 60.6 µs per new row. `establishes_via` holds 66.7 M rows in 10.1 GB:
  `establishes_via_indices_0` has 63 keys and 2.25 GB, and `establishes_via_indices_none` is
  another full copy at 1.61 GB.
- `heartratemonitor`: at 80 s, 5,259 decisions, 17.2 M `context_locals` rows with 637 M
  memberships, 29 M set unions and 42.9 M `cta_key` rows. The hubs are `FUN_0016e150` (1,495
  blocks, likely `sqlite3VdbeExec`; 1.64 M tags) and `sqlite3_open_v2` (decision sets of 92).
  Memory doubles every 20-40 s.

The mechanism is not confirmed: nobody has yet checked which indirect call sites mint these
decisions.

### P6. Wild relations

**Seen in:** `conscryptprovider`, `a2050`. **Measured in:** M7. **Solutions:** S27 (for Java only).

The offset-keyed relations of the wildcard match take most of Ascent's default containers. In
`conscryptprovider`, `edge_split_wild` (46 M rows, 7.6 GB) and `assign_wild` (26 M, 3.8 GB) are
11.4 of 13.3 GB; in `a2050`, the wild relations and `ext_fml` are 8.9 of 10.7 GB. On
`conscryptprovider` the `ext_dst` rule driven by the `assign_wild` delta rises from 0.3-0.9 to 15,
24 and 45 µs per new row. Both apps also have P4 (49% and 40% of rule time in the edge-delta
rule).

### P7. Summary growth

**Seen in:** `bined` (82% Java). **Measured in:** M7. **Solutions:** none yet.

`summary` grows from 0.5 M to 5.8 M rows, `assign_like` from 1.5 M to 14 M and `edge_split` from
3.2 M to 49 M, each at a flat cost per row. The cause was not investigated.

### P8. Large native libraries fail at import

**Seen in:** the six large APKs of the original 15. **Measured in:** M7. **Solutions:** none yet;
the corpus was extended with small apps instead.

`darkcoin`'s import hit a 50 GiB cap. Ghidra finished `libsdklib.so` (38 MB) in 30 minutes and
wrote 7.8 GB of compressed facts, and ctadl's lowering of those facts passed 50 GiB 10 minutes
later. Lowering costs about 11x the facts (`udpbus`: 108 MB of facts, a 1.2 GB peak), and each of
the other five has a larger library, from 45 MB (`greenbits`) to 156 MB (`libxul` in `ceno`).

## Java: call targets and contexts

### P9. R8-merged Kotlin lambdas are dispatch hubs

**Seen in:** `cpuinfo`, `ceno`; merged classes are in 12 of 15 apps. **Measured in:** M2, M3.
**Solutions:** S10 (done), S15 (rejected).

The functions that hold the most `context_locals` rows in `cpuinfo` are all R8-merged Kotlin
suspend lambdas: `Li;->r` and `Li;->e`, `Lp8;->r`, `Lk;->r`, `Lt40;->r`. (`nv2` is
`SuspendLambda`, `vk` is `BaseContinuationImpl`, `yp0` and `up0` are `Function2` and `Function1`.)

- **Many lambdas share one method.** `Li;` merges 29 lambdas behind a synthetic `int i` class
  id: `r` (`invokeSuspend`) and `e` (`invoke`) switch on it, and the captured state of all 29 is
  in the untyped `Object` fields `k` and `l`. The analysis does not see the switch, so `r` is the
  union of all 29 bodies, and the virtual calls in it, on casts of `this.l`, dispatch on
  whatever any of the 62 construction sites stored there. At 320 s `Li;->r` had 1,734 decisions,
  7,165 distinct decision sets, a largest set of 318 and 726 M memberships.
- **The union is instantiated 30 times over.** Each of `Li;->e`'s 30 switch cases calls
  `this.p(..).r(Unit)`, so `r`'s union summary lands at 30 identical call sites. Each site's
  receiver vertex holds 3,129 call-target tags and has 20,181 out-edges: 63 M pairs per vertex,
  1.9 B of the 3.2 B pairs that a full re-derivation of `call_target_assign_like` would visit.
- **75 `SuspendLambda` subclasses carry a class id**, so the pattern recurs across the app.
- **It drives P15 and P16.** The union summaries are what make the decision sets churn and the
  call-target join visit billions of pairs.

### P10. Reused parameter registers written back to the formals

**Seen in:** `greenbits` (blows up), and every Dex app (M5 shows it slowed the ones that
finished). **Measured in:** M4, M5. **Solutions:** S16 (done).

R8 reuses parameter registers, and SSA's exit param-flow wrote each parameter's *exit* version
back to its formal. `UTF8JsonGenerator.writeBinary(Base64Variant, byte[], int, int)` ends with

```smali
    iget-object p1, p0, ...->_outputBuffer:[B   # p1, the Base64Variant, := this._outputBuffer
    iget-byte   p0, p0, ...->_quoteChar:B       # p0, this, := this._quoteChar
    aput-byte   p0, p1, p2
    return-void
```

so the write-back `formal(0) <- @p0_2 <- @p0_1 <- @p0_0._quoteChar` gave the summaries
`this <- this._quoteChar` and `variant <- this._outputBuffer`. A caller cannot see its argument
rebound, so both are impossible. They produced call-target decisions like this:

1. `JsonGenerator.writeBinary(byte[], int, int)` calls `this.writeBinary(variant, data, off,
   len)`, which resolves to `UTF8JsonGenerator`, `WriterBasedJsonGenerator` and `TokenBuffer`.
   The UTF8 summary lands on the call-arg vertex as `arg0 <- arg0._quoteChar`, and the
   formal-side `locals` rule gives `locals(arg0, ε, 0, ._quoteChar)`.
2. `TokenBuffer.writeBinary` has a genuine `critical_summary` at `(0, ε)`, since its `this`
   reaches virtual calls. Rule 1.2 joins it with the `locals` row from step 1, at the same
   call-arg, and derives `critical_summary(JsonGenerator.writeBinary, 0, ._quoteChar)`.
3. Rule 1.2 carries it up through `ByteArraySerializer`, `_serialize` and
   `ObjectWriter$Prefetch.serialize`, and rules 2.1/2.2 mint one decision per serializer class
   that reaches the byte.

Traced with the probe's `CTADL_FOCUS` dump (see "Data"). `Prefetch.serialize` got about 23 paths x
37 serializer classes = 847 decisions. Most decisions in `greenbits` were provably impossible
(53-60% of them were a class that is not a subtype of the formal's declared type; see M4), and
both the decisions and the call-target tags behind them blew up memory.

The chess app had the same artifact: `Lt2/l;->F` reuses its `String` parameter's register for
`getMyMove()`'s result, and the exit write-back fed that into its own incoming `String`, giving a
`getMyMove -> StringBuilder.append` flow into `F`.

### P11. The C front end treats a pointer as its pointee

**Seen in:** the C regression family (`C:outparam`). **Solutions:** S17.

The front end lowers `&v` to `v`, a read of `*p` to `p`, and a store `*out = source()` to
`assign @p0 = %t0` (`flatten_expr` and the `pointer_expression` arm of `flatten_lvalue` in
`frontends/ctadl-c/src/lib.rs`). Only an interior address (`p = &x[1]`) or a same-block `p = &x`
alias gets a real target, with the `deref` field. An out-parameter therefore reaches the caller
only through the exit-version write-back, so C has to keep `Exit` (P10's fix doesn't apply), and
applying `Entry` to C lost `C:outparam`'s only flow. It also gives C a false flow of the same kind
as R8's: a function that rebinds a pointer parameter (`p = p->next; *p = x`) writes `x` back to
the caller's pointer.

### P12. Call-target tags skip casts and type tests

**Seen in:** every Dex app with more than a few hundred decisions. **Measured in:** M4, M5.
**Solutions:** S18, S19, S21.

After S16, 3-5% of decisions are still impossible. None loses a flow; each adds a context the
program can't reach, and the decisions that context passes down. Those can produce false flows,
and they raise the cost. Casts and type tests are the root of most of them: 888 of `greenbits`' 1,324, and 81% across the corpus
(caller casts 34%, plus "other" 47%, which is mostly type tests in callees). A `check-cast` passes
the value through unchanged, so the tag follows it.

- **In the caller.** `DeserializerCache._createDeserializer2` does `instance-of`/`check-cast
  CollectionType` on a `JavaType`, then calls `createCollectionDeserializer`. Its `MapType` and
  `SimpleType` tags go through anyway. Kotlin's `as?` and Java's `if (x instanceof T) ((T) x)` do
  the same. Other examples: `(RingBuffer) L$1` in `windowedIterator`, where a coroutine spill slot
  holds an `ArrayList` in another state, and Jackson's `(ContainerSerializer) ser`. Of the 26
  receiver decisions in `greenbits` whose call site admits no target, 24 are this kind too.
- **In a callee, through its return value.** Kotlin's type checker: `lowerBoundIfFlexible(x)`
  returns `asRigidType(x)`, which is `x.unwrap()` behind `instance-of`/`check-cast SimpleType`.
  The analysis has `ret <- x`, so `FlexibleTypeImpl` and `RawTypeImpl` reach `RigidTypeMarker`
  formals. In the big crypto wallets it is almost all the Bouncy Castle and Spongy Castle ASN.1
  idiom `X.getInstance(Object o)`: return `(X) o` if `o instanceof X`, otherwise build a new `X`.
  So `CVCertificateRequest.getInstance(o)` hands `DERApplicationSpecific.getInstance(o)`'s result
  to `<init>(DERApplicationSpecific)`, and that result is `o` behind an
  `instance-of`/`check-cast`. This is the same type test in the callee as Kotlin's `asRigidType`.

### P13. Receiver tags reach CHA overrides they can't dispatch to

**Seen in:** every Dex app; the dominant cause in small apps (70-89% of impossible decisions).
**Measured in:** M4, M5. **Solutions:** S18, S19, S20, S21.

Every view model's constructor in `greenbits` calls `invoke-virtual {p0},
GreenViewModel;->bootstrap()`. CHA resolves that site to three `bootstrap`s. Rules 2.1 and 2.2
join `call(caller, insn, f)`, which lists every CHA target, with the tag at the argument, and
never test that the tag dispatches to `f`. So `WalletBalanceViewModel.bootstrap` gets 68 sibling
view models as `this`. Rule 3.1 does test dispatch, with `callee_resolvents(obj, key, f)`, but
only for sites in `callee_info`; the bootstrap site (insn 1929437) has three `call` rows and no
`callee_info` row, so there is no key to test against. This is the one cause that could grow
quadratically: a hub method with many overrides, called on `this` from many subclasses, gets
subclasses × overrides decisions. 18% of impossible-decision roots across the corpus.

### P14. Computed values carry call-target tags

**Seen in:** `greenbits`, `cash.p.terminal`. **Measured in:** M4, M5. **Solutions:** S18, S19,
S21, S22.

`locals` and `assign_like` are value-flow relations: a value computed from `x` counts as coming
from `x`. Rule 1.1 asks `locals` which formal paths reach the receiver of a critical call, and
`call_target_assign_like` walks `assign_like`, so object classes reach paths that only hold
values computed from the object. `UtilsKt.accept` passes `contentType.toString()` to
`append(String, String)`, and the `String` carries the `ContentType` tag. In `cash.p.terminal`,
Jackson's `TypeFactory` computes a `JavaType` from a `java.lang.reflect.Type`, and it carries the
`Type`'s tag. 54 of `greenbits`' 1,324 remaining impossible decisions; across the corpus it is
mixed into "other" with P12's callee type tests, split from them by package name only.

Before P10 was found, this was the suspected cause of `greenbits`. In
`DefaultSerializerProvider._serialize` the only critical site is `p3.serialize(..)`, but
also lists `arg1`, the value being serialized, since Jackson
looks the serializer up by `value.getClass()`, and in `ObjectWriter$Prefetch.serialize` the
critical paths included `gen._quoteChar` (a `byte`), `gen._outputTail` (an `int`),
`HEX_BYTES_UPPER[]` and a `$SwitchMap` `int[]`. Those turned out to be P10.

### P15. Decision-set churn in `context_locals`

**Seen in:** `cpuinfo`, `greenbits` (both before their fixes). **Measured in:** M2, M4.
**Solutions:** S14 (rejected), S15 (rejected); P9's and P10's fixes remove what drives it.

`context_locals` is a lattice relation, and its decision sets grow one decision at a time. In
`cpuinfo`, 221 M unions grew a set for 39 M rows, about 6 updates per row, and every update sends
the row through the whole contextual closure again: 1.2 µs per new row at 10-20 s, 26.8 µs at
320-640 s, 64% of rule time at 640 s. `set_establishes_via` grows the same way (0.8 M, 3.8 M,
13.1 M at 160, 320, 640 s), since every grown set is unfolded again in full. In `greenbits` the
cost per new row went 1.4, 1.6, 6.2 and 9.2 µs from 20→40 s to 160→320 s, and `context_assign`
cost 14.6 µs per new row.

### P16. The unkeyed call-target join

**Seen in:** `cpuinfo`. **Measured in:** M2, M3, M6. **Solutions:** S11 (done).

`call_target_assign_like`'s transitive rule joins on `(f, v2)` and only then tests
`substitute_prefix`. In `cpuinfo` at 160 s it visited 2.04 B pairs; 1.6% matched the prefix and
0.9% were program paths. This is the wasted join that the `locals` rules' split keys were built
to avoid (see the comment above `reach_vp`): 0.9 µs per new row, rising to 87.7 µs. Elsewhere the
join is not mostly wasted: 34% of pairs match on `darkcoin`, 79% on `cash.p.terminal`.

## Memory and time constants

### P17. Call-target tags are stored many times

**Seen in:** `greenbits`, `cash.p.terminal`, `darkcoin`, `heartratemonitor`. **Measured in:** M3,
M4, M6. **Solutions:** S12, S13.

`call_target_assign_like` is stored five times: the row store plus four indices, one of which
repeats the whole row. S11's `cta_key` is stored three times. At `greenbits`' 320 s rung the two
held 17.0 GB, and accounted for 17 of the 30 GB the footprint grew from 10 s to 320 s. In BYODS
tries, `locals` and `assign_like` take 37 and 67 B/row; these take 294 and 192 B/row. `cta_key`
is a net memory loss on `cash.p.terminal` (4 GB), `darkcoin` (44 M rows, 18.0 → 27.2 GB) and
`greenbits` (8 GB), and saves no time on any of them.

### P18. `reach_vp` is stored three times

**Seen in:** `cash.p.terminal`, and every large app with native code. **Measured in:** M6, M7.
**Solutions:** S24.

`reach_vp` is a projection of `locals` that drives `locals_key`, `locals_key_wild` and
`locals_wild` through its delta. It is stored three times, once as `_indices_none`, a full copy:
2.34 GB on `cash.p.terminal`. `reach_vp <-- locals delta` also gets more expensive per row on
every large native app (`glxy` 6.7 to 25 µs, `conscryptprovider` 0.5 to 14.8 µs), since it visits
every new `locals` row to find a few new `(f, v, p)`.

### P19. `actual_param` outlives its only use

**Seen in:** `cash.p.terminal`. **Measured in:** M6. **Solutions:** S25.

Only scc 1's call-arg rule reads `actual_param`, but it is held through scc 4: 1.56 GB on
`cash.p.terminal`, 13.8 M rows, about 10 per call site (arguments, returns and the globals slot).

### P20. Memory held around the fixpoint

**Seen in:** `cash.p.terminal`. **Measured in:** M6. **Solutions:** S26.

7.5-8 GB (facts, source info and interners) is held before the fixpoint starts, and 6.9-8.7 GB is
still held after the index is saved and dropped. Loading the IR takes 3.8 GB, and SSA raises that
to 5.2 GB. `facts.try_save` adds 2.3 GB (4.9 → 7.2 GB) that is never given back.

### P21. Joins that scan a whole relation every iteration

**Seen in:** `cash.p.terminal`. **Measured in:** M6. **Solutions:** S27.

- **Scanning joins cost 25-39 s.** `critical_summary` rule 1.2, `context_assign`,
  `establishes_direct` and `critical_reach`. Rule 1.2 shows the pattern: Ascent drives it from
  `call_indices_2_total` and `critical_summary_indices_0_total`, and probes the `locals` delta
  last. So every one of the 637 iterations walks the 2.9 M `call` rows to find a few new rows.
  These rules get more expensive per row on almost no rows: `critical_summary` 0.8, 26 and
  2,277 µs, `establishes_direct` 2.5, 14 and 3,436 µs, `context_assign` 39, 99 and 875 µs.
- **The wildcard rules cost about 9 s and produce nothing on Java.** `assign_wild`,
  `edge_split_wild`, `locals_wild` and `locals_key_wild` scan every `assign_like` or `reach_vp`
  delta, but Java paths have no trailing offsets.

## Tooling

### P22. The regression suite hid a lost flow

**Measured in:** M1. **Solutions:** S28 (done).

With mem2reg's `exact` policy, `nightly/tests/c/example.c` lost its only flow, but the suite still
reported "49 passed, 0 failed": `example` showed up as a SKIP ("no tainted instructions on Darwin;
skipping strict offset check").

### P23. Nondeterminism

**Measured in:** M3, M6. **Solutions:** none yet.

- The decision-set relations (`set_*`, `context_summary_set`) differ between two runs of the same
  binary.
- The pcode import is not deterministic. One run's import of `FUN_00105b18` in
  `libcpuinfo-libs.so` had one assignment and two locals in a different order, which changed 6
  `assign_like` edges.

# Solutions

| Solution | Problems | Status |
|---|---|---|
| S1. Dead-phi pass | P2 | Superseded by S2 |
| S2. Pruned SSA | P2 | Done (`2b90a98b`) |
| S3. Stack-slot promotion (mem2reg), `window:8` | P1 | Done (`31594e08`) |
| S4. Promote escaped slots and spill around escapes | P1 | Undecided, not started |
| S5. Bound phi operands at interpreter loops | P3 | Undecided, needs investigation |
| S6. Model `__cxa_demangle`, skip its exclusive callees | P4 | Done (`fc776aae`) |
| S7. Collapsed summary rows | P4 | Undecided |
| S8. Split `edge_split` on admissible extensions | P4 | Undecided, needs a measurement |
| S9. Confirm the function-pointer mechanism | P5 | Undecided, investigation |
| S10. Split R8 class-merged classes at import | P9 | Done (`a7d716e8`) |
| S11. Key the call-target transitive rule (`cta_key`) | P16 | Done (`76982fc4`); causes P17 |
| S12. Turn off `cta_key` except where it pays | P17 | Undecided; measured −4 GB |
| S13. Store call-target tags compactly | P17 | Undecided |
| S14. Bound decision-set churn | P15 | Rejected |
| S15. Change the hybrid context mode | P9, P15 | Rejected |
| S16. Write back entry versions for JVM bytecode | P10 | Done (`8cea6bca`, `b6503b05`) |
| S17. Lower C pointer accesses to `.deref`, then use `Entry` for C | P11 | Undecided |
| S18. Filter call-target tags by static type | P12, P13, P14 | Undecided |
| S19. Check decisions against the formal's declared type | P12, P13, P14 | Undecided |
| S20. Check receiver dispatch at CHA sites | P13 | Undecided |
| S21. Only mint decisions that can resolve a critical call | P12, P13, P14 | Undecided |
| S22. Build critical summaries from identity flows only | P14 | Undecided |
| S23. A Jackson model | P10, P14 | Rejected |
| S24. Fold `reach_vp` into its consumers | P18 | Undecided; est. −2.3 GB |
| S25. Free `actual_param` after scc 1 | P19 | Undecided; est. −1.6 GB |
| S26. Find the memory held around the fixpoint | P20 | Undecided, not investigated |
| S27. Reorder scanning joins; skip wildcard rules without offsets | P21, P6 | Undecided |
| S28. Make the suite fail on a lost flow | P22 | Done (`31594e08`) |

### S1. Dead-phi pass

**Applies to:** P2. **Status:** superseded by S2.

An experimental pass (`CTADL_DEAD_PHIS=1`, in WIP commit `fd5130f4`) cut `androidudpbus`'s assign
edges from 14.6 M to 1.09 M. The index then finished, in 875 s at 49.8 GB: still about 18x over
budget, because of P1.

### S2. Pruned SSA

**Applies to:** P2. **Status:** done (`2b90a98b`).

`androidudpbus` finishes in 849 s at 52.0 GB, about 18x over budget. P1 remains.

### S3. Stack-slot promotion (mem2reg), `window:8` by default

**Applies to:** P1. **Status:** done (`31594e08`). Reduces P1; `androidudpbus` is still 5x over
budget.

The pass is `ctadl-ir/src/ssa/mem2reg/`. It runs after coalescing and before SSA. It gives each
`__stack_top.[k].deref` slot its own local (`__stack_m16` for offset -16) and rewrites the slot's
loads and stores into copies, so SSA versions the slot like any other local.
`Pipeline::index_default` runs it with `EscapePolicy::Window(8)`. The policy measurements in M1
used the since-removed `CTADL_MEM2REG=exact|window:N|above` switch.

**Promotion keeps today's behavior except through escaped addresses.** The analysis matches
offsets exactly and adds adjacent offsets together, so distinct offsets are already distinct
locations; overlapping accesses of different widths behave as before. What promotion can lose is
a flow through an escaped frame address. An address `__stack_top.[j]` escapes when it is passed
to a call, stored, copied or returned; the escaped address plus an offset `d` reaches slot `j+d`.
The escape policy decides how far an escaped address is assumed to reach, and those slots stay
in memory:

- `exact`: only slot `j`. **Rejected:** it loses real flows (`example.c`, one `structret`
  result); see M1.
- `window:N`: slots `j .. j+N`. **`window:8` chosen**: it loses nothing on the suite and is the
  fastest of the safe policies. `window:16` and `window:64` also lose nothing but are slower and
  bigger.
- `above`: every slot at or above `j`. This is the only policy that can't lose a flow, but it
  promotes almost nothing (0.1% of accesses), because every large function has an escape near
  the bottom of its frame. **Rejected.**

### S4. Promote escaped slots and spill them around their escapes

**Applies to:** P1. **Status:** undecided, not started.

The kept slots are the heavily used ones, and their addresses really do escape, so a tighter
window may not free them. Instead, promote an escaped slot anyway and spill it around its
escapes: store the local to the slot before a call or escape, and reload it after a call. Direct
accesses between escapes would then be in SSA, and only the values live at an escape would mix.

### S5. Bound phi operands at interpreter loops

**Applies to:** P3. **Status:** undecided; investigate first.

`FUN_00142330` is one function. First check whether its live variables are promoted stack slots
(S3) or registers.

### S6. Model `__cxa_demangle` and skip the functions only it reaches

**Applies to:** P4. **Status:** done (`fc776aae`) for the demangler. P4 remains for BoringSSL.

`__cxa_demangle` now has a default model (`native-index.jsonl`). It has the propagation
`Arg0.deref -> Ret.deref`, `Arg0.deref -> Arg1.deref` and `Arg1.deref -> Ret.deref`, and the modes
`skip-analysis` and the new `skip-exclusive-callees`. The new mode also skips every function that
only the matched one reaches (`codegen::exclusive_callees`), computed per import from direct
calls. A skipped function must have a Ghidra default name (`FUN_…`), since a function with a
symbol could be called from another library, and must not be address-taken. The demangler's parse
functions are mutually recursive, so the set is a greatest fixpoint: start from everything
reachable, then drop any function with a caller outside the set.

The demangler is libc++abi's own code, linked into any NDK library built with `c++_static`, and it
carries no app data flow worth tracking. It skips 32-106 bodies per app; every one is a `FUN_`
between `__cxa_demangle` and `operator new`. `termux.nix` goes from 812 s to 10 s. See M8.

### S7. Collapsed summary rows

**Applies to:** P4. **Status:** undecided; S6 showed it is needed (P4 survives in BoringSSL).

A collapsed summary row ("everything under arg 0 reaches `ret.P`") would bound the cross-product
shape without knowing the library, but it needs a path representation for "everything under".

### S8. Split `edge_split` on the admissible extensions of `rest`

**Applies to:** P4. **Status:** undecided; measure first.

If most of the 99.9% of pairs that yield nothing fail the `concat` admissibility test, rather than
rederiving existing rows, then splitting `edge_split` on the admissible extensions of `rest` would
cut the enumeration without changing the result. Which of the two it is was not measured.

### S9. Confirm the function-pointer mechanism

**Applies to:** P5. **Status:** undecided; investigation before any fix.

Dump the decisions of `AnarchRE` and `heartratemonitor` and check which indirect call sites mint
them.

### S10. Split R8 class-merged classes at import

**Applies to:** P9. **Status:** done (`a7d716e8`). Fixes `cpuinfo`.

`ctadl-dex/src/merged.rs` finds a `final int` field that each constructor stores from an argument
or a constant, and that methods load from `this` and switch on. It uses a must-analysis of
register values, so `move-object v4, p0` and a constant id in the constructor are handled. Each id
gets a subclass `C$r8id<k>` with clones of the switching methods, each switch narrowed to arm `k`.
Construction sites with a known id allocate the subclass. The `synthetic` flag is not required,
since `final` is what makes the split sound. It bumps `IMPORT_FORMAT_VERSION` to 10. See M2 and
M3.

### S11. Key the call-target transitive rule on the path prefix (`cta_key`)

**Applies to:** P16. **Status:** done (`76982fc4`). Fixes P16 on `cpuinfo`, but is a net memory
loss everywhere else (P17); see S12.

Each tag's path is split once at every prefix (`cta_key`, `cta_key_wild`), so the join only
retrieves edges whose source path is that prefix. The step emits the new tag's exact keys itself.
A first version derived them in a separate rule, which cost an extra iteration per edge and fell
behind the baseline. On its own it doesn't make `cpuinfo` converge, because P15 dominates; with
S10 it cuts `cpuinfo`'s time by 30%. See M2, M3 and M6.

### S12. Turn off `cta_key` except where it pays

**Applies to:** P17. **Status:** undecided; measured −4 GB on `cash.p.terminal`.

Use the keyed join only at high-fan-out vertices, or revert S11. S11 is a net loss on three of the
four apps measured, and the one that benefits (`cpuinfo`) was measured before S16 shrank its
`call_target_assign_like`. Do this before S13.

### S13. Store call-target tags compactly

**Applies to:** P17. **Status:** undecided.

Put `call_target_assign_like` and `cta_key` in a BYODS trie, as `locals` and `assign_like` are,
the way `locals_key` is held.

### S14. Bound decision-set churn

**Applies to:** P15. **Status:** rejected.

`bounded:k` and `spill:k` cap the churn. Not needed for `cpuinfo` after S10: it mints 7.2 k
decisions and `context_locals` holds 1.1 M rows. Not a fix for `greenbits` either: `--hybrid-context
none` shows the context-free closure does not converge on its own (see S15). Never measured.

### S15. Change the hybrid context mode

**Applies to:** P9, P15. **Status:** rejected.

Neither other mode finishes on `cpuinfo` or `greenbits`; see M2 and M4. `none` gives every caller
the union of the resolved callees' summaries; `collapse` removes the churn but not the volume.

### S16. Write back entry versions of parameters for JVM bytecode

**Applies to:** P10. **Status:** done (`8cea6bca`, `b6503b05`). Fixes `greenbits`.

`ParamWriteBack::Entry` in `ctadl-ir/src/ssa/mod.rs` builds the exit param-flow without the
parameters and fills in their version 0 after renaming. `ctadl index` chooses it per import
(`ArtifactLanguage::param_write_back`): `Entry` for jvm, jar, dex, apk and xapk, and `Exit` for
the rest. C keeps `Exit` because of P11; a comment on the `C` arm of `param_write_back` records
this. The chess fixture's sink is now scoped to `Lx2/c;->y`, where the move really is appended
(`0037a68e`); that passes with and without the change. See M4 and M5.

### S17. Lower C pointer accesses to `.deref`, then use `Entry` for C

**Applies to:** P11. **Status:** undecided; needs the C regression family run against it.

If `*p` lowered to `p.deref` for stores and reads, and `&v` to an address whose `deref` is `v`, a
write through a pointer parameter would be a field store on its entry version, and C could use
`Entry`. That also removes the rebinding false flow. All three changes have to go in together.
With only the store changed, the callee writes `v.deref` and the caller still reads `v`, so
`outparam` still loses its flow.

### S18. Filter call-target tags by static type

**Applies to:** P12, P13, P14 (and P10 before S16). **Status:** undecided, not implemented. Not
needed for `greenbits` since S16, but still a sound precision filter. The most precise fix: it
shrinks both the call-target relations (P17's memory) and the decisions.

- First step, with facts we have: drop a tag at a `ByVal` formal and at a field or array element
  of primitive type (field types are already in the path symbols). Before S16 that covered the
  5.5-6.7% of `greenbits` decisions that put an object in a primitive slot.
- Full fix: export a subtype fact from the dex import (the frontend already reads `.super` and
  `.implements`) and the static type of each vertex, and drop a tag wherever
  `target <: declared type` fails. A `check-cast` should narrow the same way; that is the part
  that addresses P12.

### S19. Check decisions against the formal's declared type

**Applies to:** P12, P13, P14. **Status:** undecided, not implemented.

Only mint a decision when the target is a subtype of the formal's declared type at that path:
S18's filter, applied in rules 2.1/2.2 rather than to every tag. One filter covers all the
remaining impossible decisions. The inputs are what `typecheck.py` already uses: the method
signature, field types in the path, and `.super`/`.implements`. It keeps `Object`, array-element
and framework cases, as the script does. It drops the decisions but not the tags, so S18 or S20
is still more precise.

### S20. Check receiver dispatch at CHA sites

**Applies to:** P13. **Status:** undecided.

For formal 0 at a CHA site, require that the target resolves to `f`. That needs the site's
dispatch key exported for CHA sites too, not only for sites in `callee_info`. This is the
smaller half of the fix everywhere except small apps.

### S21. Only mint a decision whose target can resolve a critical call

**Applies to:** P12, P13, P14 (and P10 before S16). **Status:** undecided; effect not measured.
Not needed for `greenbits` since S16, but still a sound precision filter.

Add the site's dispatch key to `critical_summary`, carry it up through rule 1.2, and require
`callee_resolvents(target, key, _)` in rules 2.1 and 2.2.

- A target that answers none of the calls its decision exists for never produces a
  `context_assign` (rule 3.1 needs exactly that join), so this should change no results and needs
  no new facts.
- It removes the `JsonGenerator` and `byte[]` cases, but not method names every class has, such
  as `toString`.
- It prunes decisions only, not tags, so on its own it leaves the memory.

### S22. Build critical summaries from identity flows only

**Applies to:** P14. **Status:** undecided. The most accurate fix and the most work.

Rule 1.1 should ask which formal path the receiver object comes from: copies, field loads and
stores, and returns, not computed values. It needs an identity-flow subset of `locals`.

### S23. A Jackson model

**Applies to:** P10, P14 (as `greenbits` looked before P10 was found). **Status:** rejected.

It would help `greenbits`, but the Compose and coroutine hubs showed the problem is general, and
the cause turned out to be P10.

### S24. Fold `reach_vp` into its consumers

**Applies to:** P18. **Status:** undecided; up to −2.3 GB on `cash.p.terminal`, estimated.

Derive `locals_key`, `locals_key_wild` and `locals_wild` from the `locals` delta directly.

### S25. Free `actual_param` after scc 1

**Applies to:** P19. **Status:** undecided; −1.6 GB on `cash.p.terminal`, estimated.

One option is to generate the call-arg edges in Rust before `ascent_run`.

### S26. Find the memory held around the fixpoint

**Applies to:** P20. **Status:** undecided, not investigated.

Find the 2.3 GB `facts.try_save` keeps, and the 7-8 GB held through the index.

### S27. Reorder or key the scanning joins, and skip the wildcard rules when no path has an offset

**Applies to:** P21, and P6's cost on Java-only imports. **Status:** undecided. Time only: 35-50 s
of rule time on `cash.p.terminal`.

S12, S24 and S25 together come to about 8 GB on `cash.p.terminal`, 20% of the peak. Only S12 was
measured.

### S28. Make the suite fail on a lost flow

**Applies to:** P22. **Status:** done (`31594e08`).

A pcode case with no source-to-sink flow is now a FAIL on every platform, not a SKIP on Darwin.
It hasn't been seen catching the `example` loss: with the `CTADL_MEM2REG` switch removed,
reproducing it needs an `exact` build.

# Measurements

## M1. `androidudpbus`: the `locals` blowup on native code (2026-09-29)

**Problems:** P1, P2, P22. **Solutions:** S1, S2, S3, S28.

`org.vi_server.androidudpbus` did not index in 15 minutes. It hit the timeout at 88 GiB, against a
budget of about 2.8 GB (100x its 28.2 MB of IR). Nearly all of the IR is `libudphub.so`, a 567 KB
Rust/tokio library. The Java half is trivial (231 call sites). With S2 and S3 it indexes in 169 s
at 15.0 GB, about 5x over budget.

**Growth before any fix.** `locals` held 10.7 M rows after 20 s of fixpoint, 152 M after 80 s, and
341 M after 160 s (9.9 GB for the store alone).

**mem2reg escape policies on `androidudpbus`**, one run per policy:

| Escape policy | Stack accesses promoted (of 584,560) | Wall time | Peak memory |
|---|---|---|---|
| off (pruned SSA only) | 0 | 849 s | 52.0 GB |
| `exact` | 463,765 (79%) | 141 s | 16.8 GB |
| `window:8` | 414,236 (71%) | 163 s | 15.0 GB |
| `window:16` | 345,588 (59%) | 266 s | 21.4 GB |
| `window:64` | 171,216 (29%) | 406 s | 30.1 GB |

The promotion counts match the stack census's predictions exactly, and no function was skipped.
`window:8` peaking below `exact` is probably run-to-run variation.

**On the regression suite** (`pcode`, `jni` and `android-native`, 50 cases), with SARIF output
compared against the pass turned off:

- `window:8`, `window:16` and `window:64` lose nothing: the same results and the same flows. In
  the chess app, only the intermediate steps shown for a flow differ. With `window:8` as the
  default (`31594e08`), the suite passes (50 of 50), and its SARIF matches the `window:8` run.
- `exact` loses real flows:
  - It loses the only flow in `nightly/tests/c/example.c`. `transfer(&x.b, y)` writes `out->d`, 4
    bytes past the address passed in, and the sink never sees it. The suite hid this (P22).
  - It also drops one result in `structret`.

**After mem2reg** (`31594e08`):

| App | Wall time | Peak memory | `locals` rows | `locals` store | Largest function's share |
|---|---|---|---|---|---|
| `androidudpbus` | 169 s | 15.0 GB | 340 M | 8.9 GB | `FUN_00144f04`, 72% |
| `glxy` (hit the 48 GB cap before) | 120 s | 21.6 GB | 425 M | 12.5 GB | `FUN_0010b408`, 97% |
| `cpuinfo` (timed out at 900 s before) | timed out at 1800 s | 40.8 GiB | n/a | n/a | n/a |

- `FUN_00144f04`'s `locals` rows come from about 2,560 paths of formal 0 and 120 global paths, and
  reach about 16k SSA temporaries.
- `glxy` shows the same pattern as `androidudpbus`.
- `cpuinfo` is not a native-code problem. Its native libraries are tiny (mem2reg touched about 5k
  accesses). Its index has 53,796 CHA call sites and 623,583 paths. The cause is P9 (M2), and with
  S10 (`a7d716e8`) it finishes in 82 s at 12.4 GB.

On the older `a979c371` binary, `com.kaeruct.glxy` hit the 48 GB cap and `cpuinfo` timed out.

## M2. `cpuinfo`: R8-merged Kotlin lambdas (2026-09-30)

**Problems:** P9, P15, P16, P23. **Solutions:** S10, S11, S14, S15.

`com.kgurgul.cpuinfo` did not index in 30 minutes. It is not a memory blowup (28.9 GB at 640 s)
but a fixpoint that never converges: after 640 s, scc 4 is still adding rows and minting
decisions, and every iteration costs more per new row than the last. With S10 it converges; with
S11 as well, it takes 82 s and peaks at 12.4 GB.

**Timeout ladder** (10 to 640 s, `CTADL_INDEX_TIMEOUT_SECS`, default `HybridContext::Decision`):

| Rung | Iterations | Peak | `locals` | `context_locals` | `call_target_assign_like` | Decisions |
|---|---|---|---|---|---|---|
| 10 s | 41 | 2.5 GB | 3.1 M | 0.14 M | 1.1 M | 5.8 k |
| 40 s | 95 | 6.9 GB | 15.2 M | 4.3 M | 5.8 M | 10.1 k |
| 160 s | 118 | 16.0 GB | 34.9 M | 20.0 M | 7.8 M | 16.2 k |
| 640 s | 127 | 28.9 GB | 56.9 M | 39.0 M | 8.7 M | 18.6 k |

Rule costs per tuple look normal, so the signal is the marginal cost, rule time per new row
between rungs. Two relations blow up: `context_locals` (P15) and `call_target_assign_like` (P16).

**Other hybrid context modes (S15)** also fail to finish on their own, just differently:

- `none` timed out at 1200 s (31.5 GB). With no context, every caller gets the union of the
  resolved callees' summaries: `assign_like` reached 59 M rows (105x its input), `summary`
  10.2 M, `locals` 217 M. `call_target_assign_like` alone took 45% of rule time.
- `collapse` hit a 30 GiB cap at 822 s, about the same memory trajectory as `decision`. Profiled
  alone at 600 s (27.2 GB), it removes the churn (9.3 M set-growing unions, against 221 M) but
  not the volume: `context_locals` reached 55 M rows, nearly all of them ⊤, in the same hub
  functions (`Li;->e` 10.4 M, `Li;->r` 7.9 M, `Lk;->r` 6.2 M). A ⊤ summary is applied at every
  caller that establishes any decision, so decisions doubled, to 38.8 k.
  `call_target_assign_like` was again the most expensive rule, at 24% of rule time.

**S10 and S11 on `cpuinfo`.** Each configuration was measured on a fresh import made with its own
binary. S10 alone is `1a5de8c2` (branch `r8-merged-lambdas`, the baseline plus S10).

| Build | Result | Wall time | Peak memory | Iterations | `locals` | `context_locals` | Join pairs (transitive rule) |
|---|---|---|---|---|---|---|---|
| baseline `62ffedde` | cut off at 1800 s | 1894 s | 41.5 GB | 134 | 74.6 M | 58.0 M | 18.6 B (0.59% prefix matches) |
| S11 `76982fc4` | cut off at 1800 s | 1895 s | 50.4 GB | 137 | 84.2 M | 68.2 M | 122 M |
| S10 `1a5de8c2` | fixpoint | 116-119 s | 7.6-7.8 GB | 1,316 | 27.0 M | 1.08 M | 581 M (42% prefix matches) |
| both `a7d716e8` | fixpoint | 82-85 s | 12.4 GB | 1,316 | 27.0 M | 1.08 M | 246 M |

- **S11 does its job but is not enough.**
  - At 160 s it visits 30.7 M pairs instead of 1.69 B. The call-target rules take 17.6 s instead
    of 24.4 s, and the run gets further in the same time: 117 iterations against 115, and 32.6 M
    `locals` rows against 29.0 M.
  - At 1800 s the call-target rules take 32 s, down from 210 s, but `context_locals` still takes
    1,433 s. That is the churn S10 removes.
- **S10 removes the cause.**
  - 620 classes split into 3,562 ids, 1,068 methods are cloned per id, and 4,296 construction
    sites are retagged.
  - Decisions fall from 22 k (still growing) to 7.2 k, and `context_locals` from 58 M rows (still
    growing) to 1.1 M.
  - Before pruning, the Java IR grows from 598 k to 2.2 M assignments, because each clone carries
    every arm until SSA prunes the unreachable ones. The import time is unchanged at 37-39 s.
- **Together, S11 cuts S10's time by 30%.**
  - With the split, the unkeyed call-target rule is 46% of the fixpoint's rule time: 43 s of
    92 s. Keyed, it takes 15 s of 58 s.
  - It costs memory: `cta_key` holds 15.4 M rows, and the peak rises from 7.6 GB to 12.4 GB.
- **Neither change loses a result.**
  - The regression suite's SARIF is identical across all four builds, all 324 files.
  - `ClassIdMergedFlow`, new with S10, has the merged shape with five ids, more than CHA resolves
    statically. Under dex, the baseline reports a false positive at line 30 and S10 reports only
    the two real flows. The jvm frontend shares the config and has no split, so the false
    positive is not asserted.
  - S10 re-pins the xtask apk report counts, because `com.noto` has 137 merged classes.
- **Two sources of nondeterminism (P23)** turned up while checking that S11 leaves results alone.
  All 25,250 Java functions have identical edge counts under S10 and both.

## M3. S10 and S11 across the corpus (2026-09-30)

**Problems:** P9, P16, P17. **Solutions:** S10, S11.

The Dex half of every APK in the original 15 was imported and indexed. The results are in
`/Volumes/Shampoo/ct-bigapk/small/r8-general/RESULTS.md`.

- **Merged classes are common.** 12 of 15 apps have at least one. `ceno` has 986 classes split
  into 5,812 ids, `greenbits` has 809 into 6,979, `cpuinfo` 620, `chess` 173, `pincredible`
  150 and `komodo` 83.
- **S10 generalizes where merged classes are the hub.**
  - `ie.equalit.ceno` goes from hitting the 28 GiB cap at 599 s to finishing in 110 s at
    13.9 GB. `cpuinfo`'s Dex half goes from the cap at 819 s to 96 s.
  - Small apps are unchanged.
  - On `darkcoin`, which has 8 merged classes, S10 alone takes 169 s at 17.4 GB against the
    baseline's 179 s at 18.0 GB.
- **S11 does not generalize as written.** It pays off only where the unkeyed join is mostly
  wasted, as on `cpuinfo`, where 0.6% of pairs matched the prefix.
  - On `darkcoin` 34% matched, so S11 saves no time. `cta_key` adds 44 M rows, and the peak goes
    from 18.0 GB to 27.2 GB.
  - On `greenbits` it cuts the pairs from 1.7 B to 90 M. But the keyed rule is slower (31 s
    against 22 s at 240 s), and it adds 8 GB.
- **`greenbits` blows up for another reason.** It hits a 55 GiB cap under both builds, with 87 k
  decisions at 240 s, and the split raises that to 110 k. Its `context_locals` hubs are Jackson
  databind's generic serializers, with up to 2 k decisions per function: for example
  `ObjectMapper._convert`, `DefaultSerializerProvider.serializeValue` and
  `ObjectWriter$Prefetch.serialize`. The cause is P10 (M4).

## M4. `greenbits`: reused parameter registers (2026-09-30)

**Problems:** P10, P12, P13, P14, P15, P17. **Solutions:** S16, S18-S23.

`com.greenaddress.greenbits_android_wallet` (Dex only, 215 k functions after the R8 split) did not
index. With `92fac26b` (S10 and S11) it hits a 55 GiB guard at 440 s, about 400 s into the
fixpoint. No fixpoint in sight: 97 iterations at 320 s, against the 1,316 `cpuinfo` needs. With
S16 it reaches a fixpoint in 399 iterations at 12.4 GB.

**Most decisions were provably impossible.** A probe dumped every decision, and each was checked
against the class hierarchy in the app's smali (dex formals are registers, so `J` and `D` take
two):

| Rung | Decisions | Class not a subtype of the declared type | Object in a primitive slot | Compatible | Undecidable | `context_locals` memberships held by impossible decisions |
|---|---|---|---|---|---|---|
| 40 s | 44 k | 40.9% | 0.2% | 55.0% | 3.9% | 10.5% |
| 80 s | 178 k | 59.5% | 6.0% | 30.1% | 4.3% | 17.9% |
| 160 s | 448 k | 57.7% | 6.7% | 17.6% | 18.1% | 37.8% |
| 320 s | 585 k | 53.5% | 5.5% | 16.8% | 24.1% | 50.3% |

Decisions here are `resolvent` rows, one per (function, formal.path, target). "Undecidable" is
`Object`-typed or framework-typed. Examples: a `JsonGenerator` formal "holds"
`StdDelegatingSerializer`, `ObjectMapper` "holds" `BeanDeserializer`, and a `byte[]` element holds a
serializer. The worst functions aren't Jackson: Compose's `SpanStyleKt.fastMerge` (28.5 k of 29.8 k
decisions impossible, objects in `long` slots), `TextStyle.merge`, and kotlinx `JobSupport`.

**It is not the context machinery.** At 240 s, `--hybrid-context none` is worse than `decision`:
85 M `locals` rows against 53 M, 27 M `assign_like` against 21 M, 39.7 GB against 35.4 GB. Neither
converges. The decisions make it worse, but the context-free closure is too big on its own,
because the same impossible call targets resolve calls and instantiate summaries.

**Timeout ladder** (10 to 320 s, 55 GiB guard, default `decision`):

| Rung | Iterations | Peak | `locals` | `call_target_assign_like` | `cta_key` | `context_locals` | Decisions | Set unions that grew |
|---|---|---|---|---|---|---|---|---|
| 10 s | 1 | 9.9 GB | 0.8 M | 0.08 M | 0 | 0 | n/a | n/a |
| 40 s | 21 | 13.5 GB | 7.3 M | 1.2 M | 1.6 M | 0.02 M | 13 k | 12 k |
| 80 s | 53 | 18.2 GB | 22.6 M | 5.3 M | 9.2 M | 0.58 M | 44 k | 0.3 M |
| 160 s | 79 | 26.7 GB | 41.9 M | 13.0 M | 25.8 M | 4.05 M | 100 k | 40 M |
| 320 s | 97 | 42.2 GB | 69.2 M | 23.5 M | 52.8 M | 11.7 M | 120 k | 201 M |

"Decisions" here counts distinct interned `Decision`s (formal.path = target, without the
function); the table above counts `resolvent` rows.

- **Time.** At 320 s, the `call_target_assign_like`/`cta_key` step is the most expensive rule (14%
  of rule time), followed by `context_locals` (11%, 7% and 7% for its three largest rules) and
  `locals` (7%, 7% and 5%).
  - The context-free relations cost the same per new row at every rung: `locals` 0.9 µs,
    `call_target_assign_like` 2.1 µs. They are expensive by volume.
  - `context_locals` costs more per new row as the sets grow (P15), driven by the impossible
    decisions.
- **Memory: call-target tags take about half (P17).** At 320 s the footprint is 39.0 GB.
  - The per-index census accounts for 34.3 GB: 25.9 GB in Ascent's default containers and 8.4 GB
    in BYODS stores.
  - `call_target_assign_like` and `cta_key` hold 17.0 GB of that.
  - Every contextual relation together holds 5.3 GB; `locals` is 2.4 GB.

| Relation (320 s) | Rows | Total | Row store | Indices |
|---|---|---|---|---|
| `cta_key` | 52.8 M | 10.13 GB | 3.76 GB | `cta_key_indices_0_1_2_3_4` 3.35 GB (full), `cta_key_indices_0_1_2` 3.03 GB (4.7 M keys) |
| `call_target_assign_like` | 23.5 M | 6.91 GB | 1.61 GB | `_indices_0_1_2_3` 1.44 GB (full), `_indices_0` 1.35 GB (48 k keys), `_indices_0_1_2` 1.33 GB (4.4 M keys), `_indices_0_1` 1.18 GB (0.84 M keys) |
| `edge_split` (BYODS) | 38.5 M | 2.58 GB | | trie |
| `locals` (BYODS) | 69.1 M | 2.42 GB | | trie |
| `context_locals` | 11.7 M | 1.89 GB | 0.94 GB | `_indices_0_1_2_3_4` 0.60 GB, `_indices_0_1_2` 0.21 GB, `_indices_none` 0.13 GB |
| `assign_like` (BYODS) | 23.8 M | 1.60 GB | | trie |
| `establishes_via` | 5.9 M | 0.96 GB | 0.20 GB | 4 indices, 0.16-0.21 GB each |
| `set_establishes_via` | 4.2 M | 0.87 GB | 0.40 GB | `_indices_0_1_2_3_4` 0.36 GB, `_indices_0_1` 0.10 GB |

**S16's result.** Same import and probe binary, at the 55 GiB guard, on a machine under load (so
wall times are not comparable):

| Build | Result | Iterations | Peak | `locals` | `critical_summary` | Decisions | `context_locals` | Default containers |
|---|---|---|---|---|---|---|---|---|
| `92fac26b` (320 s rung) | still growing | 97 | 42.2 GB | 69.1 M | 387 k | 585 k | 11.7 M | 25.9 GB |
| `92fac26b` | killed at 55 GiB, 440 s | | 55 GiB | | | | | |
| entry write-back | fixpoint | 399 | 12.4 GB | 7.5 M | 64 k | 36 k | 77 k | 2.6 GB |

- `call_target_assign_like` falls from 23.5 M rows to 0.97 M, and `cta_key` from 52.8 M to 1.6 M.
- `_quoteChar` is in no `critical_summary` row. The Jackson hubs keep at most one decision:
  `Prefetch.serialize` had 847.
- `typecheck.py`: 92.4% of decisions are compatible (17% before), 0.003% put an object in a
  primitive slot (6%), and impossible decisions hold 1.7% of `context_locals` memberships (44%).
  Of the 5.8% (2,123) "not a subtype" left, 799 are `$r8id` classes that the script couldn't
  type; they are fine. The other 1,324 are real.
- The regression suite loses no real flow. Its SARIF is identical in 329 of 330 files; the one
  difference was the chess artifact described in P10.
- Applied to every language, `Entry` lost `C:outparam`'s only flow (P11), so S16 is per language.

**The 1,324 decisions still impossible (3.6%).** `typecheck2.py` is `typecheck.py` plus
`C$r8id<k> <: C`. With it, "not a subtype" drops from 2,123 to exactly 1,324, and those hold 1.0%
of `context_locals` memberships. 831 of the 1,324 have only upstream decisions that are themselves
impossible (rule 2.2); each bad root seeds about 1.7 more. Every decision traces back to these
roots:

| Root cause | Problem | Decisions |
|---|---|---|
| The tag skips a `check-cast` in the caller | P12 | 520 |
| The tag skips a type test in a callee, through its return value | P12 | 368 |
| The receiver goes to a CHA override it can't dispatch to | P13 | 286 |
| Computed value | P14 | 54 |
| Mixed, or in a recursive cycle | | 97 |

56 of the mixed ones are in Kotlin's `FunctionDescriptorImpl.doSubstitute`/`substitute` recursion,
where the root walk cuts the cycle.

## M5. Impossible decisions across the corpus (2026-09-30)

**Problems:** P10, P12, P13, P14. **Solutions:** S16, S18-S21.

The same build (with S16), measurement and scripts as M4, on the Dex half of all 15 apps. Each ran
alone under a 55 GiB memguard cap with an 1800 s timeout. All 15 reach a fixpoint, and `udpbus`
and `andiodine` mint no decisions. `greenbits` was re-run the same way and reproduces the 1,324.

| App | Index | Decisions | ok | `$r8id` fixed | Impossible | Held memberships | Inherited only | Receiver (P13) | Caller cast (P12) | Other (P12, P14) |
|---|---|---|---|---|---|---|---|---|---|---|
| `cash.p.terminal` | 469 s, 40.7 GB | 83,092 | 93.0% | 14 | 2,881 | 0.4% | 1,186 | 9% | 33% | 58% |
| `darkcoin` | 69 s, 9.9 GB | 27,682 | 86.3% | 0 | 1,456 | 1.1% | 411 | 27% | 26% | 44% |
| `greenbits` | 120 s, 13.0 GB | 36,489 | 94.6% | 799 | 1,325 | 1.0% | 831 | 22% | 41% | 33% |
| `ceno` | 30 s, 4.8 GB | 6,548 | 92.3% | 2,988 | 190 | 0.2% | 67 | 31% | 51% | 18% |
| `cpuinfo` | 15 s, 2.1 GB | 4,569 | 94.9% | 2,150 | 88 | 0.2% | 27 | 26% | 50% | 24% |
| `tinykeepass` | 8 s, 1.2 GB | 1,428 | 88.7% | 0 | 32 | 0.3% | 13 | 72% | 0% | 28% |
| `komodo` | 11 s, 1.8 GB | 1,200 | 93.3% | 8 | 20 | 0.1% | 8 | 70% | 25% | 5% |
| `ipcam` | 11 s, 1.8 GB | 863 | 85.5% | 0 | 18 | 0.0% | 8 | 89% | 0% | 11% |
| `pincredible` | 4 s, 0.9 GB | 439 | 86.8% | 29 | 19 | 0.3% | 9 | 0% | 68% | 32% |
| `glxy`, `chess`, `pckeyboard`, `openttd` | 2-7 s, ≤ 0.9 GB | 619 | | 10 | 11 | 0.0% | 0 | | | |

"Impossible" is subtype failures plus objects in primitive slots. The three cause columns are
shares of those. `greenbits` also has 56 decisions (4%) in a cycle, and `darkcoin` 33 (2%).
Totals: 162,929 decisions, 6,040 (3.7%) impossible and 2,560 of those inherited only. By root:
receiver 1,092 (18%), caller cast 2,030 (34%), other 2,826 (47%), no root reached 92.

- **`greenbits` is typical.** Every app with more than a few hundred decisions has 3-5%
  impossible, holding at most 1.1% of `context_locals` memberships. Nowhere do they matter for
  memory; P12-P14 are precision problems.
- **The `$r8id` correction matters most where the split is big.** It accounts for 2,988 on `ceno`
  and 2,150 on `cpuinfo`. Without it those apps would look far worse than they are.
- **Which cause dominates depends on the libraries.** Small apps are mostly P13 (70-89%). The big
  crypto wallets are mostly "other", almost all the Bouncy Castle `getInstance` idiom (P12).
  `cash.p.terminal` adds Jackson's `TypeFactory` (P14).
- **S16 also helps apps that did finish.** Same imports as M3, and iteration counts are comparable
  even though wall times are not (those runs went four at a time). `cpuinfo` falls from 1,316
  iterations and 12.4 GB to 231 and 2.1 GB. `ceno` goes from 540 and 13.9 GB to 283 and 4.8 GB,
  and `darkcoin` from 606 and 29.7 GB to 423 and 9.9 GB. `cash.p.terminal` wasn't measured
  before; it is now the largest at 40.7 GB. That is its size, not a blowup; see M6.

## M6. `cash.p.terminal`: big, not blowing up (2026-10-01)

**Problems:** P16, P17, P18, P19, P20, P21, P23. **Solutions:** S12, S24-S27.

`cash.p.terminal` (Dex half) is the largest app in the corpus. On head (`a84d75e7`) it reaches a
fixpoint in 390 s at 41.2 GB, in 637 iterations. It was the one large outlier left after M5, so it
was profiled with the timeout ladder. Its cost is in line with its size; no fix is needed for it
to finish, and each of S12, S24-S27 lowers the constant for every large app.

**It costs what its size predicts.**

- **Size.** 846,592 functions, 1.37 M Java call sites, 638 MB of IR: about 4x `greenbits`.
- **Memory relative to IR.** Peak memory is 65x the IR. The rest of the corpus runs from 34x
  (`ceno`) to 123x (`tinykeepass`), and `greenbits` is 70x. Per function it is 48 KB, against
  `greenbits`' 61 KB.
- **The contextual relations are negligible.** 192 k `context_locals` rows and 25 k decisions.
  It has neither `cpuinfo`'s hubs (P9) nor `greenbits`' impossible decisions (P10).

**Timeout ladder** (10 to 240 s, 55 GiB guard, default `decision`):

| Rung | scc 4 iterations | Peak | `locals` | `assign_like` | `edge_split` | `ext_dst` | Default containers |
|---|---|---|---|---|---|---|---|
| 10 s, 20 s | 0 (still in scc 1) | 16.1 GB | 3.3 M | 38.2 M | 0 | 0 | 3.6 GB |
| 40 s | 2 | 24.7 GB | 8.5 M | 39.0 M | 40.2 M | 0 | 4.2 GB |
| 80 s | 6 | 30.9 GB | 13.3 M | 39.9 M | 40.6 M | 15.3 M | 6.2 GB |
| 160 s | 64 | 41.0 GB | 28.6 M | 41.8 M | 44.7 M | 27.3 M | 11.6 GB |
| 240 s, no timeout | 637 (fixpoint) | 41.2-41.7 GB | 31.0 M | 42.3 M | 46.0 M | 28.8 M | 11.7 GB |

- **Memory peaks by iteration 64.** The remaining ~570 iterations add 2 M `locals` rows and no
  memory.
- **Context-free relations cost the same per new row at every rung** (40→80 s, 80→160 s,
  160→240 s):
  - `locals`: 3.0, 1.5 and 1.8 µs
  - `edge_split`: 0.6, 0.3 and 0.7 µs
  - `ext_dst`: 0.5, 0.6 and 1.2 µs
  - `locals_key`: 0.4, 0.2 and 0.3 µs
  - `summary`: 3.3, 3.3 and 3.4 µs
- **Only contextual rules get more expensive per row, on almost no rows** (P21). That costs time,
  not memory.

**Where the memory goes.** At the end of the fixpoint the footprint is 39.0 GB:

- **Held before the fixpoint starts: 7.5-8 GB** (P20).
- **scc 1 adds 7 GB, to 15 GB.**
  - `assign_like` grows from 10.7 M to 38.2 M rows. The 27.5 M new rows are parameter edges, two
    per `actual_param` row.
  - This is linear in the number of call sites, not CHA fan-out.
- **scc 4 adds 24 GB.**
  - BYODS tries, 14.9 GB: `edge_split` 4.07, `assign_like` 3.09, `locals` 3.06, `locals_key` 2.92
    and `ext_dst` 1.77.
  - Ascent's default containers, 11.7 GB:

| Relation | Problem | Total | Row store | Indices |
|---|---|---|---|---|
| `cta_key` | P17 | 2.88 GB | 0.94 GB | `cta_key_indices_0_1_2` 1.11 GB, `cta_key_indices_0_1_2_3_4` 0.84 GB |
| `call_target_assign_like` | P17 | 2.41 GB | 0.40 GB | `_indices_0_1_2` 0.79 GB, `_indices_0_1` 0.54 GB, `_indices_0_1_2_3` 0.36 GB, `_indices_0` 0.32 GB |
| `reach_vp` | P18 | 2.34 GB | 0.81 GB | `reach_vp_indices_none` 0.81 GB, `reach_vp_indices_0_1_2` 0.73 GB |
| `actual_param` | P19 | 1.56 GB | 0.54 GB | `actual_param_indices_none` 0.54 GB, `actual_param_indices_0_1_2` 0.48 GB |
| `alias_of_formal` | | 0.61 GB | 0.13 GB | `_indices_0_1` 0.35 GB, `_indices_0_1_2` 0.12 GB |

**S12: head rebuilt with `76982fc4` (S11) reverted.**

| Build | Peak | scc 4 | Call-target rule time | Join pairs |
|---|---|---|---|---|
| head, two runs | 41.2-41.7 GB | 187-216 s | 16.7-19.4 s | 8.8 M (keyed) |
| head without `76982fc4` | 37.1 GB | 188 s | 20.7 s | 11.2 M, 79% prefix matches |

- **Keying saves no time here.** The unkeyed join is barely wasted, unlike `cpuinfo`'s 0.6%.
- **The results are unchanged.** Relation sizes are identical except `set_*` and
  `context_summary_set`. Those also differ between two runs of head (P23).

**Where the time goes.** Wall time is 341 s without `76982fc4`:

| Phase | Time |
|---|---|
| Load the IR, SSA, codegen, save the facts | 57 s |
| Path closure | 20 s |
| scc 1 | 18 s |
| scc 4 | 188 s |
| Census and save | 47 s |

Rule time is 161-189 s:

- **The core propagation rules are about 45%.** They are the two `locals` rules over `ext_dst` and
  `edge_split`, plus the rules that derive `ext_dst` and `edge_split`. They cost 0.4-0.8 µs per
  row, which is normal.
- **Joins that scan a total relation every iteration cost 25-39 s** (P21).
- **The wildcard rules cost about 9 s and produce nothing on Java** (P21).

## M7. Small apps with native code: six kinds of blowup (2026-10-02)

**Problems:** P1, P3, P4, P5, P6, P7, P8, P18. **Solutions:** S5, S6, S9.

Every APK was imported whole, Dex and arm64 libraries, and indexed on head (`a84d75e7`) with an
1800 s Ascent timeout under a 55 GiB guard. Apps that did not reach a fixpoint, and the
over-budget ones that did, got a timeout ladder. The budget is 100x the IR (the sum of
`ir-program.bitcode` over the app's imports). The six large APKs of the original 15 were not
indexed with native code, because they fail at import (P8). So the corpus was extended with small
apps instead: the 35 permissively licensed F-Droid apps of at most 8 MB whose arm64 libraries total
at most 4 MB.

44 small apps measured: 22 over budget, 11 at 5x or more, 6 without a fixpoint.

| App | IR | Peak | Against budget | Result | Kind |
|---|---|---|---|---|---|
| `glxy` | 15.1 MB | 21.5 GB | 14.3x | fixpoint, 117 s | P1 (`libgdx`) |
| `reinstead` | 46.4 MB | 62 GB or more | 13.4x | guard, 1,610 s | P3 |
| `retrodrawing` | 15.4 MB | 20.0 GB | 13.0x | fixpoint, 236 s | P1 (`libgdx`) |
| `avifview` | 51.7 MB | 59 GB or more | 11.5x | guard, 611 s | P1 |
| `AnarchRE` | 63.8 MB | 57.8 GB | 9.0x | timeout, 2,241 s | P5 (SDL3) |
| `halma` | 31.5 MB | 22.6 GB | 7.2x | fixpoint, 190 s | P1 (`libgdx`) |
| `bined` | 15.5 MB | 10.8 GB | 7.0x | fixpoint, 245 s | P7 |
| `heartratemonitor` | 97.2 MB | 61 GB or more | 6.3x | guard, 299 s | P5 (SQLite) |
| `udpbus` | 28.2 MB | 15.8 GB | 5.6x | fixpoint, 173 s | P1 (stack slots) |
| `a2050` | 60.2 MB | 33.9 GB | 5.6x | fixpoint, 889 s | P1, P6 |
| `conscryptprovider` | 67.7 MB | 34.0 GB | 5.0x | timeout, 1,879 s | P6 |
| `fir.tube` | 27.1 MB | 12.4 GB | 4.6x | fixpoint, 93 s | P1 |
| `dictionary.fork` | 32.0 MB | 9.4 GB | 2.9x | fixpoint, 321 s | P4 |
| `termux.nix` | 38.5 MB | 9.7 GB | 2.5x | fixpoint, 831 s | P4 |
| `scrcpy` | 113 MB | 20.7 GB | 1.8x | timeout, 2,068 s | P4 |

The 29 other apps are at 1.6x or below. For a guard kill, the peak is the guard's last sample. M8
later found P4 also in `conscryptprovider` and `a2050`.

**The edge-delta join (P4) on the ladder:**

- `termux.nix`: `locals` costs 0.5, 9.5, 81 and 173 µs per new row on successive rungs (40 s to
  the fixpoint). The edge-delta rule takes 59, 144, 574 and 737 s; the `locals`-delta rule stays
  at 8 s. Between 160 and 320 s `edge_split` triples, to 35.5 M rows, while `locals` gains 5 M.
  The peak is only 9.7 GB.
- `scrcpy`: 0.9, 1.1, 5.9, 11.2 and 22.0 µs per new row. Memory is flat at 14 GB after 40 s, and
  it did 32 iterations in 2,049 s.
- Milder in `androidcrypt` (11x), `openarcade`, `aiyo`, `untracker` and `freezeyou` (3.6-8x).

## M8. The edge-delta join: libc++abi's demangler (2026-10-02)

**Problems:** P4. **Solutions:** S6, S7, S8.

What feeds the edge-delta join in `termux.nix` and the other apps of P4. Answer: summaries
instantiated at static call sites, nearly all from libc++abi's Itanium demangler, which every app
with the symptom links statically. S6 fixes it for the demangler.

**Method.** A probe (`CTADL_EDGE_CENSUS`, see "Data") runs after the fixpoint or timeout. It
attributes every `assign_like` row to the rule that made it, with the callee for a summary
instantiation. It then charges each of the row's exact `edge_split` keys `(f, v2, key)` with the
`locals` rows at that key. That count, "pairs" below, is what the edge-delta half enumerates when
the edge arrives. It uses the `locals` of the moment the census runs, so it overstates edges that
arrived early. Even so, pairs cost a steady 22-40 ns each across rungs and apps, so the pair count
is the rule's time.

**`termux.nix`, at four rungs** (run 4 at a time, so wall times are longer than in M7):

| Rung | Iterations | Edge-delta rule | Pairs | `assign_like` from static summaries | Their `edge_split` | `FUN_001afefc` summary rows |
|---|---|---|---|---|---|---|
| t40 | 34 | 28 s, 72% | 1.28 B | 0.74 M | 2.86 M | 4,276 |
| t160 | 88 | 147 s, 88% | 4.72 B | 2.55 M | 9.72 M | 6,680 |
| t320 | 101 | 599 s, 96% | 15.24 B | 8.56 M | 33.6 M | 117,482 |
| fixpoint | 684 | 596 s, 95% | 15.24 B | 8.69 M | 33.9 M | 118,454 |

- **Summary instantiation, not call-target resolution.** 99.97% of the pairs come from summaries
  instantiated at static call sites. Resolved call sites contribute 24 k pairs, and seeded and
  call-site edges 4 M.
- **One callee.** Between t160 and the fixpoint, 98.1% of the new pairs come from instantiating
  `FUN_001afefc`'s summary, whose rows grow from 6,680 to 118,454. It is instantiated in 8
  callers: itself at 40 call sites (75.5% of the new pairs), the others at 1-4 sites. All the late
  growth is in iterations 89-101, which take about 450 of the 630 s. The 583 iterations after them
  take almost nothing.
- **The function is libc++abi's demangler.** `FUN_001afefc` and the other top-20 callees lie
  2-73 KB after `__cxa_demangle@001aa9b8` in `liblocal-socket.so`. That is termux's 844 KB JNI
  socket helper, with libc++ and libc++abi linked in statically. The demangler is in an anonymous
  namespace, so its functions have no symbols.
- **The summary's late destinations.** At t160 only `First` and `Last` take thousands of sources.
  The other 45 arrive later, all on the return value: `ret.[-k].deref`, `ret.deref.[-k].deref` and
  `ret.deref.deref.[-k].deref` for 15 values of `k` from 568 to 1048.
- **Between t160 and the fixpoint** the rule enumerates 10.5 B pairs, and `locals` gains 6.4 M
  rows. How many of the rest fail the admissibility test in `concat` and how many rederive an
  existing row was not measured (S8).

**Every app with the symptom links the demangler:**

| App | Library | Pairs | In the demangler | Edge-delta share of rule time |
|---|---|---|---|---|
| `termux.nix` | `liblocal-socket.so` | 15.2 B | 100% | 95% |
| `dictionary.fork` | `libc++_shared.so` | 6.2 B | 99.9% | 76% |
| `scrcpy` (t640, 29 iterations) | `libconscrypt_jni.so` | 21.8 B | 99.7% | 68%, plus 25% in the `locals`-delta half |
| `androidcrypt` | `libveracrypt_crypto.so` | 0.6 B | 99.4% | 65% |
| `untracker` | `libquickjs.so` | 0.45 B | 93.3% | 38% |

"In the demangler" counts callees in the 128 KB after `__cxa_demangle`. Every top-20 callee lies
within 0x1c560 bytes of it. In each app the hot callees have the same shape as in `termux.nix`:
summaries of 5-150 k rows, instantiated at 5-15 call sites. `dictionary.fork`'s worst
(`FUN_001cb5f8`, 144 k rows, 5 callers) gives 48% of its pairs.

16 of the 44 indexed apps contain the demangler in an arm64 library (it has `itanium_demangle`
strings). In all 16 the edge-delta rule takes 20-92% of rule time, with a median of about 44%.
This includes `conscryptprovider` (49%) and `a2050` (40%), which M7 files under P6 and P1. In the
25 apps without the demangler that have a profile, it takes at most 10%, except six over-budget
apps of other kinds: four of P1, `bined` (P7) and `AnarchRE` (P5), at 21-37%. So P4 has one cause,
and the same cause adds to the cost of two of the other kinds.

By the attribution above, S6 should remove 93-100% of the join work in these five apps: about
595 of `termux.nix`'s 625 s of rule time.

**S6's result.** Across the 16 apps that link the demangler, with an 1800 s timeout:

| App | Before | After | |
|---|---|---|---|
| `termux.nix` | 812 s, 9.7 GB | 10 s, 2.3 GB | 83x |
| `nfcgate` | 170 s, 8.6 GB | 23 s, 5.5 GB | 7.5x |
| `dictionary.fork` | 314 s, 9.4 GB | 64 s, 6.3 GB | 4.9x |
| `androidcrypt` | 35 s, 2.9 GB | 9 s, 1.7 GB | 3.8x |
| `openarcade` | 29 s, 3.4 GB | 10 s, 2.6 GB | 3.0x |
| 7 more small apps | 6-40 s | 2-22 s | 1.5-2.5x |
| `a2050` | 862 s, 33.9 GB | 665 s, 30.6 GB | 1.3x |
| `mmrl` | 82 s, 10.3 GB | 71 s, 9.7 GB | 1.2x |
| `scrcpy` | timeout | timeout (50 iterations, was 32) | |
| `conscryptprovider` | timeout | timeout (68 iterations, was 66) | |

"Before" is the M7 run. "After" ran with the census probe on. In `termux.nix` the instantiation
work fell from 15.2 B pairs to 2.4 M. `scrcpy` and `conscryptprovider` peaked higher (38 and
47 GB) because they get further in the same time; they still have P4, in BoringSSL. Data:
`/Volumes/Shampoo/ct-bigapk/small/demangle-model/RESULTS.md`.

# Data

## Experiment switches

- In commit `fd5130f4` (WIP): `CTADL_DEAD_PHIS=1` turns on the dead-phi pass (S1), and
  `CTADL_LOCALS_CENSUS=<dir>` writes the `locals` breakdown.
- `CTADL_MEM2REG=exact|window:N|above` (removed in `31594e08`): the escape policy for S3.
- `CTADL_INDEX_TIMEOUT_SECS`: the timeout for a ladder rung.
- `[idxsizes]`: the vendored Ascent macro generates `index_sizes_summary()`, with keys, entries and
  approximate shallow bytes per relation and per index. It also logs heap reports for the
  `locals_key`, `edge_split` and `ext_dst` stores. Committed in `429a66fa`, so every index run logs
  both.
- `CTADL_DECISION_CENSUS`, `CTADL_FOCUS` and `CTADL_EDGE_CENSUS` are probe-only (uncommitted); see
  M4 and M8 below.

## M1: `androidudpbus`

All output is under `/Volumes/Shampoo/ct-bigapk/small/sweep-3c85988c/`:

- `prof/ladder.log`: time and peak memory for every run.
- `prof/rank-t*.txt`: rules ranked by time.
- `prof/ssa-census.txt`: phi counts per function.
- `prof/census-{base60,dp60}/census/*.tsv`: breakdowns of `locals` rows.
- `prof/stack-slots-FUN_00144f04.txt`: store and load counts per stack slot.

The mem2reg measurements are under `/Volumes/Shampoo/ct-bigapk/small/sweep-2b90a98b-m2r/`:

- `RESULTS.md`: a summary of the sweep.
- `ladder.log`: time and peak memory for every run.
- `udpbus-*/index.err`: the index log for each policy.
- `reg-*/`: the regression output and SARIF for each policy.
- `sarif-diff*.txt` (made by `sarif_diff.py`): result and flow differences from the run with the
  pass off.
- `escape-census.txt`: where escaped frame addresses go (from the
  `ctadl-import/examples/escape_census.rs` example).
- `uncommitted.diff`: the source the binary was built from.

The measurements with `window:8` as the default are under
`/Volumes/Shampoo/ct-bigapk/small/m2r-default-w8/`:

- `RESULTS.md`, `ladder.log`: a summary, and time and peak memory for every run.
- `reg/`, `sarif-diff.txt`: the regression output, and its comparison with the earlier runs.
- `runs/<pkg>/census/*.tsv`, `runs/<pkg>/index.err`: the `locals` breakdown and index log per app.
- `fn_probe-*.txt` (from the uncommitted `ctadl-import/examples/fn_probe.rs`, copied there): memory
  accesses, kept stack slots and calls for one function after the default pipeline.

## M2: `cpuinfo`

All output is under `/Volumes/Shampoo/ct-bigapk/small/cpuinfo-blowup/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `runs/t<N>/`: the ladder rungs; `index.err` has the rule times, relation sizes and the
  `context_locals by function` census; `rank.txt` is from `rank.py`.
- `marginal.txt` (from `marginal.py`): rule time, rows and marginal cost per head relation.
- `runs/probe-t160/`: the `call_target_assign_like` join counters and fan-out census, from the
  binary `ctadl-probe` built with `probe.diff` (uncommitted).
- `runs/hc-none/`, `runs/hc-collapse*/`: the hybrid context A/B.
- `smali/out/`: the app's baksmali output (`i.smali` is the merged lambda class `Li;`).

The measurements of S10 and S11 are under `/Volumes/Shampoo/ct-bigapk/small/cpuinfo-recs/`:

- `RESULTS.md`, `ladder.log`: a summary, and time and peak memory for every run.
- `bin/`: every binary measured, with its commit or diff.
- `runs/<build>/import/`: the import each build made. `runs/<build>/t<N>/`: the index runs.
  `index.err` is the full debug log, and `rank.txt` comes from `../cpuinfo-blowup/rank.py`.
- `reg-<build>/`, `sarif-diff-*.txt` (made by `sarif_diff.py`): the regression suite for each
  build, and their comparisons.
- `case-{base,rec2}/`: the `ClassIdMergedFlow` case under each binary (`rec2` is S10).
- `runs/{rec2,both}/t1800/index-graph.dot`, `dot_edges.py`, `index-graph-perfn-rec2-both.txt`:
  the index graphs and their comparison.
- `repin/`: how the apk report counts were re-pinned.

## M3: S10 and S11 across the corpus

`/Volumes/Shampoo/ct-bigapk/small/r8-general/RESULTS.md`.

## M4: `greenbits`

All output is under `/Volumes/Shampoo/ct-bigapk/small/greenbits-probe/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `bin/ctadl` (with `bin/ctadl.diff`, uncommitted, on `92fac26b`) is the probe binary for the
  ladder. `bin/ctadl-v1` made `runs/{decision,none}-t240`. `src/` is the probe's detached git
  worktree, and `run.sh` indexes the r8-general `both` import. The probe's
  `CTADL_DECISION_CENSUS=<dir>` writes `decisions.tsv`, `critical_summary.tsv`, `callee_info.tsv`,
  `context_sets.tsv` and `establishes_{direct,via}.tsv`.
- `runs/t{10,20,40,80,160,320}/`: the ladder. `index.err` is the full log, and `census/` is the
  decision dump. `runs/t{80,320}/rank.txt` come from `../cpuinfo-blowup/rank.py`, and
  `runs/t320/idx.txt` from `idx.py`.
- `runs/guard55/`: no timeout, killed at the 55 GiB guard at 440 s.
- `runs/{decision,none}-t240/`: the hybrid-context A/B.
- `marginal.txt` (from `../cpuinfo-blowup/marginal.py`), `memgrowth.txt`: marginal cost and
  memory per relation across the rungs.
- `typecheck.py <decisions.tsv>`: classifies each decision against `smali/out`, the app's
  baksmali output. `runs/*/typecheck.txt` holds its output.
- `bin/ctadl-focus` (`bin/ctadl-focus.diff`): the probe plus `CTADL_FOCUS=<dir>:<substr>|...`,
  which dumps `call`, `summary`, `assign_like` and `locals` rows for the named functions (serial
  engine only). `runs/focus-t40/focus/*.tsv` is the trace in P10.
- `bin/ctadl-entry` (`bin/ctadl-entry.diff`): the probe with the entry write-back for every
  language. `runs/entry-guard55/`: the `greenbits` run with S16, with `typecheck.txt` and
  `idx.txt`. `runs/scoped-guard55/`: the same run with the per-language build; identical.
- `greenbits-probe/impossible/` (see `RESULTS.md` there): the 1,324 decisions still impossible. To reproduce, run
  `python3 typecheck2.py runs/entry-guard55/census/decisions.tsv runs/entry-guard55/impossible.tsv`,
  then `impossible/{bodies,classify,origins,roots}.py`, from `greenbits-probe/`.

S16's measurements are under `/Volumes/Shampoo/ct-bigapk/small/paramflow-entry/`:

- `bin/ctadl-{base,entry,scoped}`: the probe (`92fac26b`), the probe with the write-back for every
  language, and the branch build with it for JVM bytecode only (`bin/ctadl-scoped.diff`).
- `reg-{base,entry,scoped}/`, `sarif-diff.txt`, `sarif-diff-scoped.txt`: the regression suite and
  its comparisons. `chess-repin-{base,scoped}/`: the chess case with the re-scoped sink.
- `chess-app-ir.txt`: the chess app's IR dump, for the `Lt2/l;->F` and `Lx2/c;->y` bodies.

## M5: impossible decisions across the corpus

All output is under `/Volumes/Shampoo/ct-bigapk/small/impossible-corpus/`: `RESULTS.md`,
`table.md`, `ladder.log`, and per app `<pkg>/runs/entry/` (index log, census, typecheck) and
`<pkg>/impossible/`. Reproduce with `run.sh`, `smali.sh` and `analyze.sh <pkg>`, then
`summarize.py`.

## M6: `cash.p.terminal`

All output is under `/Volumes/Shampoo/ct-bigapk/small/cash-terminal/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `bin/ctadl-a84d75e7`: the head binary. `bin/ctadl-norec1`, `bin/ctadl-norec1.diff`: head with
  `76982fc4` reverted.
- `run.sh`, `run2.sh` (takes `B=<binary>` and logs the driver's `[mem cp]` checkpoints),
  `ladder.sh`: these index the r8-general `both` import.
- `runs/{full,t10,t20,t40,t80,t160,t240,norec1-full}/index.err`: the full debug logs.
- `marginal.txt` (from `../cpuinfo-blowup/marginal.py`), `mem-by-rung.txt`: marginal cost and
  memory per rung. `idx-*.txt` (from `../greenbits-probe/idx.py`): the index breakdowns.
- `ir-bytes.txt`: the size of the import's IR.

## M7: small apps with native code

All output is under `/Volumes/Shampoo/ct-bigapk/small/full-corpus/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run. `budget.py`,
  `budget.txt`: the budget table.
- `import.sh`, `index.sh`, `one.sh`, `imports.sh`, `indexes.sh`, `ladder-over.sh`,
  `ladders-more.sh`: the harness. `order.txt` and `more.txt` list the apps.
- `<pkg>/import/`: the import. `<pkg>/runs/<run>/index.err`: the full debug log of each run
  (`full`, `t<N>`), with `rank.txt` from `../cpuinfo-blowup/rank.py`. `<pkg>/marginal.txt` (from
  `../cpuinfo-blowup/marginal.py`): the ladder's marginal costs.
- `ru.hugeping.reinstead/ssa-census-libmain.txt`, and the same for `heartratemonitor`'s libraries:
  from `ctadl-import/examples/ssa_census.rs`.
- `hashengineering.darkcoin.wallet/import-cap50/`: the failed import, with its 7.8 GB of facts.
- `../more-picks.json`, `../fetch-more.py`, `../fetch-more.log`: the 35 new apps and their
  download, each checked against the F-Droid index's sha256.

## M8: the demangler

All output is under `/Volumes/Shampoo/ct-bigapk/small/edge-delta/`:

- `RESULTS.md`: a summary. `ladder.log`: time and peak memory for every run.
- `bin/ctadl`: the probe, `a84d75e7` plus `bin/ctadl.diff` (uncommitted; worktree `src/`). It adds
  `CTADL_EDGE_CENSUS=<dir>`, which writes `totals.tsv`, `by_function.tsv`, `by_callee.tsv`,
  `by_caller_callee.tsv`, `hot_keys.tsv`, `summary.tsv` (every summary row) and `rows.tsv` (every
  `assign_like` row, with its origin, callee, splits and pairs). Serial engine only.
- `run.sh <pkg> <name> <timeout>`: indexes the `full-corpus` import with the census on.
- `runs/com.termux.nix/{t40,t160,t320,full}/`: the ladder, each with `index.err`, `rank.txt` and
  `census/`. `runs/com.termux.nix/late-t160-full.txt`: output of `late.py t160 full`, the
  per-function and per-callee growth between two rungs.
- `runs/{com.annie.dictionary.fork,com.androidcrypt,me.zhanghai.android.untracker}/full/` and
  `runs/invalid.lena.scrcpy/t640/`: the other apps.
- `window.py <census>`, `window.txt`: the work share in the 128 KB after `__cxa_demangle`.
- `demangler-scan.txt`: which arm64 libraries of each app contain the demangler.
  `no-demangler.txt`: the edge-delta rule's share of rule time in the apps without it.
- `lib/liblocal-socket.so`: termux's library, from the APK.

S6's measurements: `/Volumes/Shampoo/ct-bigapk/small/demangle-model/RESULTS.md`.
