# CTADL

CTADL (Compositional Taint Analysis in Datalog) is a static taint analyzer. CTADL is implemented with the Ascent (https://s-arash.github.io/ascent/) Datalog engine embedded in Rust.

CTADL is currently under development.

## Usage

One-shot APK analysis:

```bash
ctadl go my-app /path/to/my/app.apk query.json
```

# Testing

Unit and integration tests run with Cargo:

```bash
cargo test
```

The regression tests, however, are only reliable when run through Nix, which
pins the full toolchain (compilers, Ghidra, etc.) the fixtures are built and
checked against:

```bash
nix build .#checks.${system}.regression
```

where `${system}` is your platform (e.g. `aarch64-darwin`, `x86_64-linux`).
Running the regression suite outside Nix is not reliable because results depend
on the exact compiler/disassembler versions Nix provides.

# History

CTADL is based on a prior [Souffle implementation](https://github.com/sandialabs/ctadl).

# Copyright

Copyright 2026 National Technology & Engineering Solutions of Sandia, LLC
(NTESS). Under the terms of Contract DE-NA0003525 with NTESS, the U.S.
Government retains certain rights in this software.
