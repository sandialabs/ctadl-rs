# Spec — Ladder of call resolution strategies — DO-NOT-MERGE

Requirements and design for `intent.md`, against this branch (`cha-with-surgical-fallback`,
import format 7). Every number quoted here comes from `cha-viability.md` and is not re-derived.
Code references were checked against the branch at the time of writing.

Audience: the engineers who will build it. §1–3 are the contract, §4–13 the design, §14 the
test plan, §16 the order to build it in. **Read §15 first if you read one section**: it lists
where the intent, the viability analysis and the codebase's existing policies disagree, and
what this spec decided in each case.

---

## 1. What this changes, in one paragraph

Today `--strategy mixed` resolves a Java virtual call with CHA when the site has exactly one
target and defers every other site to hybrid inlining: 15.8–40.6% of virtual sites, median
27.0% (`ctadl-ascent/src/codegen/mod.rs:547`). This replaces that test with a three-rung ladder
per call site: (1) a **dispatch model** matched on the site's static signature, which emits one
summary at the site and no targets; (2) a **threshold** `K`, under which the site gets ordinary
CHA edges; (3) **hybrid inlining** for the rest. Simulated at `K = 32`, the median real app's
call graph is 23x smaller than plain CHA and 2.2x what `mixed` emits today, with hybrid
inlining on a median 1.63% of virtual sites instead of 27.0%. Alongside it: `invoke-super`
resolves to its one real target, the single-abstract-method test closes over super-interfaces,
and every Java call site is counted into one of four buckets on the index summary line.

---

## 2. Requirements

**I-n** is the nth bullet of `intent.md` (twelve bullets).

### 2.1 Functional

| # | Requirement | Source |
| --- | --- | --- |
| **R1** | Java virtual call sites are classified by one function at codegen time, in the order *dispatch model → threshold → hybrid inlining*. | I-8, I-1 |
| **R2** | A new generator form, `find: "dispatch"`, whose `where` is evaluated against the **call site's** declared class, simple name and descriptor, and whose `model` accepts the ordinary `propagation` list. | I-6 |
| **R3** | A matched dispatch model replaces the site's target set: one `call` row to a synthetic per-signature function carrying the summary, and no CHA edges. An **empty** propagation list emits nothing. A model may instead carry `resolve: "inline"`, which sends the site to rung 3 regardless of `K` unless it has exactly one target. | I-6; viability §"How it hooks up"; §15.3 for `inline` |
| **R4** | A dispatch model is **refused** for a signature whose CHA target set contains a matched source or sink function; the site falls to rung 2. Refusals are reported. | viability §"One consequence that has to be stated"; assumes **A1**; residue in §15.1 |
| **R5** | `K` is one CLI flag and one recorded index-config field, default **32**. | I-9 |
| **R6** | Model-first is the default rung order; threshold-first is a flag. | I-9 |
| **R7** | Interface-dispatched sites are configurable separately from class-virtual ones: their own threshold and their own model on/off. | I-11 |
| **R8** | `invoke-super` (dex) and `invokespecial`-with-receiver (jvm) resolve to the single appropriate method, for a superclass target and for an interface default-method target. When the hierarchy cannot produce exactly one, the site falls to the ladder. | I-2 |
| **R9** | The structural single-abstract-method test is computed over a type's **transitive super-interface closure** and lives in one place that `report` and codegen both read. | I-7 |
| **R10** | A by-name closure-shaped list ships as data in the defaults file. | I-7 |
| **R11** | Every Java call site lands in exactly one of four buckets — **modelled, skipped, CHA, inlined** — and the counts print beside the existing `models: …` summary line. | I-12 |
| **R12** | `ctadl report` accepts extra model files and accounts for the built-in ones, so a user can see which of their app's signatures the policy covers. | I-4 |
| **R13** | Shipped defaults follow the disposition in viability §"What the carved-out calls should be handed to" for every signature there. One deviation: `close`/`dispose` go to hybrid inlining, not a model (§11.3, §15.3). | I-5 |
| **R14** | Hybrid inlining's soundness gap stays open: an unresolved call is unresolved. | I-10; consequences in §15.2 |

### 2.2 Non-functional

- **N1 Determinism.** Same import, flags and model files ⇒ byte-identical fact tables. Model
  iteration order, synthetic-function naming and bucket counting are order-independent.
- **N2 Baseline preserved.** `--strategy cha` and `hi` keep their meaning. Today's `mixed` stays
  reachable as `--strategy legacy-mixed` so the viability A/B can be re-run on one binary.
- **N3 Streaming preserved.** The import loop drops each import's IR and match index before
  loading the next (`cli/mod.rs:160–181`). Nothing here retains per-*site* state across
  imports. Per-*signature* state is fine (10^4–10^5 signatures against 10^6–10^7 sites).
- **N4 One evaluator.** `find: dispatch` reuses the `where` evaluator and universe sets in
  `models/json.rs`. No second `signature_match` (`models/match_index.rs:11`).
- **N5 Reproducibility.** The policy an index was built under is recorded and reported at
  query time.

### 2.3 Out of scope

- **I-3**, a query-phase option listing calls that resolve to nothing. Not a work item. §10.1's
  zero-target sub-count is what exists in the meantime.
- RTA as a strategy: removes a median 3.7% of edges on large apps. Stays a measurement.
- Per-call-site and per-callee configuration: both measured and rejected. The unit is the
  site's static signature.
- Closing hybrid inlining's soundness gap (R14).

---

## 3. Definitions and assumptions

- **Signature key** — the `(cls, simple_name, descriptor)` triple a `CallStyle::JavaCall`
  carries, spelled canonically as `Lcls;->name(desc)`, the same as a `JavaMethod` id, so
  `qualified-id` matching works unchanged.
- **Resolvent set** — `ClassHierarchyAnalysis::java_resolvents(key)`. A function of the key,
  not of the site.
- **Excess** — `max(0, |resolvents| − 1)`.
- **Dispatch model** — a `find: "dispatch"` generator. Matches a key, not a function.
- **Rung** — a step of the classifier. **Bucket** — one of the four counted outcomes.

**A1 — the source/sink model file is passed to `ctadl index` as well as to `ctadl query`.**
R4 intersects a key's targets with `ProgramModelMatches::endpoints`, and `index` only has the
endpoints it was given. The documented workflow (`docs/model-generators.md:55–70`, README) is
"index once, query many times with different source/sink files", and `cli/mod.rs:264–277`
warns that `index` *ignores* endpoint models. A1 changes that workflow. §9.1 and §9.2 make it
visible when A1 does not hold; §15.1 has what remains when it does.

