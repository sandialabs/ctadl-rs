# ctadl logging design - DO-NOT-MERGE

Command output goes to stdout or files and is never affected by RUST_LOG. Progress, status,
diagnostics go through the "log" crate on stderr, except for the result of the final error of the
program, which stays as it is today, printed with anyhow. No log::error should ever be called: it
should propagate up through Results and, if not handled, be output with the anyhow path.

All logging init *in the shipped binary* should route through ctadl_ascent::init; it sets INFO level
on this project only, i.e. "warn,ctadl=info" by default. This level doesn't include all of the crates
implemented here, but that is because they don't need to print status at the default level.

Examples and test helpers are exempt and keep their own init (see Migration). The consequence is that
`examples/flowy.rs` runs with the old format and the old error-only default filter, so its output
looks different from `ctadl` proper. That is accepted.

Logging follows env_loggers handling of color.

# Migration

Look at all the existing `log::info` sites and, if the message is not reporting about a high level
phase, demote it to `log::debug`. Acceptance criteria: default ctadl go on a sample artifact
produces `O(artifacts and sub-imports)` amount of output.

All the current `eprintln!()` statements that are warnings should go to `log::warn!`. Change
existing `log::error` to `log::warn`. All the others can migrate to `log::info!(...)`.

`println!` sites are command output and stay on stdout untouched — with one exception. These four in
`codegen/flowy.rs` print check failures while incrementing `fail_count`, so they are failing
assertions, not output, and should become `log::warn!`:

- `codegen/flowy.rs:86` — required summary flow is absent
- `codegen/flowy.rs:96` — forbidden summary flow is present
- `codegen/flowy.rs:178` — required endpoint not found
- `codegen/flowy.rs:186` — forbidden endpoint is present

Every other `println!` (the `inspect` listing at `cli/mod.rs:959-1012`, the stats report at
`:930-948`, the record dump at `:1038`, the MIR/VMT dumps at `:876`, `:1108`, `:1112`) is a
command's requested output. Moving
any of it onto `log` would put it on stderr and under RUST_LOG, breaking `ctadl inspect | jq`.

Strip the literal Warning: prefix from main.rs:428, cli/mod.rs:410, cli/mod.rs:483 during migration,
or they render as Warning: Warning:...

Make `ctadl_ascent::init` idempotent.

Leave the sites that bypass it alone. They are deliberate: `test_utils.rs:30-35` forces `Debug` and
sets `is_test(true)` so the test harness captures output instead of interleaving it across parallel
tests, and `examples/flowy.rs:22` and `examples/tree-sitter-compile.rs:2` are examples, not the
shipped binary.

## New log format

We don't want timestamps and module paths in INFO or WARN output. The existing logging formatter
should be changed so that warn prints a heading "Warning: <message>" instead of the current format.
Apply the default color, i.e., the same style to the new heading, not the message body.

ERROR has no producers by design (see above), so the formatter needs no error case. If one ever
appears it is a bug: it should be a `log::warn!` or a propagated `Result`.

The heading is prepended once, to the front of the record. Embedded newlines in a message pass
through verbatim — do not prefix or re-indent each line. Two INFO messages depend on this:
`cli/mod.rs:579` and `:717` ("Wrote index graph to ...", "Wrote taint graph to ...") are multi-line
blocks with their own hanging indentation.

However, we want to keep the timestamps and module paths for DEBUG and TRACE.

## New status output

`import`:

- Print count of artifacts found, including the count of parts (e.g. in Lua, should include the count of individual .lua files found)
- Explain which sub-imports were parsed out and are being imported

`index`:
- Print loading of IR and preprocessing
- Print message when starting indexing

## Debugging docs

Update the debugging docs to explain that `RUST_LOG=warn,ctadl=debug` is the way to get more info,
and that `RUST_LOG=warn` is the way to get less (status off, warnings still shown). There is no `-q`
flag; RUST_LOG is the only knob, in both directions.
