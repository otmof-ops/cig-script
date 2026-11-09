<!--
SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
SPDX-License-Identifier: CC-BY-4.0
-->

# cig-pass fuzz targets

cargo-fuzz layout, not a workspace member. Lives at
`crates/cig-pass/fuzz/` (copy `Cargo.toml`, `fuzz_targets/`, `corpus/`).

| target | what it holds |
|---|---|
| `decode` | arbitrary bytes never panic `decode`; whatever decodes re-encodes to the same bytes (one encoding per pass); its token round-trips; arbitrary text never panics `from_token` |
| `verify` | decode then `verify` against a fixed context never panics; nothing the fuzzer made up verifies (only the one real pass under the fixed key may) |
| `attenuate` | structured caveats appended one after another: never panics, earlier caveats stay in place and in order, the result round-trips, and a narrowing never makes an invalid pass valid |

## Run

```
rustup toolchain install nightly      # once
cargo install cargo-fuzz              # once
cd crates/cig-pass
cargo +nightly fuzz run decode -- -max_total_time=60
cargo +nightly fuzz run verify -- -max_total_time=60
cargo +nightly fuzz run attenuate -- -max_total_time=60
```

CI runs each target for 60 seconds on a pull request and replays the whole
corpus (`-runs=0`) so a regression found once stays found.

A crash is saved to `fuzz/artifacts/<target>/`. Commit the minimized input
(`cargo fuzz tmin`) to `fuzz/corpus/<target>/` with the fix.

## Corpus: `corpus/decode/`

The `seed-NN.bin` files are real passes (local, narrowed with each caveat
kind, shareable sealed and unsealed); the `00-*.bin` ones are malformed on
purpose. Worth adding as the format grows, named `NN-<what>.bin`:

1. `01-local-bare.bin`: a local `check` pass, no inputs, no caveats.
2. `02-local-file-input.bin`: one `File` input.
3. `03-local-all-inputs.bin`: one of each input tag 1 to 9, including a
   `Pass` input and a `Clock`.
4. `04-local-all-caveats.bin`: one of each caveat tag 1 to 11 (a `network`
   with hosts, an `args` glob, `needs human-yes`).
5. `05-local-mono.bin`: with `has_mono = 1`.
6. `06-shareable-unsealed.bin`: a shareable pass with two blocks and the
   unsealed proof.
7. `07-shareable-sealed.bin`: the same, sealed.
8. `08-max-caveats.bin`: 64 caveats.
9. `09-big-result.bin`: a result of exactly 64 KiB.
10. `10-every-kind-<kind>.bin`: one bare pass per kind (12 files).
11. `11-inputs-unicode-path.bin`: a non-ASCII path and a long env name.
12. `12-windows-style.bin`: dev, ino, ctime and mode all zero.

The `verify` and `attenuate` targets start from the same files when they
are copied into `corpus/verify/` and `corpus/attenuate/`.

Negative seeds worth keeping (the decoder must refuse each, never panic):
every truncation of seed 4, seed 4 with a trailing byte, unknown version
(2), form byte 2, kind `"Check"`, an unsorted input pair, a duplicate
input pair, a relative path, a length field of `0xFFFFFFFF`, and a result
length of 64 KiB + 1.
