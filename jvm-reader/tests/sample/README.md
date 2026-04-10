# Sample: HelloWorld

Minimal Java sample for instruction-flow tests.

- **HelloWorld.java** – Small baseline sample.
- **ControlFlowMaze.java** – Branch joins, loop/switch control flow, and try/catch/finally.
- **InvokeShapes.java** – Interface/default/virtual/static calls and long/double slot behavior.

## Rebuild the JAR

From the repo root:

```bash
javac -d tests/sample/out tests/sample/HelloWorld.java
jar cf tests/jar/HelloWorld.jar -C tests/sample/out .
```

Build all samples (`*.java`) at once with the helper script:

```bash
bash tests/sample/build_samples.sh
```

This creates/updates matching files in both:

- `tests/jar/<Sample>.jar`
- `tests/class/<Sample>.class`

On Windows (PowerShell):

```powershell
javac -d tests\sample\out tests\sample\HelloWorld.java
jar cf tests\jar\HelloWorld.jar -C tests\sample\out .
```

The JAR is written to `tests/jar/HelloWorld.jar` so existing tests (`test_jar_all_classes_parsed`, `test_instruction_flow_iter`) pick it up.
