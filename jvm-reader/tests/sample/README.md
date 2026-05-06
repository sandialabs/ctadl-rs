# Sample: HelloWorld

Minimal Java sample for instruction-flow tests.

- **HelloWorld.java** – One method with simple dataflow (locals, constants, arithmetic), another with a couple of call kinds (instance `length()`, `System.out.println`).

## Rebuild the JAR

From the repo root:

```bash
javac -d tests/sample/out tests/sample/HelloWorld.java
jar cf tests/jar/HelloWorld.jar -C tests/sample/out .
```

On Windows (PowerShell):

```powershell
javac -d tests\sample\out tests\sample\HelloWorld.java
jar cf tests\jar\HelloWorld.jar -C tests\sample\out .
```

The JAR is written to `tests/jar/HelloWorld.jar` so existing tests (`test_jar_all_classes_parsed`, `test_instruction_flow_iter`) pick it up.
