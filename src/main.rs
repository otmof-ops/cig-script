// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

// musl's allocator is slow under an interpreter's allocation pattern; musl
// builds (the released Linux binaries) use mimalloc instead (#146).
#[cfg(all(target_env = "musl", not(feature = "bench-stats")))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

// `--features bench-stats` wraps whichever allocator the platform uses in one
// that counts, for `cig bench --stats` (#179). Off in releases.
#[cfg(all(target_env = "musl", feature = "bench-stats"))]
#[global_allocator]
static ALLOCATOR: cigscript::benchstats::Counting<mimalloc::MiMalloc> =
    cigscript::benchstats::Counting(mimalloc::MiMalloc);
#[cfg(all(not(target_env = "musl"), feature = "bench-stats"))]
#[global_allocator]
static ALLOCATOR: cigscript::benchstats::Counting<std::alloc::System> =
    cigscript::benchstats::Counting(std::alloc::System);

fn main() {
    std::process::exit(cigscript::cli::entry());
}
