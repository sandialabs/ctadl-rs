# Head-to-head raw results - ctadl-souffle (old) vs ctadl-rs (new)

Both engines: built-in defaults suppressed, same shared model set, same Ghidra.

47 of 50 binaries completed all three phases under both engines.

## Corpus totals

| | old (souffle) | new (rs) | old / new |
|---|--:|--:|--:|
| import size, total | 3.0G | 543.9M | 5.71x |
| index size, total | 4.1G | 250.1M | 16.76x |
| function summaries, total | 11,063 | 146,952 | 0.08x |
| SARIF taint paths, total | 10 | 330 | 0.03x |
| wall time, total | 23.9 min | 21.1 min | 1.14x |
| binaries with >=1 path | 5 | 17 | |
| binaries where no sink bound | 0 | 2 | |

## Per-binary spread

Totals can be carried by one large binary. These are the same comparisons computed per binary and then quantiled, so a claim that holds here holds binary by binary.

| ratio | n | min | q1 | median | q3 | max |
|---|--:|--:|--:|--:|--:|--:|
| import size, old / new | 47 | 5.37x | 5.66x | **5.85x** | 6.05x | 6.98x |
| index size, old / new | 47 | 6.11x | 28.55x | **40.24x** | 55.81x | 258.32x |
| summaries, new / old | 47 | 0.15x | 2.22x | **2.64x** | 5.36x | 128.18x |

Paths, per binary: new reports more on **15**, old reports more on **0**, tied on 32 (30 of those tied at zero). Stated as wins rather than a ratio because a path count of zero is common and cannot be a denominator.

Endpoint pairs across the corpus: old 5, new 42, reported by both 5, **old only 0** (on 0 binaries), new only 37.

## Jobs that did not complete

Old engine failed on 3 of 50 binaries, new engine on 0. A binary is only in the comparison above if BOTH engines finished it, so these are excluded from every number - which means the totals understate any gap a failure represents.

| binary | old (souffle) | new (rs) |
|---|---|---|
| r6400_dbus_daemon | crash:index | ok |
| r7000_xagent_control | crash:index | ok |
| ac15_inadyn | crash:index | ok |

## Per binary

