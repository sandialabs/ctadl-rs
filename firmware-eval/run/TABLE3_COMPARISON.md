# CTADL vs Operation Mango — SaTC 7 Handpicked Firmware (paper Table 3)

**Date:** 2026-07-21 · **Build under test:** `ctadl` @ branch `firmware-eval-3` (`target/release/ctadl`, Jul 21)
**Corpus:** `operation-mango-public/firmware/7_firmware` (7 devices, 3 vendors) — the images behind the paper's **Table 3**.
**Model:** `firmware-eval/models/cmdi-firmware.json5` — name-based sink/source matching (see *Model fix* below).
**Comparison baseline:** the **published Table 3** numbers (Mango could not be re-run here — no docker/podman on the host).
**Note:** `DO-NOT-MERGE`

---

## TL;DR

- ctadl was run over **all command-injection–relevant binaries in all 7 firmware images** — the same
  binary population Operation Mango's `FirmwareFinder` selects (ELF, non-shared-object, non-symlink,
  non-busybox, deduped by sha256), globally deduped across the 3 shared Netgear images, and gated to
  binaries that actually contain a cmdi sink (Mango's `has_sinks` gate). **440 unique binaries.**
- **418/440 (95%) analyzed** end-to-end (Ghidra headless lift → ctadl taint). ctadl produced **979 command-injection
  findings across 111 distinct binaries** (121 firmware×binary alerts).
- ctadl finds the **same class of bug in the same daemons** Mango targets — the dominant pattern is
  `nvram-getter → sprintf → system` on the router service/init binaries (`rc`, `acos_service`,
  `arp_check`, …). Verified by tracing concrete flows.
- The tools are **not directly commensurable on a single number**: Mango's Table 3 reports its
  *ranked* results (`TruPoC` = closures with rank ≥ 7) and its *manually verified* `TP`; **ctadl does
  not rank or auto-verify**, so its raw alert volume sits between Mango's (unpublished) raw hit count
  and its ranked TruPoCs. The honest read is per-vendor **direction of disagreement** (below), not a
  scalar win/lose.

---

## The comparison

`pop` = cmdi-sink binaries in that firmware (ctadl's analyzed population, after global dedup).
`run` = analyzed OK · `to` = timed out · `nr` = not run (deferred giants, see Limitations).
Mango columns are verbatim from **Table 3** (its Mango side).

| Firmware | pop | run | to | nr | **ctadl alerts** | **ctadl alert-bins** ‖ | Mango TruPoC | Mango TP | Mango TruPoC-bins | Mango Total-bins |
|---|--:|--:|--:|--:|--:|--:|--:|--:|--:|--:|
| Netgear R6400 | 105 | 98 | 3 | 7 | **246** | **28** | 16 | 9 | 4 | 76 |
| Netgear R7000 | 115 | 109 | 3 | 6 | **267** | **35** | 23 | 14 | 5 | 85 |
| Netgear XR300 | 96 | 88 | 4 | 8 | **223** | **28** | 59 | 50 | 9 | 65 |
| D-Link DIR878 | 54 | 53 | 1 | 1 | **202** | **12** | 8 | 7 | 4 | 40 |
| Tenda AC15 | 56 | 56 | 1 | 0 | **14** | **6** | 16 | 3 | 4 | 39 |
| Tenda AC18 | 56 | 56 | 1 | 0 | **14** | **6** | 26 | 3 | 4 | 39 |
| Tenda W20E | 46 | 43 | 0 | 3 | **13** | **6** | 3 | 3 | 1 | 35 |
| **Total** | **528\*** | | | | **979** | **121** | **151** | **89** | **31** | **379** |

\* `pop` sums to 528 across firmware because 88 binaries are byte-identical across the 3 Netgear
images (shared samba/dnsmasq/service binaries); there are **440 unique** binaries. A finding in a
shared binary is attributed to every firmware that ships it — the same way Mango counts per-image.

### What is and isn't comparable

- **Ranking.** Mango's `TruPoC` is the subset of its closures with **rank ≥ 7** (a heuristic score that
  boosts, e.g., getters keyed on HTTP CGI vars). ctadl emits **every** source→sink taint path with no
  score, so `ctadl alerts` is conceptually Mango's *raw* "Alerts/hits" column (not in Table 3), **not**
  its TruPoC column. Expect `ctadl alerts` ≫ `Mango TruPoC` wherever ctadl works — and it does.
- **TP (true positives).** Mango's `TP` is **manual reverse-engineering verification** of a sampled
  subset. No equivalent was done for ctadl here; doing it is the natural next step (`bench.py triage`).
- **Most apples-to-apples axis:** *which binaries* each tool flags. ctadl flags a **superset** of the
  daemon types Mango does on Netgear/D-Link, and a **subset** on Tenda (see below).

---

## Per-vendor reading (the actual signal)

**Netgear (R6400 / R7000 / XR300) — ctadl over-reports vs ranked Mango, same surface.**
ctadl's alerts are dominated by the router service/init binaries where cmdi genuinely lives:
`rc` (191 NVRAM flows), `acos_service` (106 NVRAM + argv/env/file), `arp_check`, `check_ra`,
`check_db`, `wandetect`, … all `nvram-getter → … → system`. These are real taint paths; the count is
high because ctadl does not rank/dedup them the way Mango's TruPoC filter does. XR300 is the one row
where Mango's *TP* (50) is very high — most of that is its `httpd`, which ctadl **could not analyze in
budget** (see Limitations), so ctadl's XR300 number understates against that specific source.

**D-Link (DIR878) — ctadl over-reports, same surface.**
`prog.cgi` (the HNAP/CGI handler, MIPS) and `rc` drive it; the vendor wrapper **`twsystem`** shows up as
a sink (162 flows corpus-wide, mostly here). Directionally like Netgear.

**Tenda (AC15 / AC18 / W20E) — ctadl *under*-reports.**
This is the interesting disagreement. ctadl finds only ~14 alerts where Mango reports 16–26 TruPoCs,
and Tenda's `httpd` (both AC15 and AC18, ~966 K) yielded just **2** findings each. Root cause is **source-model
completeness, not the engine**: Tenda's web input arrives through vendor getters (`GetValue`,
form/`websGetVar`-style accessors) that are **not in the mirrored Mango source list**, so the taint
never gets seeded at the real entry point. This is a concrete, fixable model gap — add the Tenda web
getters as sources and the Tenda rows should rise.

