The Apktool failure is a CTADL JVM frontend defect, not malformed Apktool bytecode. I isolated three separate decoder bugs.

  ## 1. Ordinary Java 8 classes: switch operands are not consumed

  CTADL’s stack simulator checks that every predecessor enters a basic block with the same operand-stack height. The error is raised in ctadl-rs-cve-test-data/.cache/ctadl-rs/jvm-reader/src/flow.rs:520.

  The reported terms mean:

  - pc: JVM bytecode offset, not source line.
  - block: CTADL’s internal basic-block number.
  - existing_len: stack height recorded from one predecessor.
  - new_len: height calculated from a later predecessor.

  The simulator accounts for conditional branches, but ctadl-rs-cve-test-data/.cache/ctadl-rs/jvm-reader/src/flow.rs:1764 omits:

  0xaa | 0xab => (1, 0), // tableswitch, lookupswitch

  Both instructions consume one integer selector.

  ### BinaryResourceParser.parseTable

  The Java switch is at ctadl-rs-cve-test-data/.work/sources/CVE-2026-39973/brut.apktool/apktool-lib/src/main/java/brut/androlib/res/decoder/BinaryResourceParser.java:184.

  Its relevant bytecode is:

  29: aload_0                         // loop header, correct stack = 0
  ...
  38: invokevirtual chunkType:()I     // stack = 1
  41: lookupswitch                    // should consume selector: stack = 0
  ...
  84: aload_0                         // default case
  85: aload_1
  86: invokespecial skipUnexpectedChunk:(...)V
  89: goto 29

  Because CTADL gives lookupswitch a zero stack effect, the selector remains as a phantom slot. The default case therefore returns to PC 29 with one slot instead of zero:

  block 1 (pc 29) <- block 5 (pc 84), existing_len=0, new_len=1

  This exactly explains the 0 → 1 discrepancy.

  ### ResFileDecoder.decode

  The source switch is at ctadl-rs-cve-test-data/.work/sources/CVE-2026-39973/brut.apktool/apktool-lib/src/main/java/brut/androlib/res/decoder/ResFileDecoder.java:63.

  A Java string switch compiles into two JVM switches:

  97:  invokevirtual String.hashCode:()I
  100: lookupswitch
  ...
  181: iload 9
  183: tableswitch
  ...
  221: new StringBuilder

  CTADL fails to consume both selectors:

  1. lookupswitch leaves one phantom slot.
  2. tableswitch leaves a second phantom slot.

  There are also direct branches from PCs 70 and 85 to PC 221 with a correctly empty stack. The join therefore becomes:

  block 17 (pc 221) <- block 14 (pc 181), existing_len=0, new_len=2

  The class’s StackMapTable says these join points have empty stacks, and javap accepts the class. It is ordinary major-version-52 Java 8 bytecode.

  ## 2. R8 BinaryResourceParser: iushr has the wrong instruction length

  R8 rewrites BinaryResourceParser enough that CTADL encounters another decoder bug first:

  block 8 (pc 102) <- block 7 (pc 42), existing_len=0, new_len=3

  This is in R8’s rewritten unpackLanguageOrRegion(byte[], char) method. It contains iushr at PCs 71 and 94.

  CTADL incorrectly says opcode 0x7c (iushr) has two operand bytes in ctadl-rs-cve-test-data/.cache/ctadl-rs/jvm-reader/src/flow.rs:976:

  0x7c => 2,

  iushr is a one-byte instruction with no inline operands. Consequently:

  - At PC 71, CTADL skips the real instructions at PCs 72 and 73.
  - At PC 94, it skips PCs 95 and 96.
  - Those skipped instructions would consume three stack slots in total.
  - CTADL reaches PC 102 with three phantom slots.

  That exactly explains existing_len=0, new_len=3.

  The adjacent shift stack-effect grouping should also be corrected by opcode rather than ranges:

  ishl   0x78: 2 -> 1
  lshl   0x79: 3 -> 2
  ishr   0x7a: 2 -> 1
  lshr   0x7b: 3 -> 2
  iushr  0x7c: 2 -> 1
  lushr  0x7d: 3 -> 2

  ## 3. R8 ResFileDecoder: switch phantoms meet an exception handler

  The R8 class still contains the string-switch lowering:

  110: lookupswitch
  203: tableswitch

  The two unconsumed selectors survive along the normal path. At PC 468 that path therefore arrives with two phantom slots.

  An exception handler beginning at PC 417 correctly receives one exception object and immediately executes pop, leaving zero slots. When that handler joins PC 468, CTADL reports:

  block 31 (pc 468) <- block 29 (pc 417), existing_len=2, new_len=0

  Thus R8 changes where the inconsistency becomes visible, but the two-slot difference still comes from the missing switch effects.

  The R8 product itself passes JVM verification:

  $ java -Xverify:all -jar apktool-v3.0.1-SNAPSHOT.jar --version
  v3.0.1-SNAPSHOT

  ## 4. Whole-JAR InvalidUtf8 is unrelated

  The full shaded and R8 JARs fail earlier while CTADL parses every class:

  reading jar: ...
  jvm decoding error
  InvalidUtf8

  The first offending class in both products is:

  com/android/tools/smali/smali/smaliFlexLexer.class

  Its constant is:

  - ordinary JAR: constant-pool entry #381
  - R8 JAR: constant-pool entry #338

  It is a 2,688-code-unit serialized lexer table containing 25 unpaired UTF-16 surrogate code units. The ordinary shaded JAR later contains another example in:

  com/google/common/base/CharMatcher$Invisible.class, cp #6

  CTADL’s modified-UTF-8 decoder collects decoded values and calls char::from_u32 on each one in ctadl-rs-cve-test-data/.cache/ctadl-rs/jvm-reader/src/parse_utils.rs:69. Rust Unicode scalar values cannot
  represent surrogate code units, so it returns InvalidUtf8.

  This data is valid enough for the JVM and is intentionally used as arbitrary UTF-16 table data. A robust fix needs to retain Java UTF-16 code units or use a WTF-8-like representation. Merely combining valid
  surrogate pairs will not handle these deliberately unpaired values.

  The error lacks the JAR entry name because ctadl-rs-cve-test-data/.cache/ctadl-rs/jvm-reader/src/jar.rs:28 parses each class without attaching entry.name() to the returned error.

  ## Reproduction

  Using the retained vulnerable checkout:

  ROOT=/Users/auriga/Data/ctadl-rs-cve-test-data
  APK="$ROOT/.work/sources/CVE-2026-39973/brut.apktool"
  CTADL="$ROOT/.cache/ctadl-rs/target/release/ctadl"

  cd "$APK"
  git checkout --detach c15091f5115f1aa1225b21748c593003b827da8a

  ./gradlew :brut.apktool:apktool-cli:shadowJar \
    --no-daemon --console=plain

  ./gradlew :brut.apktool:apktool-cli:proguard \
    --no-daemon --console=plain

  Isolate the ordinary classes:

  PARSER="$APK/apktool-lib/build/classes/java/main/brut/androlib/res/decoder/BinaryResourceParser.class"
  DECODER="$APK/apktool-lib/build/classes/java/main/brut/androlib/res/decoder/ResFileDecoder.class"

  "$CTADL" --store "$(mktemp -d)" \
    import -l jvm -n apktool-parser "$PARSER"

  "$CTADL" --store "$(mktemp -d)" \
    import -l jvm -n apktool-decoder "$DECODER"

  Reproduce the unrelated whole-JAR error:

  CLI="$APK/apktool-cli/build/libs/apktool-cli.jar"
  R8="$APK/apktool-cli/build/libs/apktool-v3.0.1-SNAPSHOT.jar"

  "$CTADL" --store "$(mktemp -d)" import -l jar -n apktool-cli "$CLI"
  "$CTADL" --store "$(mktemp -d)" import -l jar -n apktool-r8 "$R8"

  Inspect the exact ordinary bytecode:

  javap -c -p "$PARSER"
  javap -c -p "$DECODER"

  javap -v -p "$PARSER"   # includes StackMapTable
  javap -v -p "$DECODER"

  Inspect R8 without extracting it:

  javap -c -p -classpath "$R8" \
    brut.androlib.res.decoder.BinaryResourceParser

  javap -c -p -classpath "$R8" \
    brut.androlib.res.decoder.ResFileDecoder

  ## Minimum CTADL regression suite

  I would add these fixtures before reconsidering Apktool as a gold case:

  - Sparse integer switch producing lookupswitch.
  - Dense integer switch producing tableswitch.
  - Java 8 string switch producing both instructions.
  - iushr followed by meaningful one-byte instructions.
  - Modified-UTF-8 constants containing paired and unpaired surrogates.
  - The ordinary and R8 versions of these two Apktool classes.

  Errors should also include JAR entry, class, method descriptor, PC, opcode, and predecessor edge. There is an additional mismatch in ctadl-rs-cve-test-data/.cache/ctadl-rs/xtask/src/jvm.rs:457: it tries to skip
  the old InvalidClassFile variant, while the detailed join error now uses InvalidClassFileMessage, so that special handling no longer matches.

  The retained case summary is at ctadl-rs-cve-test-data/not_working/pre-opus/CVE-2026-39973/README.md. No files were changed during this investigation.
