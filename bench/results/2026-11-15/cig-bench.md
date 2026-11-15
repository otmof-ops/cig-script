**cig bench**: cig, cig 1.24.0 — Intel(R) Core(TM) i5-8265U CPU @ 1.60GHz, 8 cores, linux 7.2.11+parrot7.1-amd64, governor powersave

| case | group | median ms | p10 to p90 ms | CV | RSS MB |
|---|---|---:|---|---:|---:|
| `hello` | start-up | 2.3 | 2.3 to 2.4 | 0.5% | 11.1 |
| `startup-0` | start-up | 2.8~ | 2.3 to 5.6 | 35.5% | 11.1 |
| `startup-1` | start-up | 5.6 | 5.6 to 5.6 | 0.2% | 11.1 |
| `startup-10` | start-up | 5.8 | 5.8 to 5.8 | 0.3% | 11.1 |
| `burn one write` | start-up | 6.3 | 6.2 to 6.4 | 1.0% | 11.1 |
| `closures` | compute | 46.6 | 46.5 to 47.0 | 0.4% | 11.1 |
| `csv` | compute | 45.5 | 45.2 to 45.9 | 0.5% | 33.5 |
| `deltablue` | compute | 7.1 | 7.0 to 7.3 | 1.3% | 11.1 |
| `dispatch` | compute | 71.8 | 70.3 to 72.0 | 0.9% | 11.1 |
| `fannkuch` | compute | 298.6 | 296.6 to 299.9 | 0.4% | 11.1 |
| `fib` | compute | 56.3 | 55.7 to 57.4 | 1.2% | 11.1 |
| `json-deep` | compute | 4.6 | 4.6 to 4.7 | 0.9% | 11.1 |
| `json` | compute | 73.5 | 71.6 to 73.8 | 1.3% | 65.9 |
| `lists` | compute | 29.4 | 29.3 to 29.5 | 0.2% | 18.1 |
| `loop` | compute | 63.6 | 63.5 to 63.6 | 0.0% | 11.1 |
| `mandel` | compute | 404.1 | 392.3 to 404.2 | 1.1% | 11.1 |
| `maps` | compute | 40.7 | 40.4 to 41.7 | 1.3% | 21.4 |
| `nbody` | compute | 513.7 | 508.4 to 560.1 | 3.7% | 11.1 |
| `regex-ish` | compute | 40.3 | 40.2 to 40.5 | 0.2% | 29.7 |
| `richards` | compute | 21.9 | 21.7 to 22.0 | 0.4% | 11.1 |
| `sieve` | compute | 173.2 | 169.9 to 174.1 | 1.1% | 43.3 |
| `spectral` | compute | 224.7 | 220.1 to 229.1 | 1.4% | 11.1 |
| `strbuild` | compute | 6.6 | 6.6 to 6.7 | 0.5% | 11.1 |
| `strings` | compute | 23.1 | 23.0 to 23.2 | 0.3% | 22.9 |
| `trees` | compute | 414.8 | 412.3 to 419.5 | 0.6% | 11.1 |
| `words` | compute | 39.2 | 38.5 to 39.5 | 0.9% | 23.1 |
| `burn small tree` | kernel | 7.6 | 7.6 to 7.8 | 1.1% | 11.1 |
| `burn big tree` | kernel | 168.3 | 166.7 to 173.8 | 1.6% | 23.0 |
| `unburn small tree` | kernel | 10.5 | 10.4 to 10.7 | 0.8% | 13.3 |
| `unburn big tree` | kernel | 260.7~ | 253.5 to 294.8 | 5.7% | 22.1 |
| `overwrite big file` | kernel | 580.7 | 578.3 to 581.3 | 0.2% | 125.6 |
| `restore big file` | kernel | 697.1 | 693.9 to 735.5 | 2.3% | 53.8 |
| `hop, 2,000 files unchanged` | kernel | 97.3 | 91.8 to 100.0 | 2.9% | 47.0 |

| case | measure | value |
|---|---|---:|
| `burn small tree` | `tin-growth` | 0 B |
| `burn small tree` | `disk-per-byte` | 4.80x |
| `burn small tree` | `fsyncs-per-op` | 1.10 |
| `burn big tree` | `tin-growth` | 0 B |
| `burn big tree` | `disk-per-byte` | 1.04x |
| `burn big tree` | `fsyncs-per-op` | 1.01 |
| `unburn small tree` | `tin-growth` | 0 B |
| `unburn small tree` | `fsyncs-per-op` | 0.09 |
| `unburn big tree` | `tin-growth` | 0 B |
| `unburn big tree` | `fsyncs-per-op` | 0.01 |
| `overwrite big file` | `tin-growth` | 16.0 KiB |
| `overwrite big file` | `disk-per-byte` | 0.00x |
| `overwrite big file` | `fsyncs-per-op` | 15.00 |
| `overwrite big file` | `file:$CIGSCRIPT_HOME/work/big.bin` | 32.0 MiB |
| `restore big file` | `tin-growth` | 0 B |
| `restore big file` | `fsyncs-per-op` | 138.00 |
| `restore big file` | `file:$CIGSCRIPT_HOME/work/big.bin` | 32.0 MiB |
| `hop, 2,000 files unchanged` | `tin-growth` | 0 B |
| `hop, 2,000 files unchanged` | `fsyncs-per-op` | 12.00 |