**A2 — re-importing every existing store is acceptable.** §7.3 bumps `IMPORT_FORMAT_VERSION`
to `8`. Cost in §15.6.

---

## 4. The classifier

### 4.1 Placement

One function, called from the `CallStyle::JavaCall` arm of `CodegenVisitor::visit_statement`
(`codegen/mod.rs:507`), replacing the `resolvents.len() == 1` test at `:547`:

```rust
/// What a site's static signature, dispatch kind and CHA target count decide.
enum SiteAction {
    /// Rung 1, non-empty propagation: one `call` row to the key's synthetic function.
    Model(SynthFunctionId),
    /// Rung 1, empty propagation: emit nothing, count it.
    Skip,
    /// Rung 0 or rung 2: the existing `CallResolutionStrategy::Cha` body (`:523`), over the
    /// given targets.
    Cha,
    /// Rung 1 `inline` disposition, or rung 3: the existing `Hi` body (`:534`).
    Defer,
}

fn classify(&mut self, key: &SignatureKey, dispatch: JavaDispatch, super_start: Option<&Symbol>,
            resolvents: usize) -> SiteAction
```

`Cha` and `Defer` are the existing bodies; only `Model` and `Skip` are new. A `SiteAction` is
returned for **every** Java site, zero-resolvent ones included, so R11 holds by construction.

### 4.2 The ladder

**Rung 0 — super resolution.** If `dispatch == Super` and §7 yields exactly one target, emit
that one `call` row and count the site as **CHA** (it is an exact CHA edge; a fifth bucket
would break R11). Otherwise fall through with the ordinary resolvent set. Rung 0 precedes rung
1 and does not move under `--dispatch-order`: a super call's target is known, and modelling a
known call is strictly worse than resolving it.

**Rung 1 — dispatch model.** The key matched a dispatch model **and** models are enabled for
this dispatch kind (§9.1). By the model's disposition (§5.2):

- `resolve: "inline"` ⇒ `Defer`, counted **inlined** (sub-count `inlined_by_model`, §10.1),
  **unless the key has exactly one CHA target**, in which case ⇒ `Cha`. Inlining a
  monomorphic site gains nothing and can lose the edge when the receiver's allocation is not
  visible (R14). Zero targets still defer: hybrid inlining can find a callee from the allocated
  class where the static type resolves to nothing. No R4 check: inlining hides no callee.
- non-empty `propagation`, and the R4 intersection is empty ⇒ `Model`, counted **modelled**;
- empty `propagation`, and the R4 intersection is empty ⇒ `Skip`, counted **skipped**;
- R4 refused ⇒ fall through.

Rung 1 ignores the target count: that is what model-first means and what takes a median 16.1%
of sites out of the engine.

**Rung 2 — threshold.** `resolvents.len() <= K_effective` (§9.1) ⇒ `Cha`, counted **CHA**.
This includes 0 and 1 targets. A zero-resolvent site emits no rows, as today, and is counted
under CHA with its own sub-count (§10.1).

**Rung 3** ⇒ `Defer`, counted **inlined**.

`--dispatch-order threshold-first` (R6) moves the propagation and skip dispositions behind the
threshold. An `inline` disposition is honoured in both orders: its purpose is to keep the site
off CHA, and threshold-first would silently undo it for every site at or under `K`.

### 4.3 Per key, not per site

The target count, the rung-1 match and the R4 intersection are all functions of the key.
Compute each once per key into a per-program `HashMap<SignatureKey, SiteAction'>` (the action
modulo dispatch kind), and let `classify` do a lookup plus the dispatch-kind test. Sites
outnumber keys by two to three orders of magnitude; this is what keeps N3 affordable.

---

## 5. `find: "dispatch"`

### 5.1 Why not `find: callsites`

`find: callsites` selects sites **by the callee they resolve to** — the wrong end, since
resolving is what the ladder avoids — and rejects `propagation` (`models/json.rs:1759`) because
a summary is a property of a function. `find: dispatch` is its sibling: `where` is evaluated
against the site's declared class, name and descriptor; `model` carries `propagation`.

Second reason: the method universe is built from VMT `methods`, i.e. **implementations**
(`match_index.rs:78`). Half of interface sites name a type the app never declares (median
50–54%; 432,334 sites on TikTok), so `java.util.Iterator` has no row there and `find: methods`
cannot name it. The dispatch universe is the keys the IR's call sites mention, which contains
undeclared types by construction.

### 5.2 Surface

```jsonc
{ "find": "dispatch",
  "where": [ { "constraint": "signature_match", "name": "toString",
               "parents": ["Ljava/lang/Object;"] } ],
  "model": { "propagation": [ { "input": "Argument(*)", "output": "Return" } ] } }
```

- `where` accepts the constraints that are functions of a signature: `signature_match`
  (`name`/`names`, `parent`/`parents`, `qualified-id`/`qualified-ids`), `name`,
  `signature`/`signature_pattern`, `parent`, `extends`, and `any_of`/`all_of`/`not`.
- `where` **rejects with a load error naming the constraint**: `in_function` (the policy is
  deliberately not per-site), `has_code`, `number_parameters`, `uses_field` (need a
  `FunctionData`; a key has none — `match_index.rs:56–61`).
- `model` accepts **exactly one** of `propagation` and `resolve`. `sources`, `sinks`, `taint`,
  `modes`, `bridge`, `access_paths`, `forward_self` are load errors: an endpoint lives on a
  function and there is none here; `modes` is a property of a body.
- Empty `propagation` is the spelling of "skip this call" and must be written explicitly
  (`"propagation": []`). Neither key present is a load error, so "forgot the model" and "mean
  to discard" are different documents.
- `"resolve": "inline"` sends every site of the key to hybrid inlining regardless of `K`,
  except a site with exactly one CHA target, which stays exact (§4.2). It is the disposition
  for a signature whose bodies must stay reachable (a sink lives inside them) but whose target
  set is too wide or too unrelated for CHA: `close`/`dispose` (§11.3). The only value is
  `"inline"`; `"cha"` (force CHA above `K`) is not added until someone needs it.
- `in` (`ProgramScope`) applies unchanged.
- Add `"dispatch"` to the `find` enum in `models/ctadl-model-generator.schema.json:398`, and a
  section to `docs/model-generators.md` that states what a dispatch model hides (§15.1) before
  it states what it saves.

