# Limitations

- **C# structs are not copied.** The CIL frontend treats a value type like an object, so a
  struct copy (an assignment or a by-value argument) aliases the original, and taint written
  into the copy shows up in the original as a false positive.
