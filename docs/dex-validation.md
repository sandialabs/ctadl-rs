# Validating `dex-reader`'s instruction semantics - DO-NOT-MERGE

Status: §1 and §2 are built (see "What was built" at the end). The frontend
check in §2 and §3 are not.

`dex-reader` hand-writes per-opcode facts that the rest of the pipeline trusts:
`Instruction::data_flow()` (registers read and written), `can_throw()`,
`call_args()`, `is_invoke()`, the operand decoders, and the index type of each
operand. [`dex-jvm-reader-improvements.md`](dex-jvm-reader-improvements.md) §1
records bugs in every one of these. All of them were found by reading code. The
unit tests added since then pin the table's current answers, but they were
checked by hand against the Dalvik spec. They do not derive the answers from
any independent source. Dex has no machine-readable pop/push table to check
against (§1.1 problem 7).

This note proposes two independent checks, cheapest first.

| | dexlib2 cross-check | Per-opcode runtime harness |
| --- | --- | --- |
| Oracle | dexlib2's opcode table and decoder (shipped inside baksmali) | ART itself, running each opcode on an emulator |
| Checks | which opcodes write a register, wide writes, can-throw, sets-result, operand index kinds, decoded operands | read sets, write sets, can-throw, value dependence |
| Device needed | no | yes (emulator) |
| Effort | a few hours | about a day |
| Runs in `cargo xtask regression` | easily | yes, but slow (emulator boot) |

---

## 1. dexlib2 cross-check

### What dexlib2 provides

`baksmali` in the Nix shell is a fat jar
(`baksmali-3.0.9-fat.jar`) that bundles dexlib2. Its
`com.android.tools.smali.dexlib2.Opcode` enum covers every opcode and carries:

| dexlib2 | Compare against | Example bug it catches |
| --- | --- | --- |
| `setsRegister()` | `!data_flow().dest.is_empty()` | `iput-wide` listing its object register as a dest (§1.2) |
| `setsWideRegister()` | `dest.len() == 2` | a wide result reported as one register, or the reverse |
| `canThrow()` | `can_throw()` | opcodes `0xfa`-`0xff` reported as unable to throw (§1.6) |
| `setsResult()` | `is_invoke()` / `call_args()` (plus `filled-new-array`) | `invoke-polymorphic` missing from `is_invoke()` (§1.6) |
| `canContinue()` | `is_control_flow()` and block splitting | a terminator treated as falling through |
| `referenceType`, `referenceType2` | the index type in each `Format*<T>` | const-method-handle's operand typed `MethodIdx` (§1.6) |
| `format` | the `Format*` each opcode decodes with | an opcode decoded with the wrong format or size |
| `name` | `DALVIK_OPCODES` | `unused-fa` and friends (§1.6) |

`Opcodes.forApi(api).getOpcodeByValue(byte)` maps a byte to an `Opcode`, which
lines it up with `Instruction::new`.

### Differential decoding

dexlib2 also decodes instructions: `DexBackedDexFile` gives, for each
instruction, its operands (`getRegisterC()` ... `getRegisterG()`,
`getRegisterCount()`, `getStartRegister()`, and the reference index). Decoding
the same `.dex` with both readers and comparing operands instruction by
instruction checks the decoders themselves. It would have caught the swapped
`A|G` nibbles in `Format45cc` (§1.6), but only if the input contains those
opcodes. The regression samples (`javac --release 8` → `dx`) contain none, so
this needs an input that does. The simplest is a hand-written `.smali` file
assembled with `smali assemble --api 28` (the assembler is in the Nix shell).

### How to build it

1. Dump dexlib2's table to JSON, one row per opcode: value, name, format,
   flags, reference types. A ~50-line Java program (or `jshell`) run against the
   baksmali jar does it. Check the dump into `xtask/tests/dex/` so the Rust
   side does not need a JVM at test time, and regenerate it when the baksmali
   version changes.
2. Add a Rust test in `dex-reader` (or an xtask check) that, for each of the
   256 opcodes, builds a representative `Instruction` and compares it with the
   row: dest non-empty iff `setsRegister`, `dest.len() == 2` iff
   `setsWideRegister`, `can_throw()` iff `canThrow`, and so on.
3. Optionally, add an xtask case for differential decoding over every
   `.dex` that the regression suite already compiles, plus the APK fixture.

### Limits