### 5.3 Matching mechanics

1. **Universe.** `ProgramMatchIndex` gains a lazily built dispatch universe, populated only when
   some loaded generator has `find: "dispatch"`: `dispatch_keys: Vec<(cls, name, desc)>`, the
   distinct keys of every `JavaCall` in the program, plus `dispatch_by_name`, `_by_parent`,
   `_by_signature`, `_by_qualified_id` in the same `HashMap<&str, Vec<&str>>` shapes as the
   method universe (`match_index.rs:39–54`). One extra pass over statements before SSA, while
   the IR is in hand; `ctadl report` already does this pass (`report/callgraph.rs:588`) at
   acceptable cost on the largest app.
2. **Evaluator.** `CurrentSet` (`json.rs:355`) gains a `Dispatch` variant; `target_set_mut`
   (`:531`) points at the dispatch universe when `find_method[n] == FindMethod::Dispatch`.
   No constraint implementation changes — N4.
3. **Output.** A new field on `ProgramModelMatches` (`matches.rs:210`):

   ```rust
   /// Keys a `find: "dispatch"` generator matched.
   pub dispatch: BTreeMap<DispatchKey, DispatchModel>,          // BTreeMap: N1

   pub struct DispatchModel {
       pub disposition: Disposition,
       /// `file:generator-index` of every contributing generator, for diagnostics.
       pub provenance: Vec<String>,
   }
   pub enum Disposition {
       /// Rung 3 regardless of `K`.
       Inline,
       /// Non-empty: one synthetic summary at the site. Two generators matching one key union
       /// their lists, as two generators matching one function do today.
       Model(Vec<(ModelPort, ModelPort)>),
       /// Empty propagation: target set discarded, nothing flows.
       Skip,
   }
   ```

   Precedence when several generators match one key: **`Inline` > `Model` > `Skip`**, the one
   that keeps the most of the program reachable. A user overrides a shipped skip by adding a
   propagation, and a shipped model by adding `resolve: inline`; going the other way needs
   `--no-default-models`. The §12 diagnostics say which generators contributed.

### 5.4 Ordering inside `ctadl index`

The import loop matches an import before it codegens it, and phase 1 already reads one
`ProgramModelMatches` field mid-loop, `skip_analysis` (`cli/mod.rs:204–210`). Dispatch matches
follow that precedent: matched in the per-import block (`:163–180`), read by `codegen_program`
for the same import. The synthetic functions' summary rows are emitted in phase 2
(`codegen/model_matches.rs`) after every import; phases agree on the synthetic function by
**name**, and `get_or_add_function` interns one name to one id from either side.

---

## 6. Dispatch models become facts

`IndexFacts::summary` is a base relation and phase 2 already writes matched propagations into
it (`model_matches.rs:101`). No inference rule changes.

### 6.1 The synthetic function

- **One per matched key**, not per generator or per site. Per-key is what makes `Argument(*)`
  well defined: the descriptor fixes the arity.
- Name `ctadl$dispatch$Lcls;->name(desc)`. The prefix cannot collide with a dex or jvm method
  id, both of which begin with `L`.
- Interned via `source_info.sites.get_or_add_function` like every callee. It gets an
  `external_function` row, the `formal_param` rows its summary mentions
  (`codegen_model_matches` already emits those, `model_matches.rs:126–140`), and nothing else.
- The header of `codegen/model_matches.rs:15` says "nothing synthesizes a function". This is the
  first thing that does; update the header.

### 6.2 At the site

`Model(f)`: push exactly one row, `facts.call.push((site, f))`. The receiver is already actual
arg 0 (`codegen/mod.rs:516`); `actual_param`, return and globals rows come from the shared tail
of the `CallAssign` arm (`:657–727`) regardless of strategy. So `Argument(0)` is the receiver,
`Argument(n)` the nth argument, `Return` the return: the same port semantics as a `find: methods`
propagation, which is what lets §11's defaults be copies of existing entries with `find` changed.

`Skip`: push nothing. `Defer` from an `inline` disposition: the rung 3 body, `callee_info` and
no `call` rows. No synthetic function is interned for a skip-only or inline-only key.

### 6.3 Cost

One summary row per key and one `call` row per site, against up to 21,257 `call` rows per site
under plain CHA. It also **removes the callee subtree from the analysis at that site** — the
real cost, and the reason for R4 (§15.1).

---

## 7. `invoke-super`

### 7.1 The defect

The instruction names the class to start lookup at; CTADL resolves it as though the receiver
were unknown, returning every implementation *below* `cls`. On TikTok, 49,774 super sites of
which 311 resolve to one target (4.4%, against 81% for virtual calls); the worst,
`UIAssem.onViewCreated`, carries 1,543 targets for an instruction whose target is fixed.

### 7.2 The resolution

The upward lookup already exists inside `run_cha` as the ascent relation `cha_super_method`
(`codegen/mod.rs:1185`, rules at `:1208–1211`): "the implementation class `c` inherits", which
is JVM/Dalvik lookup starting at `c`. It is not exported because `cha_resolve` is what codegen
wanted, and **it must not be materialised** (it is class × inherited method for the whole
program). Instead `ClassHierarchyAnalysis` (`:958`) keeps two small maps built from `run_cha`'s
own inputs, and answers super queries by a memoised walk:

```rust
/// `(cls, name, desc) -> the implementation `cls` itself declares`. Built from
/// `method_implemented` rows only: an abstract declaration has no body to resolve to.
declared: HashMap<SignatureKey, Symbol>,
/// `cls -> its direct parents` (superclass and super-interfaces, as the VMT `hierarchy` gives it).
parents: HashMap<Symbol, SmallVec<[Symbol; 2]>>,
/// Memo keyed by start signature. Bounded by distinct super-call signatures, ~1% of virtual ones.
super_memo: RefCell<HashMap<SignatureKey, SuperResolution>>,

enum SuperResolution { Exactly(Symbol), None, Ambiguous(usize) }
```

`super_resolvent(start, name, desc)`: breadth-first walk up `parents`, **level 0 being `start`
itself**, collecting `declared[(c, name, desc)]` at the first level where any class declares it
and stopping there. `Exactly` when that set is a singleton. This covers both R8 cases: a
superclass target (the walk climbs the class chain) and an interface default method for
`X.super.m()` (the walk starts at `X`, which declares the default, so level 0 is the answer).

### 7.3 What the frontends must supply

