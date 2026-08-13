# Head to head ctadl-souffle vs ctadl-rs experiment on firmware - DO-NOT-MERGE

ctadl is this project. ctadl-souffle, a previous version written with Souffle, is available at ../ctadl (with its own flake).

This branch has a harness for running ctadl firmware benchmarks. Read about it in ./firmware-eval/README.md.

Run an experiment as follows:

- The goal is to compare ctadl-souffle with this ctadl on a subset of firmware.  Let's try 5 firmware binaries for now.
- Using existing sources and sinks, make sure they work (or produce an equivalent version that works) on ctadl-souffle.
- Index each firmware with ctadl and ctadl-souffle
- Evaluate and compare SARIF paths found by running ctadl query for both versions
- Measure and compare for each:
  - import and index size
  - number of summaries
  - number of SARIF paths
- Put raw data for the results into an obvious place
- Graph the results with a stacked bar graph (old +- new)

Run the experiment.

When done, write up how to reproduce the results and the graph and summarize everything into EXPERIMENT-summary.md
