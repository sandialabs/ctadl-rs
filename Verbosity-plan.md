# ctadl logging design - DO-NOT-MERGE

Command output goes to stdout or files and is never affected by RUST_LOG. Progress, status,
diagnostics go through the "log" crate on stderr, except for the result of the final error of the
program, which stays as it is today, printed with anyhow. No log::error should ever be called: it
should propagate up through Results and, if not handled, be output with the anyhow path. All logging
init should route through ctadl_ascent::init; it sets INFO level on this project only, i.e.
"warn,ctadl=info" by default. This level doesn't include all of the crates implemented here, but
that is because they don't need to print status at the default level.

Logging follows env_loggers handling of color.

# Migration

Look at all the existing `log::info` sites and, if the message is not reporting about a high level
phase, demote it to `log::debug`. Acceptance criteria: default ctadl go on a sample artifact
produces `O(artifacts and sub-imports)` amount of output.

All the current `eprintln!()` statements that are warnings should go to `log::warn!`. Change
existing `log::error` to `log::warn`. All the others can migrate to `log::info!(...)`.

Strip the literal Warning: prefix from main.rs:428, cli/mod.rs:410, cli/mod.rs:483 during migration,
or they render as warning: Warning:...

Make `ctadl_ascent::init` idempotent.
Ignore the sites that bypass it.

## New log format

We don't want timestamps and module paths in INFO, WARN, or ERROR output. The existing logging
formatter should be changed so that warn and error print a heading "warning: <message>" and "error:
<message>" instead of the current format. Apply the default color, i.e., the same style to the new
heading, not the message body

However, we want to keep the timestamps and module paths for DEBUG and TRACE.

## New status output

`import`:

- Print count of artifacts found, including the count of parts (e.g. in Lua, should include the count of individual .lua files found)
- Explain which sub-imports were parsed out and are being imported

`index`:
- Print loading of IR and preprocessing
- Print message when starting indexing

## Debugging docs

Update the debugging docs to explain that `RUST_LOG=warn,ctadl=debug` is the way to get more info.