`cls` on a `JavaCall` is the class of the *method reference*, which is not always where the
runtime starts:

- **Dalvik** `invoke-super` starts at the superclass of the class declaring the current method.
  d8 usually puts that class in the method reference, but not always; when it names the
  *current* class, a walk from `cls` finds the current method and the call resolves to itself.
- **JVM** `invokespecial` under `ACC_SUPER` has the same rule for a superclass method. The jvm
  frontend also lowers constructors and private calls to `JavaDispatch::Super`
  (`frontends/ctadl-jvm/src/lib.rs:651`); for those the named class is the exact target class,
  and the same walk gives the right answer. The bucket counts will include them and the two
  frontends will not be comparable on the super row; pre-existing, documented on
  `JavaDispatch::Super`, not fixed here (§15.5).

So **add one field to `CallStyle::JavaCall`**:

```rust
/// For `dispatch: Super`, the class the runtime begins method lookup at. `None` for every
/// other dispatch kind and for a `Super` site whose frontend could not determine it. Kept
/// beside `cls` rather than overwriting it: `cls` is what a model matches and what
/// `ctadl report` counts.
super_start: Option<Symbol>,
```

Both frontends have the enclosing class's superclass and interface list in hand while lowering
(`ctadl-dex/src/lib.rs:169–195`, `ctadl-jvm/src/lib.rs:163–180`). The rule: if `cls` is an
interface the enclosing class directly implements, or (jvm) the call is a constructor or private
call, `super_start = cls`; otherwise `super_start = ` the enclosing class's direct superclass.
Codegen's rung 0 uses `super_start.unwrap_or(cls)`.

**Bump `IMPORT_FORMAT_VERSION` to `8`** (`ctadl-import/src/project.rs:106`) with a history
entry in the doc comment above it:

> - `8`: `CallStyle::JavaCall` gained `super_start`, the class the runtime begins lookup at for a
>   `Super` dispatch, so an `invoke-super` resolves to its single real target instead of every
>   implementation below the named class. A `bitcode` wire-format change to every
>   `ir-program.bitcode`.

Without the bump the positional `bitcode` decoder fails opaquely on a pre-change store. With it,
a stale store fails with "re-import". Sequence the bump as §16 step 1 so it happens once.

### 7.4 Soundness

A walk that finds nothing (missing superclass, framework not imported) returns `None`, and the
site falls through with its full CHA resolvent set. It never emits an empty target set CHA
would have filled. State this in the code comment: "fixing super" reads like a precision change
and this is what makes it safe.

---

## 8. Closure-shaped tests