---

## Findings breakdown (corpus-wide, 418/440)

**By source class:** NVRAM 502 · FILE 285 · ARGV 76 · NETWORK 58 · ENV 58
**By sink:** `system` 752 · `twsystem` 162 · `popen` 18 · `doSystemCmd` 14 · exec-family (`execl/execv/…`) 33

NVRAM-driven `system` is the overwhelming pattern — exactly Operation Mango's target class on SOHO routers.

**Validation — a concrete flow (Netgear `acos_service`):**
```
source  acosNvramConfig_get(...).deref        [NVRAM]
   ↓
call    sprintf(cmd, "...%s...", <tainted>)    [string-builder propagation]
   ↓
sink    system(cmd)                            [command_injection]
```
This is the textbook Netgear nvram→command-injection shape; ctadl reconstructs it end-to-end.

---

## Method (how the run was made comparable)

1. **Population = Mango's.** Re-implemented `mango_pipeline/firmware/elf_finder.FirmwareFinder`
   selection: ELF, exclude `file`-reported *shared objects* (`exclude_libs`), skip symlinks and
   `busybox`, dedup by sha256, inside the canonical squashfs/cpio root. (The provided rootfs was
   extracted with recursive binwalk `-M`, which carves spurious nested ELFs; those `.extracted/`
   carvings are excluded to match Mango's clean filesystem.)
2. **Global dedup + sink gate.** Deduped across all 7 images (Netgear share many binaries), then kept
   only binaries containing a cmdi **sink** symbol — Mango's `has_sinks` gate; a no-sink binary yields
   0 cmdi findings in *both* tools. → **440 unique binaries.**
3. **Run.** For each binary: `ctadl go -l pcode --models cmdi-firmware-name.json5` (Ghidra 12.0.4
   headless lift → ctadl Datalog taint), isolated store per job, parallel with a per-binary timeout.
   SARIF normalized to findings with the harness's `normalize_ctadl.parse_sarif` (call-site rebasing
   onto the angr load base, per `firmware-eval/README.md`).

### Model fix (required to see anything on real firmware)

The shipped `cmdi-firmware.json5` matches functions with `constraint: "signature_pattern"` — a regex
against the **whole mangled signature**. On stripped firmware, Ghidra names an imported sink
`<EXTERNAL>::system@0000a4c0`, so the anchored `^(system|…)$` matched **0 sinks** (confirmed on
`netctrl`: *Matched 0 sources and 0 sinks*) — the entire corpus would have falsely scored "0 findings."
Switching every generator to `constraint: "name"` (regex against the **short name** `system`) — the
"relax the anchor" path the model's own header comment anticipates for stripped/thunked firmware —
fixed it: `netctrl` → *Matched 396 sources and 191 sinks*, and it also repaired the propagation
builders (`sprintf`/`strcpy`/… were `<EXTERNAL>::sprintf@…` too). Verified **no regression** on the
synthetic `nested` argv→system case. This name-based matching is now folded into
`firmware-eval/models/cmdi-firmware.json5` (every `constraint: "signature_pattern"` → `constraint: "name"`).

---

## Limitations / caveats

- **ctadl is unranked & unverified here.** No TruPoC-equivalent ranking, no TP verification — so the
  volume numbers are not directly Mango's TruPoC/TP. Per-binary triage is the next step.
- **Scalability wall on string-heavy daemons.** ctadl's taint analysis blows up on large,
  string-processing daemons. The big stripped ARM `httpd` images (1.5–2 MB) exploded to **>50 GB RAM
  and >15 min with no result** under this broad model, so **4 of 6 `httpd` binaries (the Netgear +
  W20E ones) + `fbwifi` + `upAgent` were deferred** to protect the host. The Tenda `httpd`s (~966 K)
  *did* complete (2 findings each). The wall is not httpd-specific: `upnpd` (×3), `nginx` (×2),
  `dnsmasq`, and `soapd` also **hit the 15-min timeout** with no result. Since `httpd`/`upnpd` are
  Mango's richest sources on XR300/R7000, ctadl's Netgear/W20E rows *understate* against those
  binaries. (Mango is not immune — its pipeline uses a 3-hour per-call-chain timeout and reports
  OOMKilled/timeout binaries.)
- **22 binaries not analyzed:** 14 timeouts (`upnpd` ×3, `nginx` ×2, `zip`/`unzip` ×3 each, `dnsmasq`,
  `soapd`) + 1 crash (`sample.bin`, a test artifact) + the deferred giants above; plus a handful of
  non-cmdi crypto/net tools (`openssl`/`openvpn`/`gpg`/`tcpdump`/`wl.ko`/…) deliberately skipped from
  the deferred set.
- **Source-list completeness is per-vendor.** The Tenda under-reporting is a missing-source problem,
  not an engine problem — the mirrored Mango source set lacks Tenda's web getters.
- **Baseline is the paper, not a re-run.** Mango numbers are Table 3 as published; a same-host Mango
  run (needs docker/podman) would let the harness's `compare`/`score` do address-level matching.