- **Says nothing about sources.** dexlib2's flags describe writes only. The read
  set (`data_flow().source`) is unchecked, so this would not have caught the
  spurious `vC+1` in long shifts (§1.5) or anything else about which registers
  are read.
- **Says nothing about the frontend.** The self-alias store bug (§1.3) and
  the missing calls for `invoke-polymorphic`/`invoke-custom` (§1.6) were in
  ctadl-dex, not the table.
- **It is a second hand-written table.** dexlib2 is widely used and well
  tested, but it is still a table, not the runtime. Where the two disagree,
  the Dalvik spec or the runtime decides.

---

## 2. Per-opcode runtime harness

### Idea

Run each opcode on a real ART and observe what it reads and writes. A run
cannot show "flow" directly. It shows **dependence**: change one input, run
again, and see whether the output changes. That is enough to check
`data_flow()` in both directions.

### Tooling already present

The Nix shell provides `smali` (the assembler, from the same 3.0.9 release as
`baksmali`), `adb`, `emulator`, `avdmanager`, and system images for API 30 and
API 34. Every Android image ships `dalvikvm`, which runs a bare
`.dex` without packaging an APK:

```sh
adb push t.dex /data/local/tmp/
adb shell dalvikvm64 -Xint -cp /data/local/tmp/t.dex Main
```

`-Xint` forces the interpreter, so each instruction runs as written instead of
being JIT-compiled into something else.

### Shape of a case

For each opcode (in practice, each opcode × format family, about 220 cases),
generate a smali method that:

1. Loads distinct, known values into every register, with types the verifier
   will accept for this instruction.
2. Executes the one instruction under test.
3. Prints every register, and for stores and loads, the field or array element
   it touched.

Then run it several times:

| Run | `data_flow()` is wrong if… | Example bug it catches |
| --- | --- | --- |
| Baseline | a register not in `dest` changed, or a `dest` register never changes | `iput-wide` listing its object as a dest (§1.2) |
| Change one register **not** in `source` | the result changes: a missing source | — |
| Change one register **in** `source` | the result never changes, over several random values: a spurious source | the long shift's `vC+1` (§1.5) |
| Run inside `try`/`catch`, with inputs that should fault | it throws when `can_throw()` says it cannot | `0xfa`-`0xff` reported as unable to throw (§1.6) |

ART's verifier is a second oracle for free. A method that reads a register as
the wrong type (for example, a pair where an int is expected) is rejected before
it runs. So a generator that follows `data_flow()`'s idea of operand widths
fails loudly when that idea is wrong.

The same runs can drive the frontend. Feed each case's `.dex` through
`ctadl-dex` and check that every observed dependence appears as an edge in the
generated IR. That covers bugs like the self-alias store (§1.3), which
`data_flow()` alone cannot show.

### Limits

- **Value-specific dependence.** Some dependences vanish for particular values:
  `and-int` with 0, a shift by 0, a multiply by 0. Each perturbation needs
  several random values, and a "spurious source" result still needs a human to
  confirm it.
- **One instruction at a time.** This checks the per-opcode table, not whole
  methods or control flow.
- **Heap and calls need extra plumbing.** Stores and loads need the touched
  field or element read back. Invokes need a callee that records its arguments.
  `invoke-custom` needs a call-site item and a bootstrap method in the `.dex`.
- **API levels.** `invoke-polymorphic` and `invoke-custom` need API 26+, and
  `const-method-handle`/`const-method-type` need API 28+. Both images qualify.
- **Cost.** Booting a headless emulator takes minutes, so this belongs in an
  opt-in xtask case, not in `cargo test`. Per CLAUDE.md, every run should
  capture all output to a file.

### How to build it

1. Write a generator (an xtask module) that emits one smali file per case, with
   the register setup, the instruction, and the print-out, plus the perturbed
   variants.
2. Assemble with `smali` and convert or merge with `d8` as needed.
3. Run each variant with `dalvikvm64 -Xint` on a headless emulator, capturing
   output under a run directory.
4. Compare the runs with `data_flow()`, `can_throw()` and the frontend's IR, and
   report mismatches per opcode.

---

## 3. Not proposed here: single-stepping whole methods

