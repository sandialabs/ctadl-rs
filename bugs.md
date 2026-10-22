# Known bugs

## jvm: a receiver-less `invokedynamic` gets a class name in a different form from every other call

`frontends/ctadl-jvm/src/lib.rs`, `decode_call`, the `receiver: None` /
`CallKind::Dynamic` arm (for example the `makeConcatWithConstants` calls that
string concatenation compiles to).

For every other kind of call, the class name that `decode_call` passes to
`declare_extern` is the descriptor form `L<class>;`, the same prefix that
starts the method signature:

| arm                                | `JavaClass`     | `JavaMethod` (signature)       |
|------------------------------------|-----------------|--------------------------------|
| no receiver, Interface/Special/Virtual | `L<class>;` | `L<class>;-><name><descr>`     |
| no receiver, Static                | `L<class>;`     | `L<class>;-><name><descr>`     |
| receiver, any kind                 | `L<class>;`     | `L<class>;-><name><descr>`     |
| **no receiver, Dynamic**           | **`?unknown`**  | `L?unknown;-><name><descr>`    |

In the receiver-less dynamic arm, `class_name` is the bare `"?unknown"`. The
signature is built as `"L" + class_name + ";->" + ...`, so it reads
`L?unknown;->...`, but the extern's `JavaClass` is recorded as `?unknown`,
without the `L…;`. Anything that joins an extern's class with its signature's
class prefix, or matches externs by class (default models, CHA, `class`
constraints in model generators), treats these calls as belonging to a class
`?unknown` that no signature names.

**Likely fix:** make `class_name` `"L?unknown;"` and build the signature from
it the way the other arms do (`class_name + "->" + method_name + descr`).
Nothing else in the repo mentions `?unknown` (`git grep`), so no model or
golden in the tree depends on the bare form.

Found during the comment sweep on branch `code-comment-cleanup-everywhere`. It
was left unfixed because fixing it changes behavior.
