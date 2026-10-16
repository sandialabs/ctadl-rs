# How big an app can this machine actually analyze - DO-NOT-MERGE

TaintBench's apps are small - 1.9 to 4.4 MB of Dex, and at most 1 MB of native
code - so nothing in `taintbench-callgraph-rework.md` says where ctadl stops
being usable. This measures that directly, on real apps from a public repo,
under a hard memory cap.

The machine is an M1 Ultra, 20 cores, 128 GiB. Every run is wrapped in a
**116 GiB guard** that polls the process tree's physical footprint and kills the
job if it crosses the cap, plus a wall-clock timeout (90 min for import, 2 h for
index). The binary is the branch on `origin/blowup2` (`28f949ea`), the fastest
of the five points measured in the other document.

## Short answer

| | practical ceiling on this machine |
| --- | --- |
| **Dex (Java)** | roughly **25-35 MB** of `classes*.dex`. 27 MB finishes in under 3 minutes at 17 GB; 35 MB ran 35 minutes and was killed at 117 GiB. |
| **Native (per library)** | well under **38 MB** of `.so`. A single 37.7 MB library did not finish *import* in 90 minutes, at 46 GB - it never reached `index`. |

Memory is the binding constraint on the Dex side; **time** is the binding
constraint on the native side. Neither ceiling is a clean function of size -
which side of the line an app falls on is structural, the same lesson `xbot` and
`vibleaker` taught at 1/10th the scale.

## What was run

Apps come from F-Droid (all FOSS, freely redistributable, and its index carries
per-APK sizes so candidates can be ranked without downloading them). The zip
central directory of each candidate was read over HTTP range requests to
separate real code from assets - APK size is a poor guide, since much of a
300 MB APK is usually pictures.

| app | APK | dex | arm64 native | mode | import | index | peak | outcome |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | --- |
| `hashengineering.darkcoin.wallet` | 183 MB | 27.1 MB | 39.8 MB (5 libs) | Dex only | 7 s | **165 s** | **17.0 GB** | finished |
| `com.greenaddress.greenbits_android_wallet` | 246 MB | 34.9 MB | 145.4 MB | Dex only | 9 s | 2109 s | **117 GiB** | **killed at the cap** |
| `hashengineering.darkcoin.wallet` | 183 MB | 27.1 MB | 39.8 MB (5 libs) | Dex + native | **>90 min** | not reached | 45.7 GiB | **timed out in import** |

For scale, the TaintBench apps these sit above:

| app | dex | native | index | peak |
| --- | ---: | ---: | ---: | ---: |
| `xbot_android_samp` | 1.9 MB | none | 84 s | 4.7 GB |
| `vibleaker_android_samp` | 4.4 MB | none | 8 s | 1.8 GB |
| `remote_control_smack` | 2.9 MB | 1.0 MB (armeabi) | 31 s | 3.8 GB |

## The Dex ceiling

27 MB of Dex - six times TaintBench's largest - is comfortable: 165 s and 17 GB,
with the store at 470 MB.

35 MB of Dex is not. It climbed steadily for 35 minutes - 48 GB at 10 minutes,
79 GB at 20, 102 GB at 30 - and the guard killed it at 117 GiB. The trajectory
never plateaued, so it was not close to finishing; a bigger machine would have
bought time, not a result.

That is 1.3x the Dex for at least 7x the memory, which says the ceiling is not a
size threshold. Somewhere between these two apps the analysis goes from linear-ish
to something much steeper, and what decides it is the shape of the code, not its
weight. The same thing happened inside TaintBench: `xbot` at 1.9 MB of Dex costs
4.7 GB while `vibleaker` at 4.4 MB costs 1.8 GB.

So the usable rule is: **under ~25 MB of Dex, expect it to work; over ~35 MB,
expect to need either a model that cuts the app down or a different approach.**
The largest Dex found on F-Droid - `cash.p.terminal`, at 147.8 MB - is out of
reach by a wide margin.

## The native ceiling

Native is a different pipeline and a much lower ceiling.

Importing the same 27 MB app *with* its five arm64 libraries (39.8 MB total)
never finished. What happened, from the logs:

- three small libraries (`libdashjbls`, `libpthread`, `libscrypt`) disassembled
  and imported in about a minute, all told;
- `libsdklib.so`, at 37.7 MB, took **18 minutes in Ghidra** alone;
- lowering that library's pcode then ran for a further **71 minutes** without
  finishing, holding 32-46 GB, until the 90-minute timeout killed it;
- the fifth library was never reached, and `index` never ran.

Memory was not the problem - it peaked at 45.7 GiB, well inside the cap. Time
was. And the expensive half was not Ghidra but ctadl's own ingest of what Ghidra
produced, which ran four times longer than the disassembly.

Set against `remote_control_smack` - whose **1 MB** of armeabi code was enough to
need the unwinder modelled `skip-analysis` before the app would terminate at all
(see `hybrid-inlining-plateau.md`) - a practical native budget today is a few MB
per library, not tens. The single-library monsters on F-Droid are far outside it:
`libxul.so` at 156 MB (a Firefox-based browser), `libgodot_android.so` at 175 MB,
`libapplication.so` at 142 MB.

## Caveats

- One run per configuration. These are ceilings, not benchmarks; the numbers are
  big enough that a repeat would not move the conclusion.
- Only the `28f949ea` build was measured here. The earlier points would be
  slower, not faster.
- The 90-minute import timeout is mine, not a ctadl limit. A native import left
  alone might finish in two hours or ten; what the run shows is that it is not a
  minutes-scale operation at 38 MB.
- `--no-native-libs` is what separates the two ladders. Without it, importing any
  APK with sizeable native code pays the cost above before analysis starts.

## Raw data

```
/Volumes/Shampoo/ct-bigapk/
  apks/                  the six APKs downloaded
  runs/<app>.<mode>.<cfg>/{import,index}.{out,err}   per-phase output, incl. `time -l` and guard lines
  logs/runs.log          one summary block per run
  probe.py, shortlist.json  the range-request probe that sized candidates without downloading
  run-big.sh             the guarded runner
```
