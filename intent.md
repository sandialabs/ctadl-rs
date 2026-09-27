DO-NOT-MERGE

The workflow I want to support is:

- Import an apk and the native code inside it. Say there are two native libs inside, X.so and Y.so.
- Index X.so.
- Index just the dex code and co-analyze just the summaries of X.so (not Y.so).

Changes:

- add an `--no-native-libs` flag to IndexArgs.
- for each `--summary` project, call jni_observer.observe() on each of its imports' symbol tables,
  and observe_registry() on each of its imports. 
  - The symbol table has its own file (ir-vmt.bitcode), so this doesn't
    load or process X's code.
- Problem: X's summaries are discarded. load_and_map_summaries keeps a summary only if its function
  is already in the current project (cli/mod.rs:1032-1041). In a Dex-only project, X's Java_…
  functions never are.
     - Fix: in link, add the native target to the project's function table if it isn't there yet
       (get_or_add_function instead of get_function_id for native_id), then keep summary loading
       after linking.
     - Make sure to pull out accurate native parameter information from the Dex methods (where it's
       reliable) and compare it against what Ghidra produced (which is not reliable in general).
       This we produce a warning when Ghidra wasn't able to recover the right parameter count