Both forms are **diagnostics and defaults-authoring aids, not a rung**. Gating rung 3 on a
closure-shaped receiver was measured and rejected: it collapses on large apps (TikTok: 175,598
residual sites, 302 million edges against the policy's 7.06 million). The threshold covers that
hole. Do not reintroduce the closure test as a gate.

### 8.1 Structural (R9)

Move `TypeFacts::from_vmt` (`report/callgraph.rs:748`) into `ctadl-ir`'s `call` module so
`report` and codegen read one definition, and fix it while moving. Today it is "interface with
exactly one declared abstract method", which misses `dagger.internal.Provider` and
`dagger.internal.Factory` (DuckDuckGo: 2,918 targets over 10,031 sites, 29.3M excess edges from
`Provider` alone) because their one method is declared on the interface they extend.

Corrected: `I` is single-abstract-method iff `I ∈ interfaces` and

```
| { (n,d) : (C,n,d)   ∈ abstract_methods, C ∈ closure(I) }
  \ { (n,d) : (C,n,d,_) ∈ methods,        C ∈ closure(I) }
  \ OBJECT_METHODS |  == 1
```

with `closure(I)` = `I` plus its transitive parents in `hierarchy`, restricted to `interfaces`.

- The **implementation subtraction** is necessary: `methods` holds interface *default* methods,
  which are not abstract. Without it a one-abstract-one-default interface reads as two.
- **`OBJECT_METHODS`** = `toString()Ljava/lang/String;`, `equals(Ljava/lang/Object;)Z`,
  `hashCode()I`. Java's own functional-interface rule excludes them, and interfaces redeclare
  `equals` for documentation.

Report the corrected count in `functional_interfaces`, keeping `interface_sites_on_unknown_type`
beside it: an undeclared interface is still invisible to this test, which is why §8.2 exists.

### 8.2 By name (R10)

Framework names shipped as **data in the defaults JSONL**: `Runnable`, `Callable`,
`kotlin.jvm.functions.FunctionN`, `java.util.function.*`, the RxJava `Observer`/`Subscriber`
family, `javax.inject.Provider`, `dagger.internal.Provider`, `dagger.internal.Factory`,
`android.os.Parcelable$Creator`, and Kotlin's `invokeSuspend`/`create` continuation pair. These
are names an obfuscator cannot touch.

### 8.3 What they are for

(a) Naming what rung 3 is *for*, in the report and docs. (b) Letting `ctadl report --models`
(§12) say "these 12 signatures own 40% of your excess, are single-abstract-method, and no
shipped default names them": the per-app profiling that a fixed list leaves 6–9 points of
benefit on the table for. The structural test flags `LX/09A;->invoke` (1,989 targets),
`LX/0sSp;->invoke` (946) and `LX/SJG;->LB` (637) without knowing a name; across the 23 real apps
it flags 237 of the apps' 100-worst signatures.

---

## 9. Configuration

### 9.1 CLI (`ctadl index`, mirrored on `ctadl go`)

| Flag | Default | Meaning |
| --- | --- | --- |
| `--cha-threshold K` | `32` | Rung 2 threshold for virtual, super and unknown-dispatch sites. `0` disables rung 2; a very large value disables rung 3. |
| `--cha-threshold-interface K` | = `--cha-threshold` | Rung 2 threshold for `Interface` sites (R7). |
| `--dispatch-models` / `--no-dispatch-models` | on | Rung 1 on/off. |
| `--no-dispatch-models-interface` | off | Rung 1 off for interface sites only (R7). |
| `--dispatch-order <model-first\|threshold-first>` | `model-first` | R6. |
| `--strategy <mixed\|cha\|hi\|legacy-mixed>` | `mixed` | `mixed` **is** the ladder. `legacy-mixed` is today's `len() == 1` rule (N2). |

`K` is not delicate: between 16 and 32 the inlined share falls by more than half while the
graph grows by a sixth; past 32 both flatten.

**The ladder lives only in the `Mixed` arm.** `classify` is called from nowhere else, so under
`cha`, `hi` and `legacy-mixed` the `JavaCall` arm bodies are untouched: no rung 0 (a pure CHA
run still resolves `invoke-super` to every implementation below the named class, which keeps the
viability A/B's `cha` column comparable), no dispatch models (they are loaded and matched but
never consulted), no threshold. The bucket line still prints under every strategy, since
counting happens where rows are emitted. Giving `--cha-threshold*`, `--dispatch-models*` or
`--dispatch-order` with a strategy other than `mixed` is a `warn`, and the `call_policy` stamp
records the flags as given.

### 9.2 Recorded in the index config (R5, N5)

`ctadl_import::project::IndexConfig` (`project.rs:160`) holds only `version`, so an index built
with `--strategy cha` is indistinguishable at query time from one built with `mixed`. Extend it:

```rust
pub struct IndexConfig {
    pub version: String,
    /// `None` for an index written before this field.
    #[serde(default)]
    pub call_policy: Option<CallPolicyRecord>,
}
pub struct CallPolicyRecord {
    pub strategy: String,              // "mixed" | "cha" | "hi" | "legacy-mixed"
    pub cha_threshold: usize,
    pub cha_threshold_interface: usize,
    pub dispatch_models: bool,
    pub dispatch_models_interface: bool,
    pub order: String,                 // "model-first" | "threshold-first"
    /// SHA-256 over the endpoint-declaring model files given to `ctadl index`, sorted. Empty
    /// when none. Lets `ctadl query` warn when its sources and sinks are not the ones R4 ran
    /// against (§15.1).
    pub endpoint_model_digest: String,
}
```

`check_index_config` (`project.rs:705`) keeps rejecting only on `version`. The policy record is
**reported, not enforced**, except the digest, which warns at query time.

**Where `K` lives.** There are two things called `IndexConfig`: `index_engine::IndexConfig`
(`index_engine/mod.rs:297`, engine parameters) and the on-disk stamp above. `K` is consumed at
codegen and never reaches the engine, so it belongs to neither: it travels in a new `CallPolicy`
struct handed to `codegen_program` beside `strategy`, and is *recorded* in the stamp. That is
one flag and one recorded field, as the intent asks.

---

## 10. Bucket accounting (R11)

### 10.1 The counter

```rust
/// Every Java call site lands in exactly one of the four. The invariant is asserted:
/// `modelled + skipped + cha + inlined == java_sites`.
#[derive(Default, Debug, Clone, Copy)]
pub struct SiteBuckets {
    pub modelled: usize,
    pub skipped: usize,
    pub cha: usize,
    pub inlined: usize,
    // Sub-counts inside the buckets above, not additional buckets.
    pub cha_zero_targets: usize,   // ⊆ cha: resolved to nothing
    pub cha_super_exact: usize,    // ⊆ cha: rung 0 found the single target
    pub inlined_by_model: usize,   // ⊆ inlined: a `resolve: inline` entry, not the threshold
    pub java_sites: usize,
}
```

Kept per `JavaDispatch` kind as well (`[SiteBuckets; 4]` via `JavaDispatch::index()`), since R7
makes interface sites a separate population and `report` already uses this convention.

The sub-counts stay inside their buckets because the intent requires exactly four and a
zero-target site genuinely is on the CHA rung. Reporting it is how the 0.04–0.3% of sites that
resolve to nothing stop being silent (today a `log::trace!`, `codegen/mod.rs:557–560`).
`inlined_by_model` exists for the same reason as the bucket line itself: a mis-scoped `inline`
entry moves sites to hybrid inlining silently, and its only other symptom is a slower index.

### 10.2 Where it prints

Beside `cli/mod.rs:252`, at `info`, unconditionally:

```
models: 1224 summary row(s), 0 declared access path(s), 0 function body(ies) not analyzed
calls: 1,088,334 java site(s): 200,254 modelled, 42,445 skipped, 830,833 CHA (2,118 with no
       target, 12,904 exact super), 14,802 inlined (1,632 by model)
  virtual:   612,904 sites: 141,993 modelled, 30,110 skipped, 434,109 CHA, 6,692 inlined
  interface: 174,113 sites:  58,261 modelled, 12,335 skipped,  95,409 CHA, 8,108 inlined
  super:      13,116 sites:       0 modelled,      0 skipped,  13,116 CHA,     0 inlined
  unknown:         0 sites
```

Keep the intent's rationale in the code comment: without this line a mis-scoped dispatch model
silently swallows a signature and the only symptom is a missing finding.

### 10.3 The invariant

`debug_assert!` at the end of `codegen_program`; `log::error!` on mismatch in release. Returning
a `SiteAction` for every site makes it true by construction; the assertion keeps it true when
someone adds a rung.

---

## 11. Shipped defaults (R13)

In `models/defaults/java-index.jsonl`, beside the existing entries, mostly as a copy of an
existing `find: methods` entry with `find` changed: the propagation semantics for both modellable
buckets are already shipped; what was missing was attaching them to a site instead of a callee.

### 11.1 Dispatch models, non-empty (rung 1, modelled)

| Signature | Propagation | Notes |
| --- | --- | --- |
| `Object.toString` | `Argument(*) → Return` | Defaults line 24 is the `find: methods` twin. 17.7% of a median app's excess. |
| `Object.hashCode`, `Object.equals` | `Argument(*) → Return` | **Not** skips: the primitive is derived from the receiver's *contents*, and a configuration that follows an integer is entitled to. |
| `Object.clone` | `Argument(0) → Return` | |
| `Iterator.next` | `Argument(0).\[] → Return` | Defaults line 76 is the twin. |
| `List.get`, `Map.get`, `Collection`/`Deque`/`Queue` readers | `Argument(0).\[] → Return` | Defaults lines 75–96 are the twins. |
| `Map.put`, collection writers | `Argument(1) → Argument(0).\[]`, `Argument(2) → Argument(0).\[]` | Both arities: overloaded, and a generator matches by name. |
| `Iterable.iterator`, `List.iterator` | `Argument(0).\[] → Return.\[]` | |
| `Map$Entry.getKey`/`getValue`, `keySet`/`values`/`entrySet` | `Argument(0) → Return` | |

The `Object` models are faithful, not merely cheap: over 204 named-field writes across all four
contract methods in two apps, 181 went into an object reached from the receiver's own field, 13
into an object allocated in the body, 10 unattributed, zero into an argument or a static. Put a
pointer to `/Volumes/Shampoo/ctadl-sweep/sigstudy/purity.py` in the file's header comment.

### 11.2 Dispatch models, empty (rung 1, skipped)

`Iterator.hasNext`, `Collection.size`/`isEmpty`/`contains`/`containsAll`,
`Map.containsKey`/`containsValue`, `List.indexOf`/`lastIndexOf`, `Comparable.compareTo`,
`Comparator.compare` — named **on the interfaces and on the abstract bases**
(`AbstractCollection.size` alone is 21,550 sites in the corpus; the simulation names only the
interfaces and so counts those as modelled rather than skipped).

A median 2.6% of an app's virtual sites, up to 4.5%. This is the one lever that can silently
lose a finding, so it stays the shortest list and every entry must be defensible alone: the only
output is a primitive derived from the receiver's *shape*. **A `void` return is not evidence
that nothing happens** (`Runnable.run()V`, `Activity.onCreate(Bundle)V`). Primitives-in,
primitive-or-void-out signatures are 20.8% of the excess and 31.2% of the sites, far too big a
bucket to take wholesale, which is why the list is keyed on the contract, not the descriptor.

### 11.3 `close`/`dispose` — `resolve: "inline"` (deviation from the table row, §15.3)

One entry, matching the sweep classifier's resource bucket exactly (`sigstudy/buckets.py:50–55`)
so the shipped policy and the simulation agree on which sites it covers: `names` `close`,
`dispose`, `cancel`, `flush`, `release`, `recycle`, `shutdown` on `parents` `java.io.Closeable`,
`java.lang.AutoCloseable`, `java.io.InputStream`/`OutputStream`/`Reader`/`Writer`,
`java.util.concurrent.Future`, RxJava `Disposable`, `android.database.Cursor`, and okio's
`Source`/`Sink`/`BufferedSource`/`BufferedSink`.

Why inline and not a model: a `close()` body is where buffered data is finally handed to a file
or a socket, which is where a sink model lives. A model of any shape at the site keeps taint out
of that body, and R4's guard cannot see a sink reached *through* a target (§15.1). Hybrid
inlining keeps the body reachable and resolves the receiver precisely when its allocation is
visible, which for a stream it usually is. Why not CHA: 17% of Chrome's 110 `close` overrides
and 32% of NewPipe's 182 write a named field, and above `K` (worst case 176 targets) they would
defer anyway; forcing it makes the disposition the same at every site of the key. Cost: a
median 0.15% of virtual sites move from modelled to inlined (§15.3).

### 11.4 Left to rung 3 — no default entry

`FunctionN.invoke`, `Runnable.run`, `Provider.get`, `invokeSuspend`/`create`, and the generated
serializers (`ProtoAdapter.decode`, `TypeAdapter.read`, `Parcelable$Creator.createFromParcel`,
`TLObject.serializeToStream`): one-method interfaces whose implementations are unrelated
fragments of the program. No contract, nothing to model; the receiver's identity is exactly what
a context-sensitive technique recovers (`invoke` bodies write named fields 12.4% of the time, 73
of those to a non-receiver object, and call something 96% of the time). They exceed `K` and fall
to rung 3 by themselves. §8.2's name list is a diagnostic aid, not a rung. Do not write a
propagation model for them; that is the line the design is drawn on. They get no
`resolve: inline` entry either: on real apps it would change nothing, and on TaintBench, where
a `Function1.invoke` may have three targets, it would defer sites CHA handles exactly.

### 11.5 Left to rung 0 — no default entry

Android lifecycle and `super` dispatch (`UIAssem.onViewCreated`). Do not model them.

### 11.6 Everything else

79.59% of a median app's virtual sites and 3.1% of its excess. CHA at rung 2. Nothing ships. A
user's `--models` file extends or overrides the defaults per app: a fixed list gets 87–89% of
the benefit on real apps, each app's own best 20 would get 95–96%.

---

## 12. `ctadl report --models` (R12)

### 12.1 Flags

`ctadl report` reads an import and writes nothing. Keep that. Add `--models <FILE>`
(repeatable, same loader as `index`), `--no-default-models`, and `--cha-threshold`,
`--cha-threshold-interface`, `--dispatch-order` so the simulated policy matches the index the
user will run. The report builds a `ProgramMatchIndex` as `cli::index` does (`cli/mod.rs:165`),
loads defaults unless suppressed, loads the user's files, and evaluates only the `find: dispatch`
generators. It already has the keys: `Walk::keys` (`report/callgraph.rs:524`) is the same triple.

### 12.2 New section, "policy", Java programs only

```rust
pub struct PolicySection {
    pub buckets: SiteBuckets,
    pub by_dispatch: [SiteBuckets; 4],
    /// Plain CHA edges and edges under the policy; the ratio is the headline.
    pub cha_edges: usize,
    pub policy_edges: usize,
    /// Left on rung 3, ranked by excess, each row saying whether the threshold or an `inline`
    /// entry (with provenance) put it there. What a user writes dispatch models against.
    pub top_inlined: Vec<InlinedSignatureRow>,
    /// Matched by a dispatch model, ranked by excess removed, with provenance.
    pub top_modelled: Vec<ModelledSignatureRow>,
    /// R4 refusals. Empty unless endpoint-declaring models were given.
    pub refused: Vec<RefusedSignatureRow>,
    /// Single-abstract-method (§8.1) or on the name list (§8.2), and named by no model.
    pub unmodelled_closures: Vec<SignatureRow>,
}
```

`top_inlined` and `unmodelled_closures` are what satisfy I-4: a user runs
`ctadl report app --models mine.jsonl`, reads them, adds entries, re-runs in seconds with no
re-import.

### 12.3 A trap

`top_by_excess` drops every signature with zero excess (4,855 of antennapod's 29,365), so site
percentages computed from it are wrong. The policy section counts sites and must be built from
the complete key table. Say so in the code.

---

## 13. Scalability work that comes along

Neither is in the intent; both are implied by "faster and more scalable".

**13.1 `callee_resolvents` is emitted for the whole program.** `emit_callee_resolvents`
(`codegen/mod.rs:1133`) writes every CHA row into facts before the walk. Under the ladder,
hybrid inlining runs on ~1.63% of sites, so almost none of those rows can be joined by the
resolution rule (`index_engine/mod.rs:1475`). Collect the keys of sites that took `Defer`, and
emit `callee_resolvents` for those keys only in `finish_with_vmt` (`:264`, already after the
walk). Pure deletion of unjoinable rows; no result changes. Land it with the ladder so the A/B
measures one thing.

**13.2 `load_models` clones the whole `call` relation** (`models/codegen.rs:13`) once per import
for a rule that touches a handful of rows. Pre-filter it. **Optional**: pre-existing, independent
of the ladder; do it only if §14.3's A/B shows import-time peak mattering.

---

## 14. Testing

### 14.1 Unit

- `classify`: table-driven over (disposition ∈ {none, model, skip, inline}, dispatch kind,
  target count, super-resolvable) → `SiteAction`, for both orders; `inline` must give `Defer`
  under `threshold-first` at a target count under `K`, `Cha` at exactly one target, and
  `Defer` at zero.
- Disposition precedence: two generators on one key, each ordered pair, → the higher one wins
  and both appear in provenance.
- `super_resolvent`: superclass chain; interface default method; missing class (→ `None`,
  falls through); ambiguous diamond (→ `Ambiguous`, falls through); the self-loop case where
  the method reference names the current class and `super_start` is set.
- SAM closure: `dagger.internal.Provider extends javax.inject.Provider`; one abstract plus one
  default; one abstract plus a redeclared `equals`. Extend the fixture at
  `report/callgraph.rs:1545`, which already builds `abstract_methods` by hand.
- `find: dispatch` loading: each rejected constraint and `model` key gives the named error;
  empty `propagation` loads; `resolve: "inline"` loads; neither key, both keys, or any other
  `resolve` value errors.
- Bucket invariant: a synthetic program with one site of each shape, per dispatch kind.

### 14.2 End-to-end

- `ctadl-ascent/tests/dispatch_models.rs` in the style of `default_models.rs`: index a small dex
  fixture with a dispatch model; assert one `call` row to `ctadl$dispatch$…`, its summary rows,
  **zero** CHA rows at the site, and taint flowing through it.
- A `.tnt` fixture for the skip case: site exists, no callee, no flow.
- The inline case: a `close()` site with two CHA targets under `K`, a sink inside one body, and
  a `resolve: inline` entry; assert `callee_info` and no `call` rows at the site, and that the
  sink is reached when the receiver's allocation is visible.
- R4 refusal: a sink declared on a class inside the modelled key's target set; assert the model
  is refused, the site takes CHA, and the warning names both.
- `--strategy legacy-mixed` reproduces today's fact tables byte for byte on an existing fixture
  (N2's guard).

### 14.3 Regression and corpus

- `cargo test --workspace`; `cargo xtask regression --frontend c,lua,pcode` then `jvm,dex,jni`.
- **TaintBench, 38 apps, findings diffed per app.** Expect movement: those apps ship no
  framework, so shipped defaults cover 37% of their excess against 87–89% on real apps, and
  27.0% of their excess is lifecycle/`super` against 0.5%, so rung 0 moves most there. A finding
  that moves on TaintBench and nowhere else is not by itself a reason to change `K`.
- **The index A/B.** Re-run `ab.sh` on antennapod, newpipe and schildi under the 24 GiB guard.
  `mixed` today fails on newpipe (1,283 s) and schildi (139 s). Newpipe indexing at all is the
  first real evidence the design works; the simulation does not prove it (§15.7).
- **The 61-app sweep** re-imported at format 8, `ctadl report --format json` before and after,
  to confirm bucket shares near the simulation (median 16.1% modelled, 2.6% skipped, 1.63%
  inlined). Capture all output under `/Volumes/Shampoo/ctadl-sweep/`.

---

## 15. Areas of concern

Each is a place where two things this spec must honour disagree, with what was decided.

### 15.1 R4 needs endpoints at index time, and only sees direct targets

**The contradiction.** R4 (from the viability analysis) refuses a dispatch model whose targets
contain a matched source or sink. The codebase's documented policy is the opposite of what that
needs: sources and sinks are `query`'s input, `index` warns that it ignores them
(`cli/mod.rs:264–277`), the docs recommend "query before you index", and the README workflow is
index once, query many times with different endpoint files. Both cannot hold. **Decision: A1**
— the endpoint file is passed to `index` too — plus two things that make a violation visible:

1. Change the `index` warning: endpoints are not analysed at index time but they gate the
   dispatch models, so pass the same file to both commands.
2. Record `endpoint_model_digest` (§9.2) and warn at query time on mismatch: *"this index's
   dispatch models were checked against a different set of sources and sinks; a sink inside a
   modelled signature's targets will not match. Re-index with the same `--models`."*

**The residue, even when A1 holds.** The guard is an intersection over the key's *direct* CHA
targets with `ProgramModelMatches::endpoints`, which are functions. It catches a sink declared
*on* a target method. It does not catch a sink reached *through* a target's body (a `close()`
that calls `OutputStream.write`), and it cannot: making it transitive over the CHA graph would
refuse nearly every model, since `toString` bodies call out 84% of the time and most of the
program sits in one SCC. This is why §11.3 sends `close`/`dispose` to hybrid inlining instead
of modelling them, and it must be the first sentence of the `find: dispatch` user docs. Two
narrower gaps:

- Endpoints matched in a *later* import of the same `ctadl index` run are invisible when an
  earlier import's sites are classified. Restructuring the loop to match-all-then-codegen-all
  would give up N3. Bites only when two imports declare the same class.
- A `find: callsites` endpoint scoped to a caller inside a modelled callee is unreachable and no
  intersection sees it.

Make refusals visible whether or not they fire: the `refused` list in §12.2 and an `info` count
at index time.

### 15.2 Hybrid inlining's soundness gap stays open (R14) — consequences

The intent settles this; the viability analysis said the opposite ("should be settled before the
strategy is switched"). The intent wins. What it means: on Java a deferred site emits
`callee_info` and **no** `call` rows, and hybrid inlining finds a callee only when an allocation
reaches the receiver (`emit_callee_resolvents` keys on the *allocated* class). Where none does,
the site has no callees: an absence, not an over-approximation. Half of interface calls name a
type the app never declares. The Lua arm declines to do this and says why at
`codegen/mod.rs:611–631` (Prosody: 2,865 sinks / 806 paths → 2,145 / 263). Java is unmeasured,
not safer. The ladder shrinks the exposure sixteen-fold (27.0% → 1.63% of sites), and what is
left is closures and serializers, which is what hybrid inlining is for. Sites moved to rung 1 do
not gain soundness; they trade CHA's over-approximation for a model (§15.1).

**Optional, and flagged because it borders I-3:** an `index_engine` counter for deferred sites
that ended the fixpoint with zero resolved callees, printed beside the bucket line. It is a
count, not the listing I-3 defers, and it is what turns "1.63% of sites" into an actual hole on
an actual app. Last in §16; drop it if the team reads I-3 as covering it.

### 15.3 R13 vs `close`/`dispose`: the analysis disagrees with itself, and this spec picks a third option

The viability table row says "a model, and *not* an empty one". The prose under it says "or
simply left on CHA … the simulation below treats it that way". The simulation code does
neither: `sigstudy/policy.py:22` lists `resource` in `MODEL_BUCKETS`, so the quoted numbers
count those sites as **modelled** with the target set discarded. Two documents, three
treatments.

**Decision: hybrid inlining**, via `resolve: "inline"` (§11.3). It is the only one of the three
that keeps a sink inside a `close()` body reachable, which is the very reason the analysis moved
`close` off the skip list. Consequences:

- It is a deviation from the table row R13 points at. Deliberate; flagged here.
- It adds a disposition to the DSL (§5.2) and a sub-count to the summary line (§10.1) that the
  intent did not ask for. Both are small, and the sub-count is what keeps a mis-scoped `inline`
  entry from being silent.
- The simulated shares move slightly: the resource bucket is a median 0.15% of virtual sites
  (0.1% of excess), so median inlined goes from about 1.63% to about 1.8% and modelled falls by
  the same. Edge counts are unchanged, since a deferred site emits no `call` rows either way.
  §14.3's sweep is where the real numbers land.
- Under R14 a `close()` whose receiver never sees an allocation has no callees. That is the
  accepted gap, and for streams the allocation is usually local. A site with exactly one
  target stays on CHA (§4.2), so the entry never regresses a site that is exact today.

### 15.4 "One CLI flag" (I-9) vs a separate interface threshold (I-11)

Two intent bullets pull apart: `K` is one flag, and interface sites are separately configurable.
§9.1 resolves it with `--cha-threshold-interface` defaulting to `--cha-threshold`, so a user who
sets one flag gets one `K` and the second flag exists only for the population the intent says is
different. Two recorded fields, not one.

### 15.5 Four buckets (I-12) vs five outcomes

0.04–0.3% of virtual calls resolve to nothing. They satisfy `len() <= K`, take rung 2 and emit
no rows. Calling that CHA is defensible and keeps R11, but "830,833 CHA" reads as "sites with
edges" and 2,118 have none. Resolved as §10.1: four buckets, sub-count in parentheses. Five
buckets would be a better summary line and a deviation from I-12; flag, do not decide silently.
Likewise the jvm frontend's `Super` covers constructors and private calls while dex's does not
(`ctadl-ir/src/mir/call.rs:22–29`), so the `super:` row is not comparable across frontends;
pre-existing, repeat it in the summary line's help text.

### 15.6 Changing `mixed` and bumping the format touch every existing index and store

`mixed` is the default and nothing on disk records which strategy produced an index. A user who
upgrades and runs `ctadl query` on an old project gets old-policy results with no indication.
§9.2's `call_policy` fixes it going forward; for older indexes it is `None` and query says so
once at `info`. This is also why `legacy-mixed` exists rather than being deleted.

Format 8 (§7.3) stops every store under `/Volumes/Shampoo/ctadl-sweep/out/*/stores/` loading.
Rebuilding is bounded by the sweep's ~50 minutes at 3–6 way parallelism (that figure included
four reports per app), TikTok alone 95 s at an 18.3 GB peak. The 2.4 GB of signature tables
under `out/*/full/` stay valid: a CHA target count is a function of the static signature. Do the
re-import once, as §16 step 1, and keep the stores (`run-one.sh` and `manifest.tsv` do this).

### 15.7 The simulation does not establish that newpipe and schildi index

The policy's graph is 1.4–3.7x (median 2.2x) the `call` rows `mixed` emits today; plain CHA is
4–205x. That is in the range of a configuration that already runs on the smaller apps, which is
all the simulation can say. `mixed` itself failed on two of three F-Droid apps under 24 GiB.
Cutting hybrid inlining from 27% to 1.6% of sites is the largest lever available, but the engine's
own cost has to come down too, and nothing measured says by how much. **Do not plan as though this
change alone makes newpipe index.** Run §14.3's A/B at §16 step 5, not at the end.

### 15.8 Scope added beyond the intent

`legacy-mixed` (N2), the `call_policy` stamp (N5), §13.1, and the `report` flags in §12.1 are
not in `intent.md`. Each is justified above; none is large. `legacy-mixed` and the stamp are the
two to keep if anything is cut, because the A/B and the reproducibility warning depend on them.

---

## 16. Sequencing

Each step is independently testable and, except where noted, independently committable.

1. **IR change, format bump, corpus re-import.** `super_start`, both frontends,
   `IMPORT_FORMAT_VERSION = 8` with its history entry. Rung 0 is not wired; nothing resolves
   differently, so this is safe to land first and the re-import happens once (§15.6).
2. **SAM closure fix, moved into `ctadl-ir`** (§8.1). Changes only the report's numbers; verify
   against DuckDuckGo's `dagger.internal.Provider` (10,031 sites, 29.3M excess edges).
3. **Rung 0** (§7). TikTok's 49,774 super sites should go from 311 exact to nearly all exact.
4. **Bucket counting and the summary line** (§10), against today's `mixed`, for a baseline.
5. **The ladder with rung 1 stubbed** (`--no-dispatch-models`): `K`, rungs 2/3, `legacy-mixed`,
   `call_policy`. Run the index A/B here (§15.7); it needs nothing from the DSL.
6. **`find: dispatch`** (§5): loader, universe, evaluator, schema, `ProgramModelMatches::dispatch`,
   all three dispositions. No defaults yet; test against a fixture model file. The `inline`
   disposition needs nothing from step 7 and can ship its `close`/`dispose` default here.
7. **Synthetic-function codegen and the R4 guard** (§6, §15.1), including the warning-text change
   and the endpoint digest.
8. **Shipped defaults** (§11). First step that changes results for a user passing no flags; run
   TaintBench before and after.
9. **`ctadl report --models` and the policy section** (§12), and the `find: dispatch` docs.
10. **§13.1**, then the optional zero-callee counter (§15.2) and §13.2 only if §14.3 says so.