Running real methods under `dalvikvm -Xint` with the debugger enabled, stepping
one instruction at a time, and snapshotting every register with JDWP
`StackFrame.GetValues` would check whole methods, not just single opcodes.
JDI (the standard Java debugger API) exposes only named locals, not raw
register slots, so this needs a small raw-JDWP client that tracks each slot's
type tag. It costs more and is less precise than the harness, so it is worth
considering only after §2 exists.

## Recommendation

Build §1 first. It costs a few hours, needs no device, and would have caught
§1.2 and most of §1.6. Then build §2, which is the only check here that
validates read sets and frontend edges. It would have caught §1.3, §1.5 and the
rest of §1.6.

---

## What was built

### §1: dexlib2 cross-check

| Check | Where | Runs in |
| --- | --- | --- |
| Opcode table: name, format, size, reference kinds, written registers, can-throw, sets-result, can-continue | `readers/dex-reader/tests/dexlib2_opcodes.rs`, against `tests/data/dexlib2-opcodes.tsv` | `cargo test` |
| Differential decoding, instruction by instruction | `dex:dexlib2-decode` (`xtask/src/dexlib2.rs`) | `cargo xtask regression` |
| Disassembly text of every opcode | `dex:baksmali-all-opcodes` | `cargo xtask regression` |

The dump lives in dex-reader rather than `xtask/tests/dex/`, because the test
that reads it is in dex-reader. `scripts/DumpDexlib2Opcodes.java` regenerates
it. Differential decoding runs over the compiled samples, the APK fixture, and
`xtask/tests/dex/all-opcodes.smali`, a new fixture that holds every opcode with
edge-case operands. The check fails if the fixture stops covering an opcode.

Results:

- The opcode table agrees with dexlib2 on every fact but one. dexlib2 says
  `fill-array-data` cannot throw, but it throws on a null array or a short
  one. ART's own table and the runtime harness agree with dex-reader, so the
  test lists this as a known divergence.
- Decoding agrees on every instruction (about 1.5M lines across the fixture,
  samples and APK). Reverting the 45cc fix (`809f5321`) fails the check.
- The disassembly comparison over the fixture found four display bugs, all
  hidden from `dex:baksmali` by its normalizers or absent from the samples:
  `const-wide/high16` printed negative values wrong (`d936c882`); array-data
  elements were unsigned and unsuffixed (`ba6f5224`); proto operands and
  method-handle operands printed as raw indices (`78d910de`, `2b6edcba`).
  The normalizers that hid the first two are gone. Call sites are still shown
  only by index, `call<N>`, because dex-reader has no encoded-array decoder.

### §2: per-opcode runtime harness

`cargo xtask dex-opcodes [--avd <name>] [--out <dir>] [--filter <name>]`
(`xtask/src/dex_opcodes/`). It is opt-in, not part of `regression`. It needs a
device or an emulator. To make one:

```sh
export ANDROID_AVD_HOME=/Volumes/Shampoo/android-avd   # or anywhere
avdmanager create avd -n dexop34 -k "system-images;android-34;google_apis;arm64-v8a"
cargo xtask dex-opcodes --avd dexop34
```

It differs from the sketch above in two ways. First, it perturbs inputs
inside one process, not by rerunning `dalvikvm` per variant: the Java driver
calls each case once per input value, so all 250 cases take about ten seconds.
Second, each case is its own class, so a verifier rejection costs only that
case. Everything a run produces (smali, manifest, device log, results, report)
lands in `--out`.

Results on an API 34 arm64 emulator: all 250 cases pass, so ART contradicts no
`data_flow()` or `can_throw()` answer. The review items that remain are listed
with their reasons in `EXPECTED_REVIEWS`: resolution-only throws, monitors
(no visible value), `check-cast` (writes the same reference), and
`long-to-int`'s high word (truncated, but the verifier requires the pair).
Reintroducing old bugs shows the harness catches them:

| Reintroduced bug | Harness verdict |
| --- | --- |
| `iput-wide` lists its object as a dest (§1.2 of the improvements doc) | FAIL |
| long shifts read `vC+1` (§1.5) | REVIEW: source v8 never changed the result |
| invoke-polymorphic reads no registers (§1.6) | FAIL |
| invoke-custom cannot throw (§1.6) | FAIL |

Not built: checking the ctadl-dex IR against the observed dependences, which
would catch frontend bugs like the self-alias store (§1.3). It needs a
reachability check over each case's IR, including fields, array elements and
calls, and is the next step if one is wanted.

