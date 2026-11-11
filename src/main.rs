// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

// musl's allocator is slow under an interpreter's allocation pattern; musl
// builds (the released Linux binaries) use mimalloc instead (#146).
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    std::process::exit(cigscript::cli::entry());
}
