# Head to head ctadl-souffle vs ctadl-rs experiment on firmware - DO-NOT-MERGE

ctadl is this project. ctadl-souffle, a previous version written with Souffle, is available at ../ctadl (with its own flake).

This branch has a harness for running ctadl firmware benchmarks. Read about it in ./firmware-eval/README.md.

Before the experiment runs, make sure:

- devShell that pins a common ghidra version
- Both engines can run on control binaries that are really simple with similar results

Run an experiment as follows:

- The goal is to compare ctadl-souffle with this ctadl on a subset of firmware. 50 firmware binaries. (This started at 5; the 5-binary run is kept in `firmware-eval/run/head2head/results-5binary/`.)
- Using existing sources and sinks, make sure they work (or produce an equivalent version that works) on ctadl-souffle.
- Make sure both engines use the same set of models (override the default models). You'll need library code models in addition to the sources and sinks
- Index each firmware with ctadl and ctadl-souffle
- Evaluate and compare SARIF paths found by running ctadl query for both versions
- Measure and compare for each:
  - import and index size
  - number of summaries
  - number of SARIF paths
- Put raw data for the results into an obvious place
- Graph the results with a stacked bar graph (old +- new)
- These graphs are going into a presentation, so size appropriately
  - At 50 binaries a bar per binary is a picket fence. Lead with corpus totals
    and the per-binary spread; keep the every-binary chart as the appendix.

Run the experiment.

When done, write up how to reproduce the results and the graph and summarize everything into EXPERIMENT-summary.md
