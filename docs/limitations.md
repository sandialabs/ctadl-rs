# Limitations

- **C# structs are not copied.** The CIL frontend treats a value type like an object, so a
  struct copy (an assignment or a by-value argument) aliases the original, and taint written
  into the copy shows up in the original as a false positive.
- **The address of a C struct member is not nameable.** The C frontend has no struct layout,
  so `&s.f` has no address in the IR and falls back to a copy of `s.f`: a callee that writes
  through that pointer (`init(&s.f)`) does not taint `s.f` in the caller, a false negative.
  `&s` and `&a[1]` are nameable and not affected.
- **A write through the address of a C global is lost.** A global `g` is `$globals.g` in every
  unit and is not moved behind `deref` when its address is taken, so `&g` is a copy of it and a
  callee's `p->f = v` through `init(&g)` is not observed at `g.f`, a false negative.
- **JVM and Dex drop an exception a callee does not catch.** An exception is the last result of
  every call, but the JVM and Dex front ends do not pass a callee's escaping exception on to the
  caller's handler, so taint carried by an exception thrown in a callee and caught in a caller is
  lost, a false negative. The CIL front end accumulates them (`excout`) and is not affected.
- **A Java method handle resolves only to the method its constant names.** `mh.invoke(..)` and
  `mh.invokeExact(..)` call through the handle's function reference, which only a method-handle
  constant (`ldc`, `const-method-handle`) supplies. A handle built at run time
  (`lookup.findVirtual(..)`), adapted (`bindTo`, `asType`) or invoked with
  `invokeWithArguments` calls nothing, and a virtual handle reaches the method it names, not an
  override. Arguments are passed by position, so a call that relies on `invoke`'s boxing or
  varargs adaptation lines up wrongly. These are false negatives.