| binary | vendor / arch | size | engine | import | index | summaries | SARIF paths | wall |
|---|---|--:|---|--:|--:|--:|--:|--:|
| dir878_dlcfg_cgi | D-Link / MIPS | 9.1K | old (souffle) | 2.2M | 16.2M | 10 | 0 | 20s |
| dir878_dlcfg_cgi | D-Link / MIPS | 9.1K | new (rs) | 379.8K | 64.2K | 20 | 0 | 20s |
| dir878_ated | D-Link / MIPS | 18.5K | old (souffle) | 13.7M | 30.6M | 29 | 0 | 26s |
| dir878_ated | D-Link / MIPS | 18.5K | new (rs) | 2.3M | 566.4K | 733 | 0 | 26s |
| dir878_nvram_daemon | D-Link / MIPS | 23.3K | old (souffle) | 15.9M | 30.4M | 71 | 0 | 26s |
| dir878_nvram_daemon | D-Link / MIPS | 23.3K | new (rs) | 2.3M | 223.8K | 159 | 0 | 26s |
| dir878_fota_config | D-Link / MIPS | 28.4K | old (souffle) | 13.2M | 29.0M | 30 | 0 | 20s |
| dir878_fota_config | D-Link / MIPS | 28.4K | new (rs) | 2.1M | 648.9K | 80 | 0 | 20s |
| dir878_dxml | D-Link / MIPS | 93.0K | old (souffle) | 63.2M | 85.5M | 196 | 0 | 36s |
| dir878_dxml | D-Link / MIPS | 93.0K | new (rs) | 10.3M | 2.3M | 2905 | 0 | 31s |
| dir878_goahead | D-Link / MIPS | 144.1K | old (souffle) | 89.3M | 109.6M | 256 | 0 | 31s |
| dir878_goahead | D-Link / MIPS | 144.1K | new (rs) | 14.4M | 2.9M | 9817 | 0 | 31s |
| dir878_easyroaming | D-Link / MIPS | 258.6K | old (souffle) | 65.5M | 83.8M | 450 | 0 | 31s |
| dir878_easyroaming | D-Link / MIPS | 258.6K | new (rs) | 10.3M | 1.5M | 1483 | 0 | 31s |
| r6400_dlnad | Netgear / ARM | 9.7K | old (souffle) | 3.6M | 16.8M | 34 | 2 | 20s |
| r6400_dlnad | Netgear / ARM | 9.7K | new (rs) | 641.3K | 205.8K | 71 | 15 | 20s |
| r6400_check_dap | Netgear / ARM | 15.4K | old (souffle) | 9.8M | 24.7M | 78 | 0 | 20s |
| r6400_check_dap | Netgear / ARM | 15.4K | new (rs) | 1.7M | 507.3K | 354 | 4 | 20s |
| r6400_genie_cgi | Netgear / ARM | 16.2K | old (souffle) | 8.3M | 23.1M | 69 | 0 | 20s |
| r6400_genie_cgi | Netgear / ARM | 16.2K | new (rs) | 1.4M | 399.4K | 133 | 0 | 20s |
| r6400_bd | Netgear / ARM | 22.3K | old (souffle) | 15.2M | 32.4M | 52 | 2 | 20s |
| r6400_bd | Netgear / ARM | 22.3K | new (rs) | 2.5M | 630.6K | 117 | 2 | 20s |
| r6400_bftpd | Netgear / ARM | 51.2K | old (souffle) | 44.9M | 64.4M | 146 | 0 | 26s |
| r6400_bftpd | Netgear / ARM | 51.2K | new (rs) | 7.5M | 2.2M | 1907 | 2 | 26s |
| r6400_readycloud_control_cgi | Netgear / ARM | 95.1K | old (souffle) | 295.3M | 360.8M | 348 | 0 | 77s |
| r6400_readycloud_control_cgi | Netgear / ARM | 95.1K | new (rs) | 54.4M | 58.9M | 452 | 0 | 51s |
| r6400_rc | Netgear / ARM | 106.1K | old (souffle) | 174.0M | 201.4M | 150 | 0 | 41s |
| r6400_rc | Netgear / ARM | 106.1K | new (rs) | 30.8M | 13.2M | 290 | 29 | 26s |
| r6400_acos_service | Netgear / ARM | 138.5K | old (souffle) | 126.1M | 154.4M | 128 | 2 | 36s |
| r6400_acos_service | Netgear / ARM | 138.5K | new (rs) | 22.7M | 6.1M | 310 | 72 | 26s |
| r6400_dbus_daemon | Netgear / ARM | 303.1K | old (souffle) | 161.3M | - | None | 0 | 41s |
| r6400_dbus_daemon | Netgear / ARM | 303.1K | new (rs) | 28.0M | 5.8M | 6039 | 0 | 46s |
| r7000_usbheartbeat | Netgear / ARM | 9.7K | old (souffle) | 5.9M | 21.3M | 20 | 0 | 20s |
| r7000_usbheartbeat | Netgear / ARM | 9.7K | new (rs) | 1.1M | 542.4K | 56 | 2 | 20s |
| r7000_ftpc | Netgear / ARM | 13.7K | old (souffle) | 4.7M | 18.5M | 92 | 0 | 20s |
| r7000_ftpc | Netgear / ARM | 13.7K | new (rs) | 850.9K | 175.1K | 149 | 0 | 20s |
| r7000_genie_cgi | Netgear / ARM | 15.5K | old (souffle) | 8.1M | 22.8M | 63 | 0 | 20s |
| r7000_genie_cgi | Netgear / ARM | 15.5K | new (rs) | 1.3M | 390.3K | 112 | 0 | 20s |
| r7000_hotplug2 | Netgear / ARM | 17.7K | old (souffle) | 10.4M | 25.6M | 66 | 0 | 20s |
| r7000_hotplug2 | Netgear / ARM | 17.7K | new (rs) | 1.8M | 602.4K | 167 | 0 | 20s |
| r7000_arp_check | Netgear / ARM | 18.3K | old (souffle) | 29.6M | 46.6M | 76 | 0 | 20s |
| r7000_arp_check | Netgear / ARM | 18.3K | new (rs) | 4.8M | 2.0M | 198 | 36 | 20s |
| r7000_bd | Netgear / ARM | 22.6K | old (souffle) | 15.2M | 32.4M | 52 | 2 | 20s |
| r7000_bd | Netgear / ARM | 22.6K | new (rs) | 2.5M | 624.6K | 117 | 2 | 20s |
| r7000_checkDullWan | Netgear / ARM | 24.5K | old (souffle) | 14.1M | 32.0M | 41 | 0 | 20s |
| r7000_checkDullWan | Netgear / ARM | 24.5K | new (rs) | 2.5M | 646.1K | 172 | 4 | 20s |
| r7000_bftpd | Netgear / ARM | 51.1K | old (souffle) | 44.8M | 64.2M | 146 | 0 | 26s |
| r7000_bftpd | Netgear / ARM | 51.1K | new (rs) | 7.5M | 2.2M | 1900 | 2 | 26s |
| r7000_circled | Netgear / ARM | 56.8K | old (souffle) | 27.8M | 46.2M | 3199 | 0 | 26s |
| r7000_circled | Netgear / ARM | 56.8K | new (rs) | 4.9M | 983.2K | 476 | 23 | 26s |
| r7000_xagent_control | Netgear / ARM | 80.4K | old (souffle) | 43.2M | - | None | 0 | 21s |
| r7000_xagent_control | Netgear / ARM | 80.4K | new (rs) | 7.6M | 1.6M | 1130 | 2 | 26s |
| r7000_readycloud_control_cgi | Netgear / ARM | 96.5K | old (souffle) | 296.5M | 361.8M | 345 | 0 | 72s |
| r7000_readycloud_control_cgi | Netgear / ARM | 96.5K | new (rs) | 54.4M | 59.2M | 451 | 0 | 51s |
| r7000_rc | Netgear / ARM | 112.2K | old (souffle) | 193.5M | 222.1M | 157 | 0 | 46s |
| r7000_rc | Netgear / ARM | 112.2K | new (rs) | 34.0M | 15.1M | 298 | 31 | 31s |
| xr300_ftpc | Netgear / ARM | 13.7K | old (souffle) | 4.7M | 18.5M | 92 | 0 | 20s |
| xr300_ftpc | Netgear / ARM | 13.7K | new (rs) | 853.8K | 175.7K | 243 | 0 | 20s |
| xr300_genie_cgi | Netgear / ARM | 14.4K | old (souffle) | 8.1M | 22.8M | 63 | 0 | 20s |
| xr300_genie_cgi | Netgear / ARM | 14.4K | new (rs) | 1.3M | 389.4K | 112 | 0 | 20s |
| xr300_arp_check | Netgear / ARM | 18.5K | old (souffle) | 29.7M | 46.7M | 76 | 0 | 20s |
| xr300_arp_check | Netgear / ARM | 18.5K | new (rs) | 4.8M | 2.0M | 198 | 36 | 20s |
| xr300_dap_daemon | Netgear / ARM | 30.2K | old (souffle) | 15.7M | 32.9M | 98 | 0 | 20s |
| xr300_dap_daemon | Netgear / ARM | 30.2K | new (rs) | 2.7M | 866.2K | 310 | 0 | 20s |
| xr300_funjsq_conntime | Netgear / ARM | 93.7K | old (souffle) | 59.8M | 80.1M | 154 | 0 | 31s |
| xr300_funjsq_conntime | Netgear / ARM | 93.7K | new (rs) | 10.1M | 1.8M | 614 | 0 | 26s |
| xr300_acos_service | Netgear / ARM | 135.5K | old (souffle) | 121.3M | 149.0M | 117 | 2 | 36s |
| xr300_acos_service | Netgear / ARM | 135.5K | new (rs) | 21.8M | 5.9M | 297 | 64 | 26s |
| xr300_funjsq_httpd | Netgear / ARM | 162.4K | old (souffle) | 76.5M | 100.7M | 363 | 0 | 36s |
| xr300_funjsq_httpd | Netgear / ARM | 162.4K | new (rs) | 13.4M | 3.3M | 1337 | 0 | 31s |
| xr300_funjsq_detect | Netgear / ARM | 286.1K | old (souffle) | 185.4M | 223.1M | 419 | 0 | 51s |
| xr300_funjsq_detect | Netgear / ARM | 286.1K | new (rs) | 32.8M | 13.1M | 53706 | 0 | 46s |
| xr300_dap_logd | Netgear / ARM | 419.6K | old (souffle) | 191.3M | 236.0M | 700 | 0 | 57s |
| xr300_dap_logd | Netgear / ARM | 419.6K | new (rs) | 34.8M | 8.9M | 1541 | 0 | 41s |
| ac15_logserver | Tenda / ARM | 9.6K | old (souffle) | 2.8M | 16.4M | 39 | 0 | 20s |
| ac15_logserver | Tenda / ARM | 9.6K | new (rs) | 494.0K | 114.0K | 191 | 0 | 20s |
| ac15_inadyn | Tenda / ARM | 26.9K | old (souffle) | 12.7M | - | None | 0 | 15s |
| ac15_inadyn | Tenda / ARM | 26.9K | new (rs) | 2.2M | 795.6K | 168 | 0 | 20s |
| ac15_acsd | Tenda / ARM | 38.4K | old (souffle) | 16.6M | 34.1M | 141 | 0 | 20s |
| ac15_acsd | Tenda / ARM | 38.4K | new (rs) | 2.9M | 662.4K | 530 | 0 | 20s |
| ac15_app_data_center | Tenda / ARM | 82.0K | old (souffle) | 42.7M | 63.8M | 439 | 0 | 26s |
| ac15_app_data_center | Tenda / ARM | 82.0K | new (rs) | 7.6M | 2.3M | 4766 | 0 | 26s |
| ac15_business_proc | Tenda / ARM | 172.0K | old (souffle) | 86.2M | 108.1M | 234 | 0 | 36s |
| ac15_business_proc | Tenda / ARM | 172.0K | new (rs) | 14.6M | 2.8M | 537 | 0 | 31s |
| ac15_netctrl | Tenda / ARM | 310.6K | old (souffle) | 181.9M | 216.9M | 409 | 0 | 52s |
| ac15_netctrl | Tenda / ARM | 310.6K | new (rs) | 31.0M | 7.0M | 3790 | 2 | 36s |
| ac18_business_proc | Tenda / ARM | 172.0K | old (souffle) | 86.2M | 107.9M | 234 | 0 | 36s |
| ac18_business_proc | Tenda / ARM | 172.0K | new (rs) | 14.6M | 2.8M | 537 | 0 | 31s |
| ac18_dhttpd | Tenda / ARM | 208.0K | old (souffle) | 89.4M | 117.0M | 523 | 0 | 36s |
| ac18_dhttpd | Tenda / ARM | 208.0K | new (rs) | 15.8M | 3.1M | 13151 | 0 | 31s |
| w20e_logserver | Tenda / ARM | 13.8K | old (souffle) | 3.8M | 17.2M | 44 | 0 | 20s |
| w20e_logserver | Tenda / ARM | 13.8K | new (rs) | 678.7K | 160.5K | 256 | 0 | 20s |
| w20e_ucloud_ctl | Tenda / ARM | 18.3K | old (souffle) | 3.7M | 16.8M | 45 | 0 | 20s |
| w20e_ucloud_ctl | Tenda / ARM | 18.3K | new (rs) | 610.2K | 110.0K | 102 | 0 | 20s |
| w20e_portal | Tenda / ARM | 50.4K | old (souffle) | 22.6M | 39.6M | 83 | 0 | 26s |
| w20e_portal | Tenda / ARM | 50.4K | new (rs) | 3.9M | 827.6K | 268 | 0 | 26s |
| w20e_dhcpcd | Tenda / ARM | 84.2K | old (souffle) | 74.8M | 96.1M | 107 | 0 | 31s |
| w20e_dhcpcd | Tenda / ARM | 84.2K | new (rs) | 12.8M | 3.2M | 3903 | 0 | 26s |
| w20e_dhcps | Tenda / ARM | 170.9K | old (souffle) | 210.6M | 291.8M | 379 | 0 | 56s |
| w20e_dhcps | Tenda / ARM | 170.9K | new (rs) | 37.2M | 18.0M | 37636 | 4 | 51s |

