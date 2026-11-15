// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// Mirrors bench/compute/sieve.cig: sieve of Eratosthenes, index writes into a big list.
// The list is built and counted in loops, as the cig program does; idiom/sieve.rs
// uses vec![true; n + 1] and an iterator count.
// black_box: the size is not a constant the compiler can fold the work on.
use std::hint::black_box;

fn main() {
    let n: usize = black_box(300000);
    let mut flags: Vec<bool> = Vec::new();
    for _ in 0..n + 1 {
        flags.push(true);
    }
    flags[0] = false;
    flags[1] = false;
    let mut p = 2;
    while p * p <= n {
        if flags[p] {
            let mut m = p * p;
            while m <= n {
                flags[m] = false;
                m += p;
            }
        }
        p += 1;
    }
    let mut c = 0;
    for b in &flags {
        if *b {
            c += 1;
        }
    }
    println!("{}", c);
}
