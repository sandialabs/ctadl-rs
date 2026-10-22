# Known bugs

## index: flowy imports go through SSA twice

`ctadl_flowy::compile_program` (`frontends/ctadl-flowy/src/lib.rs`) already
calls `ssa::transform` on every function, and `ctadl_frontends::flowy::import`
stores that SSA program as the import. `ctadl index` then runs
`ssa::Pipeline::index_default()` on every import, with no check of the
language (`ctadl-ascent/src/cli/mod.rs`, the `run_pipeline_with_threads`
call). That pipeline has `ssa: true`, so flowy functions are transformed a
second time. `ssa::transform` lists "the function is not already in SSA form"
as a precondition it does not check: a second run versions the versions and
adds another exit block.

Tests don't catch it. The flowy test path (`codegen::flowy::check_with_config`)
runs no pipeline, and no CLI test indexes a flowy import.

**Likely fix:** pick one place that puts flowy into SSA. Either skip `ssa` in
the index pipeline for imports that are already in SSA form (which needs the
import to record that), or stop transforming in `compile_program` and let the
index pipeline do it the way it does for every other frontend.