## Do the two engines find the same paths?

Paths are compared as `source -> sink` endpoint pairs, the coarsest join that is meaningful across engines (they do not agree on how to name an intermediate vertex). `both` counts pairs reported by both engines. Binaries on which neither engine reported a path are omitted - the row would be all zeros; their count is in the summary above.

| binary | pairs old | pairs new | both | old only | new only |
|---|--:|--:|--:|--:|--:|
| r6400_dlnad | 1 | 2 | 1 | 0 | 1 |
| r6400_check_dap | 0 | 2 | 0 | 0 | 2 |
| r6400_bd | 1 | 2 | 1 | 0 | 1 |
| r6400_bftpd | 0 | 2 | 0 | 0 | 2 |
| r6400_rc | 0 | 4 | 0 | 0 | 4 |
| r6400_acos_service | 1 | 4 | 1 | 0 | 3 |
| r7000_usbheartbeat | 0 | 1 | 0 | 0 | 1 |
| r7000_arp_check | 0 | 3 | 0 | 0 | 3 |
| r7000_bd | 1 | 2 | 1 | 0 | 1 |
| r7000_checkDullWan | 0 | 2 | 0 | 0 | 2 |
| r7000_bftpd | 0 | 2 | 0 | 0 | 2 |
| r7000_circled | 0 | 2 | 0 | 0 | 2 |
| r7000_rc | 0 | 4 | 0 | 0 | 4 |
| xr300_arp_check | 0 | 3 | 0 | 0 | 3 |
| xr300_acos_service | 1 | 4 | 1 | 0 | 3 |
| ac15_netctrl | 0 | 1 | 0 | 0 | 1 |
| w20e_dhcps | 0 | 2 | 0 | 0 | 2 |

