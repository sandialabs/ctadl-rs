  Merged to main (Sep 28)
  - #109, Android inter-component communication (ICC): taint now links Android intents across app components. The DroidBench ICC test APKs are fetched      
    through Nix instead of being committed, and the expensive tests moved to xtask.
  - #136, JNI native code as summaries: you can now index an app's native libraries against --summary projects. It picks the JNI native slot layout per     
    method and checks it against the prototype. It also adds --no-native-libs, stores each import's hash in the index config, warns when a summary project  
    may not match the app's libraries, adds a 32-bit x86 regression fixture, and documents the workflow.
  - #137 / #138, dependency cleanup: removed the vulnerable thrift dependency and the stale per-crate Cargo.lock files.

  Ongoing work on improve-native-handling, native-handling-for-main and r8-merged-lambdas
  - Native/Android pipeline:
    - JNI prototypes and dropped return values are recovered before pcode export.
    - An app bundle's Dex natives are passed to its native-only splits.
    - Re-imports are skipped by default (--force to re-import).
  - New android-native regression family: uses a real F-Droid app (jwtc-android-chess). You later narrowed its source/sink models.
  - R8 class merging: classes that R8 merged are split back apart at Dex import (new ctadl-dex/src/merged.rs), with a ClassIdMergedFlow test.
  - SSA work in ctadl-ir:
    - Pruned SSA.
    - A mem2reg pass that promotes stack slots to locals before SSA (window 8 by default).
    - SSA can now write parameters' entry versions back. This is turned on for JVM bytecode, while C imports keep the exit-version write-back because of how
      the C front end lowers pointers.
  - Index engine performance:
    - The call-target transitive rule is now keyed on the tag's path prefix.
    - Every relation and index logs its size (through a vendored, patched ascent_macro).
    - An experimental dead-phi pass and census example programs (SSA, stack, locals, escape).
    - A cleanup pass on index_engine/mod.rs today with no change in behavior.
  - Dev tooling: the Nix dev shell now has Android app debugging tools (documented in docs/debugging.md), and each reader is built from its own slice of the
    source tree.
  - Notes: you added a lot to things-to-improve.md, including the greenbits root cause and fix and the decisions that still fail the type check. You also   
    started recipes.md.
