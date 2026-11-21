# Intent: Run the serial index engine on small inputs

## Problem

`-j N` with N > 1 (or `-j 0`, every core) always runs the parallel index engine. On a small program the parallel engine is slower than the serial one, because thread scheduling and the concurrent data structures cost more than they save. In one comparison a small browser took 0.077 s serially and 0.129 s on four workers, while a large application took 117.7 s serially and 45.2 s on four workers. Earlier notes put the crossover between about 45,000 assignments (serial faster and smaller) and 160,000 (parallel useful).

## Change

When the caller asks for several index workers, run the serial engine anyway if the input has fewer than a cutoff number of assignment facts, and log that choice. The starting cutoff is 50,000 assignment facts. Re-measure it on the current engine before fixing the number. `-j 1` stays serial, and the parallel engine runs unchanged above the cutoff.

## Constraints

- The index results must be the same whichever engine runs. Use `index-diff` as a gate that must pass.
- The cutoff selects an engine. It does not change how many threads import or decoding use.