## Endpoints of the reported paths

Only engine/binary pairs that reported at least one path.

| binary | engine | sinks reached | sources | taint labels |
|---|---|---|---|---|
| r6400_dlnad | old | system | fgets | file_input |
| r6400_dlnad | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r6400_check_dap | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r6400_bd | old | system | fgets | file_input |
| r6400_bd | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r6400_bftpd | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r6400_rc | new | system | acosNvramConfig_get, fgets, getenv, nvram_get | env_input, file_input, nvram_input |
| r6400_acos_service | old | system | fgets | file_input |
| r6400_acos_service | new | system | acosNvramConfig_get, fgets, getenv, main | argv_input, env_input, file_input, nvram_input |
| r7000_usbheartbeat | new | system | acosNvramConfig_get | nvram_input |
| r7000_arp_check | new | system | acosNvramConfig_get, fgets, recv | file_input, network_input, nvram_input |
| r7000_bd | old | system | fgets | file_input |
| r7000_bd | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r7000_checkDullWan | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r7000_bftpd | new | system | acosNvramConfig_get, fgets | file_input, nvram_input |
| r7000_circled | new | popen, system | fgets | file_input |
| r7000_xagent_control | new | execlp | nvram_get | nvram_input |
| r7000_rc | new | system | acosNvramConfig_get, fgets, getenv, nvram_get | env_input, file_input, nvram_input |
| xr300_arp_check | new | system | acosNvramConfig_get, fgets, recv | file_input, network_input, nvram_input |
| xr300_acos_service | old | system | fgets | file_input |
| xr300_acos_service | new | system | acosNvramConfig_get, fgets, getenv, main | argv_input, env_input, file_input, nvram_input |
| ac15_netctrl | new | doSystemCmd | fgets | file_input |
| w20e_dhcps | new | execl, popen | read | file_input |

## Configuration control (Operation Mango synthetic binaries)

Confirms neither engine was handed a model set it cannot use.

| binary | old (souffle) paths | new (rs) paths |
|---|--:|--:|
| nested | 3 | 2 |
| simple | 6 | 4 |
| heap | 0 | 2 |
| wrapper | 0 | 2 |
| off_shoot | 0 | 3 |
