# Head-to-head raw results - ctadl-souffle (old) vs ctadl-rs (new)

Both engines: built-in defaults suppressed, same shared model set, same Ghidra.

| binary | vendor / arch | size | engine | import | index | summaries | SARIF paths | wall |
|---|---|--:|---|--:|--:|--:|--:|--:|
| r7000_arp_check | Netgear / ARM | 18.3K | old (souffle) | 29.6M | 46.6M | 76 | 0 | 20s |
| r7000_arp_check | Netgear / ARM | 18.3K | new (rs) | 4.8M | 2.0M | 198 | 36 | 20s |
| dlink878_nvram_daemon | D-Link / MIPS | 23.3K | old (souffle) | 15.7M | 30.3M | 71 | 0 | 26s |
| dlink878_nvram_daemon | D-Link / MIPS | 23.3K | new (rs) | 2.3M | 223.9K | 159 | 0 | 26s |
| r7000_rc | Netgear / ARM | 112.2K | old (souffle) | 193.5M | 222.1M | 157 | 0 | 46s |
| r7000_rc | Netgear / ARM | 112.2K | new (rs) | 34.0M | 15.1M | 295 | 31 | 31s |
| r6400_acos_service | Netgear / ARM | 138.5K | old (souffle) | 126.1M | 154.4M | 128 | 2 | 36s |
| r6400_acos_service | Netgear / ARM | 138.5K | new (rs) | 22.7M | 6.1M | 310 | 72 | 26s |
| ac15_netctrl | Tenda / ARM | 310.6K | old (souffle) | 181.9M | 216.9M | 409 | 0 | 46s |
| ac15_netctrl | Tenda / ARM | 310.6K | new (rs) | 31.0M | 7.0M | 3790 | 2 | 36s |

## Do the two engines find the same paths?

Paths are compared as `source -> sink` endpoint pairs, the coarsest join that is meaningful across engines (they do not agree on how to name an intermediate vertex). `both` counts pairs reported by both engines.

| binary | pairs old | pairs new | both | old only | new only |
|---|--:|--:|--:|--:|--:|
| r7000_arp_check | 0 | 3 | 0 | 0 | 3 |
| dlink878_nvram_daemon | 0 | 0 | 0 | 0 | 0 |
| r7000_rc | 0 | 4 | 0 | 0 | 4 |
| r6400_acos_service | 1 | 4 | 1 | 0 | 3 |
| ac15_netctrl | 0 | 1 | 0 | 0 | 1 |

## Endpoints of the reported paths

| binary | engine | sinks reached | sources | taint labels |
|---|---|---|---|---|
| r7000_arp_check | old | - | - | - |
| r7000_arp_check | new | system | acosNvramConfig_get, fgets, recv | file_input, network_input, nvram_input |
| dlink878_nvram_daemon | old | - | - | - |
| dlink878_nvram_daemon | new | - | - | - |
| r7000_rc | old | - | - | - |
| r7000_rc | new | system | acosNvramConfig_get, fgets, getenv, nvram_get | env_input, file_input, nvram_input |
| r6400_acos_service | old | system | fgets | file_input |
| r6400_acos_service | new | system | acosNvramConfig_get, fgets, getenv, main | argv_input, env_input, file_input, nvram_input |
| ac15_netctrl | old | - | - | - |
| ac15_netctrl | new | doSystemCmd | fgets | file_input |

## Configuration control (Operation Mango synthetic binaries)

Confirms neither engine was handed a model set it cannot use.

| binary | old (souffle) paths | new (rs) paths |
|---|--:|--:|
| nested | 3 | 2 |
| simple | 6 | 4 |
| heap | 0 | 2 |
| wrapper | 0 | 2 |
| off_shoot | 0 | 3 |
