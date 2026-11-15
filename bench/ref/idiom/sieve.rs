// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2
// The sieve of bench/compute/sieve.cig as Rust is usually written: the list made at
// its full length and counted by an iterator. ../sieve.rs is the
// statement-for-statement version.
// black_box: the size is not a constant the compiler can fold the work on.
use std::hint::black_box;

fn main() {
    let n: usize = black_box(300000);
    let mut flags = vec![true; n + 1];
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
    println!("{}", flags.iter().filter(|b| **b).count());
}
