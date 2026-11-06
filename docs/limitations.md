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
