// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
//! The static x86_64 build of `cig` takes `memcpy` and `memmove` from
//! `crates/cig-copy`: musl's are the string instruction, which costs tens
//! of cycles to start however little it copies (cig-script#243, step 0).
//!
//! The linker is told to wrap the two names, so every reference in the
//! program, in its allocator and in the C library goes to `__wrap_memcpy`
//! and `__wrap_memmove`. A build where those are missing does not link.
//! Only the `cig` binary is linked this way; every other target of the
//! package, and every other platform, keeps its C library's routines.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let cfg = |name: &str| std::env::var(name).unwrap_or_default();
    if cfg("CARGO_CFG_TARGET_ARCH") == "x86_64" && cfg("CARGO_CFG_TARGET_ENV") == "musl" {
        for name in ["memcpy", "memmove"] {
            println!("cargo:rustc-link-arg-bins=-Wl,--wrap={name}");
        }
    }
}
